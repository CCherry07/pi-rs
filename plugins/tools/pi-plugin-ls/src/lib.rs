use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{execution, optional_positive_usize, resolve_to_cwd, spec, truncate_head};
use serde_json::{Value, json};
use std::sync::Arc;
pub struct LsPlugin;
pub struct LsTool;
#[async_trait]
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
        let limit = optional_positive_usize(&input, "limit", 500)?.min(10_000);
        let mut reader = tokio::fs::read_dir(&path)
            .await
            .map_err(|e| execution(format!("cannot list {}: {e}", path.display())))?;
        let mut entries = Vec::new();
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|e| execution(e.to_string()))?
        {
            context
                .abort_signal
                .check()
                .map_err(|_| ToolError::Aborted)?;
            let is_dir = entry.file_type().await.map(|v| v.is_dir()).unwrap_or(false);
            entries.push(format!(
                "{}{}",
                entry.file_name().to_string_lossy(),
                if is_dir { "/" } else { "" }
            ));
        }
        entries.sort();
        let total = entries.len();
        let (text, bytes_truncated) = truncate_head(entries, limit);
        let mut result = ToolResult::text(if text.is_empty() {
            "(empty directory)".to_string()
        } else {
            text
        });
        result.details = Some(json!({"total":total,"truncated":total > limit || bytes_truncated}));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

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
    }
}
