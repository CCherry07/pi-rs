use async_trait::async_trait;
use pi_core::{PluginId, ToolCallId, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    execution, optional_result_limit, resolve_to_cwd, spec, truncate_lines_by_bytes,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 500;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;

pub struct LsPlugin;
pub struct LsTool;
#[pi_plugin::plugin]
impl Plugin for LsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("ls-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        c.register_tool(Arc::new(LsTool))
    }
}
#[async_trait]
impl Tool for LsTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "ls",
                "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).",
                json!({
                    "type":"object","properties":{"path":{"type":"string","description":"Directory to list (default: current directory)"},"limit":{"type":"number","description":"Maximum number of entries to return (default: 500)"}}
                }),
            ),
            "List directory contents",
            std::iter::empty::<&str>(),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let path = resolve_to_cwd(
            context.cwd(),
            input.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let limit = optional_result_limit(&input, "limit", DEFAULT_LIMIT)?;
        // Pi probes existence first and turns every failed probe (including an
        // inaccessible parent) into the same path-not-found result.
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| execution(format!("Path not found: {}", path.display())))?;
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        if !metadata.is_dir() {
            return Err(execution(format!("Not a directory: {}", path.display())));
        }
        let mut reader = tokio::fs::read_dir(&path)
            .await
            .map_err(|error| execution(format!("Cannot read directory: {error}")))?;
        let mut entries = Vec::new();
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|error| execution(format!("Cannot read directory: {error}")))?
        {
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            entries.push((entry.file_name(), entry.path()));
        }
        // Pi lowercases names before localeCompare. Use the oracle's default
        // English ICU collation rather than approximating accents/punctuation.
        let collator = icu_collator::Collator::try_new(
            icu_locale::locale!("en-US").into(),
            Default::default(),
        )
        .map_err(|error| execution(format!("Could not initialize filename collation: {error}")))?;
        entries.sort_by(|(left, _), (right, _)| {
            collator.compare(
                &left.to_string_lossy().to_lowercase(),
                &right.to_string_lossy().to_lowercase(),
            )
        });

        let mut results = Vec::new();
        let mut entry_limit_reached = false;
        for (name, entry_path) in entries {
            if results.len() >= limit {
                entry_limit_reached = true;
                break;
            }
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            let Ok(metadata) = tokio::fs::metadata(entry_path).await else {
                continue;
            };
            let mut rendered = name.to_string_lossy().into_owned();
            if metadata.is_dir() {
                rendered.push('/');
            }
            results.push(rendered);
        }

        if results.is_empty() {
            return Ok(ToolResult::text("(empty directory)"));
        }

        let (mut text, truncation) = truncate_lines_by_bytes(&results, MAX_OUTPUT_BYTES);
        let mut notices = Vec::new();
        let mut details = Map::new();
        if entry_limit_reached {
            notices.push(format!(
                "{limit} entries limit reached. Use limit={} for more",
                limit.saturating_mul(2)
            ));
            details.insert("entryLimitReached".into(), json!(limit));
        }
        if let Some(truncation) = truncation {
            notices.push("50.0KB limit reached".to_string());
            details.insert("truncation".into(), truncation);
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }
        let mut result = ToolResult::text(text);
        result.details = (!details.is_empty()).then_some(Value::Object(details));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    async fn run_ls(cwd: &std::path::Path, input: Value) -> Result<ToolResult, ToolError> {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        LsTool
            .execute(
                ToolContext::standalone(cwd.to_path_buf(), signal),
                ToolCallId::new("test"),
                input,
                updates,
            )
            .await
    }

    fn result_text(result: &ToolResult) -> &str {
        match &result.content[0] {
            pi_core::ContentBlock::Text(value) => &value.text,
            _ => panic!("expected text result"),
        }
    }

    #[tokio::test]
    async fn sorts_and_marks_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b"), "").unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = LsTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert_eq!(text, "a/\nb");
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn floors_numeric_entry_limits_like_pi() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("first"), "").unwrap();
        std::fs::write(directory.path().join("second"), "").unwrap();
        for limit in [-1.0, 0.0, 1.9] {
            let result = run_ls(directory.path(), json!({"limit":limit}))
                .await
                .unwrap();
            assert_eq!(result.details.as_ref().unwrap()["entryLimitReached"], 1);
            assert!(result_text(&result).starts_with("first\n"));
            assert!(!result_text(&result).contains("second"));
        }
    }

    #[tokio::test]
    async fn sorts_case_insensitively_and_includes_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("z"), "").unwrap();
        std::fs::write(dir.path().join("B"), "").unwrap();
        std::fs::write(dir.path().join("a"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();

        let result = run_ls(dir.path(), json!({})).await.unwrap();

        assert_eq!(result_text(&result), ".hidden\na\nB\nz");
    }

    #[tokio::test]
    async fn sorts_latin_accents_and_punctuation_like_the_pi_locale_oracle() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "z",
            "B",
            "a",
            ".hidden",
            "éclair",
            "eclair",
            "École",
            "eagle",
            "apple",
            "a-b",
            "a_b",
            "ab",
            "a.b",
            "file10",
            "file2",
            "über",
            "ångström",
            "äther",
            "_a",
            "-a",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let result = run_ls(dir.path(), json!({})).await.unwrap();
        // Current Node's en-US a.toLowerCase().localeCompare(b.toLowerCase()).
        assert_eq!(
            result_text(&result),
            "_a\n-a\n.hidden\na\na_b\na-b\na.b\nab\nångström\napple\näther\nB\neagle\neclair\néclair\nÉcole\nfile10\nfile2\nüber\nz"
        );
    }

    #[tokio::test]
    async fn distinguishes_missing_paths_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = run_ls(dir.path(), json!({"path":"missing"}))
            .await
            .unwrap_err();
        assert!(missing.to_string().contains("Path not found:"));

        std::fs::write(dir.path().join("file.txt"), "").unwrap();
        let file = run_ls(dir.path(), json!({"path":"file.txt"}))
            .await
            .unwrap_err();
        assert!(file.to_string().contains("Not a directory:"));
    }

    #[tokio::test]
    async fn reports_entry_limit_with_an_actionable_notice() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }

        let result = run_ls(dir.path(), json!({"limit":2})).await.unwrap();

        assert!(result_text(&result).contains("2 entries limit reached. Use limit=4 for more"));
        assert_eq!(result.details.as_ref().unwrap()["entryLimitReached"], 2);
    }

    #[tokio::test]
    async fn truncates_output_at_fifty_kibibytes() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..500 {
            let name = format!("{index:03}-{}", "x".repeat(110));
            std::fs::write(dir.path().join(name), "").unwrap();
        }

        let result = run_ls(dir.path(), json!({})).await.unwrap();
        let text = result_text(&result);

        assert!(text.len() < 52 * 1024);
        assert!(text.contains("50.0KB limit reached"));
        assert_eq!(
            result.details.as_ref().unwrap()["truncation"]["truncated"],
            true
        );
    }

    #[tokio::test]
    async fn lists_hidden_files_and_hidden_directories_with_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden-file"), "secret").unwrap();
        std::fs::create_dir(dir.path().join(".hidden-dir")).unwrap();

        let result = run_ls(dir.path(), json!({})).await.unwrap();
        let lines = result_text(&result).lines().collect::<Vec<_>>();

        assert!(lines.contains(&".hidden-file"));
        assert!(lines.contains(&".hidden-dir/"));
    }

    #[tokio::test]
    async fn exact_entry_limit_has_no_notice_or_details() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }

        let result = run_ls(dir.path(), json!({"limit":3})).await.unwrap();

        assert_eq!(result_text(&result), "a.txt\nb.txt\nc.txt");
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn uses_tool_context_cwd_for_default_listing_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ctx-cwd-ls.txt"), "list me").unwrap();

        let result = run_ls(dir.path(), json!({})).await.unwrap();

        assert_eq!(result_text(&result), "ctx-cwd-ls.txt");
    }

    #[tokio::test]
    async fn pre_aborted_listing_does_no_filesystem_work() {
        let dir = tempfile::tempdir().unwrap();
        let (abort, signal) = AbortHandle::new();
        abort.abort();
        let (updates, _) = ToolUpdateSink::channel();

        let result = LsTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("test"),
                json!({"path":"missing"}),
                updates,
            )
            .await;

        assert!(matches!(result, Err(ToolError::Aborted)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_root_access_and_directory_read_failures_like_pi() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let blocked_parent = dir.path().join("blocked-parent");
        let child = blocked_parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::set_permissions(&blocked_parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let inaccessible = run_ls(dir.path(), json!({"path":child})).await;
        std::fs::set_permissions(&blocked_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let inaccessible = inaccessible.unwrap_err();
        assert!(inaccessible.to_string().contains("Path not found:"));

        let unreadable = dir.path().join("unreadable");
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        let read_failure = run_ls(dir.path(), json!({"path":unreadable})).await;
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let read_failure = read_failure.unwrap_err();
        assert!(read_failure.to_string().contains("Cannot read directory:"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skips_entries_that_disappear_or_cannot_be_statted() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        symlink(
            dir.path().join("missing-target"),
            dir.path().join("dangling"),
        )
        .unwrap();

        let result = run_ls(dir.path(), json!({})).await.unwrap();

        assert_eq!(result_text(&result), "(empty directory)");
        assert!(result.details.is_none());
    }
}
