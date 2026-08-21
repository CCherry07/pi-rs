use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    ContentBlock, TextContent, Tool, ToolCallId, ToolContext, ToolError, ToolResult, ToolSpec,
    ToolUpdate, ToolUpdateSink,
};
use pi_shell::{DEFAULT_TIMEOUT, ShellChunk, ShellRequest};
use serde_json::{Value, json};

use pi_tool_support::with_prompt;
use pi_tool_support::{execution, invalid, require_str, spec};

pub struct BashPlugin;
pub struct BashTool;

#[async_trait]
impl pi_core::AgentPlugin for BashPlugin {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("bash-tool")
    }
    fn register(&self, context: &mut pi_core::RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(std::sync::Arc::new(BashTool))
    }
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "bash",
                "Execute a shell command in the working directory. Returns combined stdout/stderr tail, with timeout and cancellation support.",
                json!({"type":"object","properties":{
                "command":{"type":"string"},
                "timeout":{"type":"integer","minimum":0,"description":"Seconds; default 120, 0 disables"}
            },"required":["command"],"additionalProperties":false}),
            ),
            "Execute bash commands (ls, grep, find, etc.)",
            std::iter::empty::<&str>(),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let command = require_str(&input, "command")?;
        if command.trim().is_empty() {
            return Err(invalid("command cannot be empty"));
        }
        let timeout = match input.get("timeout") {
            None | Some(Value::Null) => Some(DEFAULT_TIMEOUT),
            Some(value) => {
                let seconds = value
                    .as_u64()
                    .ok_or_else(|| invalid("timeout must be a non-negative integer"))?;
                (seconds != 0).then(|| std::time::Duration::from_secs(seconds))
            }
        };
        let update_sink = updates.clone();
        let result = pi_shell::execute(ShellRequest {
            command: command.to_string(),
            cwd: context.cwd,
            timeout,
            shell_path: None,
            abort_signal: context.abort_signal,
            on_chunk: Some(Arc::new(move |chunk: ShellChunk| {
                update_sink.send(ToolUpdate {
                    content: vec![ContentBlock::Text(TextContent::new(chunk.text))],
                    details: Some(
                        json!({"stream": format!("{:?}", chunk.stream).to_ascii_lowercase()}),
                    ),
                });
            })),
        })
        .await
        .map_err(|error| execution(error.to_string()))?;
        if result.cancelled {
            return Err(ToolError::Aborted);
        }
        let mut output = if result.output.is_empty() {
            "(no output)".to_string()
        } else {
            result.output
        };
        if result.truncated {
            output.push_str("\n\n[Output truncated to the last 2000 lines or 1MB]");
        }
        if result.timed_out {
            output.push_str("\n\nCommand timed out");
        }
        let code = result.exit_code.unwrap_or(-1);
        let truncated = result.truncated;
        let timed_out = result.timed_out;
        if code != 0 {
            output.push_str(&format!("\n\nCommand exited with code {code}"));
        }
        let mut result = if code == 0 {
            ToolResult::text(output)
        } else {
            ToolResult::error(output)
        };
        result.details = Some(json!({
            "exitCode": code,
            "truncated": truncated,
            "timedOut": timed_out
        }));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    #[tokio::test]
    async fn large_output_is_bounded_and_keeps_tail() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"command":"i=0; while [ $i -lt 10000 ]; do echo line-$i; i=$((i+1)); done"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert!(text.contains("line-9999"));
        assert!(text.len() <= pi_tool_support::MAX_OUTPUT_BYTES + 100);
    }

    #[tokio::test]
    async fn captures_output_and_nonzero_status() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"command":"printf hello; printf error >&2; exit 7"}),
                updates,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(value) => &value.text,
            _ => panic!("expected text"),
        };
        assert!(text.contains("hello"));
        assert!(text.contains("error"));
        assert!(text.contains("code 7"));
    }
}
