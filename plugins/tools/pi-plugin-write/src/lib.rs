#![forbid(unsafe_code)]

use async_trait::async_trait;
use pi_core::{PluginId, ToolCallId, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_tool_support::with_prompt;
use pi_tool_support::{execution, require_str, resolve_to_cwd, spec, with_file_mutation_queue};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct WritePlugin;
pub struct WriteTool;

#[pi_plugin::plugin]
impl Plugin for WritePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("write-tool")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(WriteTool))
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        let mut tool = with_prompt(
            spec(
                "write",
                "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
                json!({
                    "type":"object","properties":{"path":{"type":"string","description":"Path to the file to write (relative or absolute)"},"content":{"type":"string","description":"Content to write to the file"}},
                    "required":["path","content"]
                }),
            ),
            "Create or overwrite files",
            ["Use write only for new files or complete rewrites."],
        );
        tool.constrained_sampling = Some(pi_core::ConstrainedSampling::JsonSchema {
            strict: pi_core::ToolSchemaStrict::Prefer,
        });
        tool
    }
    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let requested = require_str(&input, "path")?.to_string();
        let content = require_str(&input, "content")?.to_string();
        let path = resolve_to_cwd(context.cwd(), &requested)?;
        with_file_mutation_queue(path.clone(), async move {
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            let created = tokio::fs::create_dir_all(
                path.parent().ok_or_else(|| execution("missing parent"))?,
            )
            .await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            created.map_err(|error| execution(error.to_string()))?;
            let written = tokio::fs::write(path, content).await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            written.map_err(|error| execution(error.to_string()))?;
            Ok(ToolResult::text(format!(
                "Successfully wrote to {requested}"
            )))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;
    #[cfg(unix)]
    #[tokio::test]
    async fn writes_large_content_through_symlinks_without_replacing_the_target_inode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("target");
        let alias = root.path().join("alias");
        let hardlink = root.path().join("hardlink");
        std::fs::write(&path, "before").unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        std::fs::hard_link(&path, &hardlink).unwrap();
        let content = "x".repeat(10 * 1024 * 1024 + 1);
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = WriteTool
            .execute(
                ToolContext::standalone(root.path().into(), signal),
                ToolCallId::new("large"),
                json!({"path":"alias", "content":content}),
                updates,
            )
            .await
            .unwrap();
        assert!(
            std::fs::symlink_metadata(alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(hardlink).unwrap(), content);
        assert_eq!(
            result.content,
            ToolResult::text("Successfully wrote to alias").content
        );
    }

    #[tokio::test]
    async fn creates_parent_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        WriteTool
            .execute(
                ToolContext::standalone(dir.path().into(), signal),
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
