#![forbid(unsafe_code)]

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{MAX_WRITE_BYTES, execution, invalid, require_str, resolve_to_cwd, spec};
use serde_json::{Value, json};
use std::io::Write as _;
use std::sync::Arc;

pub struct WritePlugin;
pub struct WriteTool;

#[pi_core::agent_plugin]
impl AgentPlugin for WritePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("write-tool")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(WriteTool))
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "write",
                "Write content to a file. Creates parent directories and replaces the destination atomically.",
                json!({
                    "type":"object","properties":{"path":{"type":"string","description":"Path to the file to write (relative or absolute)"},"content":{"type":"string"}},
                    "required":["path","content"],"additionalProperties":false
                }),
            ),
            "Create or overwrite files",
            ["Use write only for new files or complete rewrites."],
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
        let requested = require_str(&input, "path")?;
        let content = require_str(&input, "content")?;
        if content.len() > MAX_WRITE_BYTES {
            return Err(invalid(format!("content exceeds {MAX_WRITE_BYTES} bytes")));
        }
        let path = resolve_to_cwd(&context.cwd, requested)?;
        tokio::fs::create_dir_all(path.parent().ok_or_else(|| execution("missing parent"))?)
            .await
            .map_err(|e| execution(e.to_string()))?;
        let path_copy = path.clone();
        let bytes = content.as_bytes().to_vec();
        tokio::task::spawn_blocking(move || atomic_write(&path_copy, &bytes))
            .await
            .map_err(|e| execution(e.to_string()))?
            .map_err(|e| execution(e.to_string()))?;
        Ok(ToolResult::text(format!(
            "Successfully wrote {} bytes to {requested}",
            content.len()
        )))
    }
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| std::io::Error::other("missing parent"))?,
    )?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;
    #[tokio::test]
    async fn creates_parent_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        WriteTool
            .execute(
                ToolContext {
                    cwd: dir.path().into(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"path":"a/b.txt","content":"ok"}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b.txt")).unwrap(),
            "ok"
        );
    }
}
