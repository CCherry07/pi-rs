use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    execution, optional_positive_usize, require_str, resolve_to_cwd, spec, truncate_head,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::SystemTime;
pub struct FindPlugin;
pub struct FindTool;
#[async_trait]
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
                "Find files by glob, respecting .gitignore and sorting newest first.",
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
        let matcher = build_glob(require_str(&input, "pattern")?)?;
        let root = resolve_to_cwd(
            &context.cwd,
            input.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let limit = optional_positive_usize(&input, "limit", 1000)?.min(10_000);
        let signal = context.abort_signal.clone();
        let entries = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            for entry in WalkBuilder::new(&root)
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build()
                .filter_map(Result::ok)
            {
                if signal.is_aborted() {
                    return Err(ToolError::Aborted);
                }
                if entry.depth() == 0 {
                    continue;
                }
                let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                if matcher.is_match(rel) {
                    let modified = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    entries.push((
                        modified,
                        format!(
                            "{}{}",
                            rel.display(),
                            if entry.file_type().is_some_and(|v| v.is_dir()) {
                                "/"
                            } else {
                                ""
                            }
                        ),
                    ));
                }
            }
            entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            Ok(entries
                .into_iter()
                .map(|(_, path)| path)
                .collect::<Vec<_>>())
        })
        .await
        .map_err(|e| execution(e.to_string()))??;
        let total = entries.len();
        let (text, bytes_truncated) = truncate_head(entries, limit);
        let mut result = ToolResult::text(if text.is_empty() {
            "No files found".to_string()
        } else {
            text
        });
        result.details = Some(json!({"total":total,"truncated":total > limit || bytes_truncated}));
        Ok(result)
    }
}

fn build_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
    Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|e| pi_tool_support::invalid(format!("invalid glob: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

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
}
