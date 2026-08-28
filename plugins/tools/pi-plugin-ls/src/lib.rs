use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{execution, optional_positive_usize, resolve_to_cwd, spec};
use serde_json::{Map, Value, json};
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 500;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;

pub struct LsPlugin;
pub struct LsTool;
#[pi_core::agent_plugin]
impl AgentPlugin for LsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("ls-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        c.register_tool(Arc::new(LsTool))
    }
}
#[async_trait]
impl Tool for LsTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "ls",
                "List directory entries alphabetically, including dotfiles; directories end in '/'.",
                json!({
                    "type":"object","properties":{"path":{"type":"string","description":"Directory to list (default: current directory)"},"limit":{"type":"integer","minimum":1}},"additionalProperties":false
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
        context
            .abort_signal
            .check()
            .map_err(|_| ToolError::Aborted)?;
        let path = resolve_to_cwd(
            &context.cwd,
            input.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let limit = optional_positive_usize(&input, "limit", DEFAULT_LIMIT)?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    execution(format!("Path not found: {}", path.display()))
                }
                _ => execution(error.to_string()),
            })?;
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
            context
                .abort_signal
                .check()
                .map_err(|_| ToolError::Aborted)?;
            entries.push((entry.file_name(), entry.path()));
        }
        entries.sort_by(|(left, _), (right, _)| {
            left.to_string_lossy()
                .to_lowercase()
                .cmp(&right.to_string_lossy().to_lowercase())
        });

        let mut results = Vec::new();
        let mut entry_limit_reached = false;
        for (name, entry_path) in entries {
            if results.len() >= limit {
                entry_limit_reached = true;
                break;
            }
            context
                .abort_signal
                .check()
                .map_err(|_| ToolError::Aborted)?;
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

        let (mut text, truncation) = truncate_output(&results);
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

fn truncate_output(lines: &[String]) -> (String, Option<Value>) {
    let total_bytes = lines.iter().map(String::len).sum::<usize>() + lines.len().saturating_sub(1);
    if total_bytes <= MAX_OUTPUT_BYTES {
        return (lines.join("\n"), None);
    }

    let mut output = String::new();
    let mut output_lines = 0usize;
    for line in lines {
        let separator = usize::from(!output.is_empty());
        if output
            .len()
            .saturating_add(separator)
            .saturating_add(line.len())
            > MAX_OUTPUT_BYTES
        {
            break;
        }
        if separator == 1 {
            output.push('\n');
        }
        output.push_str(line);
        output_lines += 1;
    }
    let details = json!({
        "content": output.clone(),
        "truncated": true,
        "truncatedBy": "bytes",
        "totalLines": lines.len(),
        "totalBytes": total_bytes,
        "outputLines": output_lines,
        "outputBytes": output.len(),
        "lastLinePartial": false,
        "firstLineExceedsLimit": false,
        "maxLines": 9_007_199_254_740_991u64,
        "maxBytes": MAX_OUTPUT_BYTES
    });
    (output, Some(details))
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
                ToolContext {
                    cwd: cwd.to_path_buf(),
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
}
