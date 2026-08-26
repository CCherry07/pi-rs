use async_trait::async_trait;
use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{execution, optional_positive_usize, require_str, resolve_to_cwd, spec};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 1000;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;

pub struct FindPlugin;
pub struct FindTool;
#[pi_core::agent_plugin]
impl AgentPlugin for FindPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("find-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        c.register_tool(Arc::new(FindTool))
    }
}
#[async_trait]
impl Tool for FindTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "find",
                "Search for files by glob pattern. Returns matching paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB.",
                json!({
                    "type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string","description":"Directory to search in (default: current directory)"},"limit":{"type":"integer","minimum":1}},
                    "required":["pattern"],"additionalProperties":false
                }),
            ),
            "Find files by glob pattern (respects .gitignore)",
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
        let pattern = require_str(&input, "pattern")?;
        let matcher = build_glob(pattern)?;
        let root = resolve_to_cwd(
            &context.cwd,
            input.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let metadata = std::fs::metadata(&root)
            .map_err(|_| execution(format!("Path not found: {}", root.display())))?;
        if !metadata.is_dir() {
            return Err(execution(format!("Not a directory: {}", root.display())));
        }
        let limit = optional_positive_usize(&input, "limit", DEFAULT_LIMIT)?;
        let require_git = is_inside_git_repository(&root);
        let signal = context.abort_signal.clone();
        let entries = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            let mut builder = WalkBuilder::new(&root);
            builder
                .hidden(false)
                .ignore(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .require_git(require_git)
                .add_custom_ignore_filename(".fdignore");
            for entry in builder.build() {
                if signal.is_aborted() {
                    return Err(ToolError::Aborted);
                }
                let entry = entry.map_err(|error| execution(error.to_string()))?;
                if entry.depth() == 0 {
                    continue;
                }
                let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                let candidate = if matcher.full_path {
                    entry.path()
                } else {
                    entry.path().file_name().map(Path::new).unwrap_or(rel)
                };
                if !matcher.glob.is_match(candidate) {
                    continue;
                }
                let mut rendered = rel.to_string_lossy().replace('\\', "/");
                if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    rendered.push('/');
                }
                entries.push(rendered);
                if entries.len() >= limit {
                    break;
                }
            }
            Ok(entries)
        })
        .await
        .map_err(|e| execution(e.to_string()))??;

        if entries.is_empty() {
            return Ok(ToolResult::text("No files found matching pattern"));
        }

        let result_limit_reached = entries.len() >= limit;
        let (mut text, truncation) = truncate_output(&entries);
        let mut notices = Vec::new();
        let mut details = Map::new();
        if result_limit_reached {
            notices.push(format!(
                "{limit} results limit reached. Use limit={} for more, or refine pattern",
                limit.saturating_mul(2)
            ));
            details.insert("resultLimitReached".into(), json!(limit));
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

struct FindMatcher {
    glob: GlobMatcher,
    full_path: bool,
}

fn build_glob(pattern: &str) -> Result<FindMatcher, ToolError> {
    let full_path = pattern.contains('/');
    let mut effective_pattern = pattern.to_string();
    if full_path && !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
        effective_pattern = format!("**/{pattern}");
    }
    #[cfg(windows)]
    if full_path {
        effective_pattern = effective_pattern.replace('/', "[/\\\\]");
    }
    let glob = GlobBuilder::new(&effective_pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| pi_tool_support::invalid(format!("invalid glob: {error}")))?
        .compile_matcher();
    Ok(FindMatcher { glob, full_path })
}

fn is_inside_git_repository(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
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

    async fn run_find(cwd: &std::path::Path, input: Value) -> Result<ToolResult, ToolError> {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        FindTool
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
    async fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "ok").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "no").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = FindTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"pattern":"*.txt"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert!(text.contains("visible.txt"));
        assert!(!text.contains("ignored.txt"));
    }

    #[tokio::test]
    async fn includes_hidden_files_and_respects_gitignore_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".secret")).unwrap();
        std::fs::write(dir.path().join(".secret/hidden.txt"), "yes").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "no").unwrap();

        let result = run_find(dir.path(), json!({"pattern":"**/*.txt"}))
            .await
            .unwrap();
        let text = result_text(&result);

        assert!(text.contains(".secret/hidden.txt"));
        assert!(!text.contains("ignored.txt"));
    }

    #[tokio::test]
    async fn matches_basenames_unless_pattern_contains_a_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        std::fs::write(dir.path().join("src/nested/one.spec.ts"), "").unwrap();
        std::fs::write(dir.path().join("other/two.spec.ts"), "").unwrap();

        let basename = run_find(dir.path(), json!({"pattern":"*.ts"}))
            .await
            .unwrap();
        assert!(result_text(&basename).contains("src/nested/one.spec.ts"));
        assert!(result_text(&basename).contains("other/two.spec.ts"));

        let full_path = run_find(dir.path(), json!({"pattern":"src/**/*.spec.ts"}))
            .await
            .unwrap();
        assert!(result_text(&full_path).contains("src/nested/one.spec.ts"));
        assert!(!result_text(&full_path).contains("other/two.spec.ts"));
    }

    #[tokio::test]
    async fn stops_parent_gitignore_rules_at_a_nested_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repository = dir.path().join("nested-repo");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "visible-inside.txt\n").unwrap();
        std::fs::write(repository.join("visible-inside.txt"), "yes").unwrap();

        let result = run_find(dir.path(), json!({"pattern":"*.txt", "path":repository}))
            .await
            .unwrap();

        assert!(result_text(&result).contains("visible-inside.txt"));
    }

    #[tokio::test]
    async fn reports_empty_and_limited_results_like_pi() {
        let dir = tempfile::tempdir().unwrap();
        let empty = run_find(dir.path(), json!({"pattern":"--help"}))
            .await
            .unwrap();
        assert_eq!(result_text(&empty), "No files found matching pattern");
        assert!(empty.details.is_none());

        std::fs::write(dir.path().join("one.txt"), "").unwrap();
        std::fs::write(dir.path().join("two.txt"), "").unwrap();
        let limited = run_find(dir.path(), json!({"pattern":"*.txt", "limit":1}))
            .await
            .unwrap();
        assert!(result_text(&limited).contains("1 results limit reached"));
        assert_eq!(limited.details.as_ref().unwrap()["resultLimitReached"], 1);
    }

    #[tokio::test]
    async fn truncates_output_at_fifty_kibibytes() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..500 {
            let name = format!("{index:03}-{}.txt", "x".repeat(110));
            std::fs::write(dir.path().join(name), "").unwrap();
        }

        let result = run_find(dir.path(), json!({"pattern":"*.txt"}))
            .await
            .unwrap();
        let text = result_text(&result);

        assert!(text.len() < 52 * 1024);
        assert!(text.contains("50.0KB limit reached"));
        assert_eq!(
            result.details.as_ref().unwrap()["truncation"]["truncated"],
            true
        );
    }
}
