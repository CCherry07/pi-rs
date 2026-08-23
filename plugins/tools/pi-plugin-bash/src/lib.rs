use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pi_core::{
    ContentBlock, TextContent, Tool, ToolCallId, ToolContext, ToolError, ToolResult, ToolSpec,
    ToolUpdate, ToolUpdateSink,
};
use pi_shell::{MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES, ShellChunk, ShellRequest, TruncatedBy};
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
                "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to the last 2000 lines or 50KB. If truncated, full output is saved to a temporary file. Optionally provide a timeout in seconds.",
                json!({"type":"object","properties":{
                "command":{"type":"string","description":"Bash command to execute"},
                "timeout":{"type":"number","description":"Timeout in seconds (optional, no default timeout)"}
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
        let timeout = match input.get("timeout") {
            None | Some(Value::Null) => None,
            Some(value) => {
                const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1_000.0;
                let seconds = value
                    .as_f64()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .ok_or_else(|| {
                        invalid("Invalid timeout: must be a finite number of seconds")
                    })?;
                if seconds > MAX_TIMEOUT_SECONDS {
                    return Err(invalid(format!(
                        "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
                    )));
                }
                Some(Duration::from_secs_f64(seconds))
            }
        };
        updates.send(ToolUpdate {
            content: Vec::new(),
            details: None,
        });
        let update_sink = updates.clone();
        let update_state = Arc::new(Mutex::new(BashUpdateState::default()));
        let callback_state = Arc::clone(&update_state);
        let result = pi_shell::execute(ShellRequest {
            command: command.to_string(),
            cwd: context.cwd,
            timeout,
            shell_path: None,
            abort_signal: context.abort_signal,
            on_chunk: Some(Arc::new(move |chunk: ShellChunk| {
                if let Ok(mut state) = callback_state.lock()
                    && let Some(snapshot) = state.push(&chunk.text)
                {
                    update_sink.send(ToolUpdate {
                        content: vec![ContentBlock::Text(TextContent::new(snapshot))],
                        details: None,
                    });
                }
            })),
        })
        .await
        .map_err(|error| execution(error.to_string()))?;
        let details = bash_details(&result);
        let mut output = format_output(&result, "(no output)");
        updates.send(ToolUpdate {
            content: vec![ContentBlock::Text(TextContent::new(output.clone()))],
            details: details.clone(),
        });
        if result.cancelled {
            output.push_str(if output.is_empty() {
                "Command aborted"
            } else {
                "\n\nCommand aborted"
            });
            return Err(execution(output));
        }
        if result.timed_out {
            let seconds = input
                .get("timeout")
                .map_or_else(|| "unknown".to_string(), Value::to_string);
            output.push_str(&format!("\n\nCommand timed out after {seconds} seconds"));
            return Err(execution(output));
        }
        if let Some(code) = result.exit_code.filter(|code| *code != 0) {
            output.push_str(&format!("\n\nCommand exited with code {code}"));
            return Err(execution(output));
        }
        let mut tool_result = ToolResult::text(output);
        tool_result.details = details;
        Ok(tool_result)
    }
}

#[derive(Default)]
struct BashUpdateState {
    tail: String,
    last_update: Option<Instant>,
}

impl BashUpdateState {
    fn push(&mut self, chunk: &str) -> Option<String> {
        self.tail.push_str(chunk);
        trim_rolling_tail(&mut self.tail);
        let now = Instant::now();
        if self
            .last_update
            .is_some_and(|last_update| now.duration_since(last_update) < Duration::from_millis(100))
        {
            return None;
        }
        self.last_update = Some(now);
        Some(update_snapshot(&self.tail))
    }
}

fn trim_rolling_tail(output: &mut String) {
    let max_bytes = MAX_OUTPUT_BYTES * 2;
    if output.len() <= max_bytes {
        return;
    }
    let mut start = output.len() - max_bytes;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output.drain(..start);
}

fn update_snapshot(output: &str) -> String {
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for line in output.split_terminator('\n').rev() {
        let line_bytes = line.len() + usize::from(!kept.is_empty());
        if kept.len() >= MAX_OUTPUT_LINES || bytes.saturating_add(line_bytes) > MAX_OUTPUT_BYTES {
            break;
        }
        kept.push(line);
        bytes += line_bytes;
    }
    kept.reverse();
    kept.join("\n")
}

fn format_output(result: &pi_shell::ShellResult, empty_text: &str) -> String {
    let mut output = if result.output.is_empty() {
        empty_text.to_string()
    } else {
        result.output.clone()
    };
    let Some(truncation) = result.truncation.as_ref() else {
        return output;
    };
    let full_path = result
        .full_output_path
        .as_ref()
        .map_or_else(|| "unknown".into(), |path| path.display().to_string());
    let end_line = truncation.total_lines;
    if truncation.last_line_partial {
        output.push_str(&format!(
            "\n\n[Showing last {} of line {end_line} (line is {}). Full output: {full_path}]",
            format_size(truncation.output_bytes),
            format_size(truncation.last_line_bytes)
        ));
    } else {
        let start_line = end_line.saturating_sub(truncation.output_lines) + 1;
        if truncation.truncated_by == TruncatedBy::Lines {
            output.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {full_path}]",
                truncation.total_lines
            ));
        } else {
            output.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {full_path}]",
                truncation.total_lines,
                format_size(MAX_OUTPUT_BYTES)
            ));
        }
    }
    output
}

fn bash_details(result: &pi_shell::ShellResult) -> Option<Value> {
    let truncation = result.truncation.as_ref()?;
    Some(json!({
        "truncation": {
            "content": truncation.content,
            "truncated": true,
            "truncatedBy": match truncation.truncated_by {
                TruncatedBy::Lines => "lines",
                TruncatedBy::Bytes => "bytes",
            },
            "totalLines": truncation.total_lines,
            "totalBytes": truncation.total_bytes,
            "outputLines": truncation.output_lines,
            "outputBytes": truncation.output_bytes,
            "lastLinePartial": truncation.last_line_partial,
            "firstLineExceedsLimit": false,
            "maxLines": MAX_OUTPUT_LINES,
            "maxBytes": MAX_OUTPUT_BYTES,
        },
        "fullOutputPath": result.full_output_path,
    }))
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
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
        assert!(text.len() <= pi_shell::MAX_OUTPUT_BYTES + 300);
        let full_output_path = result
            .details
            .as_ref()
            .and_then(|details| details.get("fullOutputPath"))
            .and_then(Value::as_str)
            .unwrap();
        let full_output = std::fs::read_to_string(full_output_path).unwrap();
        assert!(full_output.contains("line-0\n"));
        assert!(full_output.contains("line-9999\n"));
        std::fs::remove_file(full_output_path).unwrap();
    }

    #[tokio::test]
    async fn captures_output_and_nonzero_status() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let error = BashTool
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
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("hello"));
        assert!(text.contains("error"));
        assert!(text.contains("code 7"));
    }

    #[tokio::test]
    async fn has_no_default_timeout_and_accepts_empty_commands() {
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
                json!({"command":""}),
                updates,
            )
            .await
            .unwrap();
        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text == "(no output)")
        );
        assert!(result.details.is_none());
        assert_eq!(
            BashTool
                .spec()
                .parameters
                .pointer("/properties/timeout/type"),
            Some(&Value::String("number".to_string()))
        );
    }

    #[tokio::test]
    async fn supports_fractional_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let error = BashTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"command":"sleep 1","timeout":0.05}),
                updates,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out after 0.05 seconds"));
    }
}
