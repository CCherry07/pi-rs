use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pi_core::{ContentBlock, TextContent, ToolCallId, ToolResult, ToolSpec, ToolUpdate};
use pi_plugin::{Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_shell::{MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES, ShellChunk, ShellRequest, TruncatedBy};
use serde_json::{Value, json};

use pi_tool_support::with_prompt;
use pi_tool_support::{execution, invalid, require_str, spec};

pub struct BashPlugin;
pub struct BashTool;

/// Immutable settings captured when a runtime generation is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashToolOptions {
    pub shell_path: Option<PathBuf>,
    pub command_prefix: Option<String>,
    pub expose_session_environment: bool,
}

impl Default for BashToolOptions {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl BashToolOptions {
    pub fn new(shell_path: Option<PathBuf>, command_prefix: Option<String>) -> Self {
        Self {
            shell_path,
            command_prefix,
            expose_session_environment: true,
        }
    }
}

/// Configured form used by the product runtime. `BashPlugin` remains the
/// zero-configuration public default for embedders and tests.
pub struct ConfiguredBashPlugin {
    options: BashToolOptions,
}

impl ConfiguredBashPlugin {
    pub fn new(options: BashToolOptions) -> Self {
        Self { options }
    }
}

struct ConfiguredBashTool {
    options: BashToolOptions,
}

#[pi_plugin::plugin]
impl pi_plugin::Plugin for BashPlugin {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("bash-tool")
    }
    fn register(&self, context: &mut pi_plugin::RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(std::sync::Arc::new(BashTool))
    }
}

#[pi_plugin::plugin]
impl pi_plugin::Plugin for ConfiguredBashPlugin {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("bash-tool")
    }

    fn register(&self, context: &mut pi_plugin::RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(std::sync::Arc::new(ConfiguredBashTool {
            options: self.options.clone(),
        }))
    }
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        shell_tool_spec(
            "bash",
            "bash",
            "Execute bash commands (ls, grep, find, etc.)",
            true,
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_shell_tool(&BashToolOptions::default(), None, context, input, updates).await
    }
}

/// Shared declaration for independently registered shell tools.
pub fn shell_tool_spec(
    name: &str,
    shell_name: &str,
    snippet: &str,
    expose_session_environment: bool,
) -> ToolSpec {
    let mut tool = with_prompt(
        spec(
            name,
            &format!(
                "Execute a {shell_name} command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds."
            ),
            json!({"type":"object","properties":{
                "command":{"type":"string","description":"Shell command to execute"},
                "timeout":{"type":"number","description":"Timeout in seconds (optional, no default timeout)"}
            },"required":["command"]}),
        ),
        snippet,
        ["You can inspect PI_* environment variables for current model and session details."],
    );
    if !expose_session_environment {
        tool.prompt_guidelines.clear();
    }
    tool.output_schema = Some(json!({
        "type":"object", "properties":{
            "output":{"type":"string","description":"Combined stdout and stderr, possibly truncated"},
            "truncated":{"type":"boolean"},
            "full_output_path":{"type":"string","description":"Full output, when truncated"},
            "full_output_error":{"type":"string","description":"Why the full output could not be saved, when truncated and saving failed"},
            "exit_code":{"type":"number"}, "wall_time_seconds":{"type":"number"}
        }, "required":["output","truncated","exit_code","wall_time_seconds"]
    }));
    tool.constrained_sampling = Some(pi_core::ConstrainedSampling::JsonSchema {
        strict: pi_core::ToolSchemaStrict::Prefer,
    });
    tool
}

#[async_trait]
impl Tool for ConfiguredBashTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = BashTool.spec();
        if !self.options.expose_session_environment {
            spec.prompt_guidelines.clear();
        }
        spec
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_shell_tool(&self.options, None, context, input, updates).await
    }
}

/// Executes a shell tool with the shared Pi output contract. The optional
/// configuration selects another shell's process arguments and output prefix.
pub async fn execute_shell_tool(
    options: &BashToolOptions,
    configuration: Option<pi_shell::ShellConfiguration>,
    context: ToolContext,
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
                .ok_or_else(|| invalid("Invalid timeout: must be a finite number of seconds"))?;
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
    let environment = session_environment(&context, options.expose_session_environment)?;
    let request = ShellRequest {
        command: prefixed_command(options.command_prefix.as_deref(), command),
        cwd: context.cwd().to_path_buf(),
        timeout,
        shell_path: options.shell_path.clone(),
        abort_signal: context.signal().clone(),
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
    };
    let started_at = Instant::now();
    let result = match configuration {
        Some(configuration) => {
            pi_shell::execute_configured(request, environment, configuration).await
        }
        None => pi_shell::execute_with_environment(request, environment).await,
    }
    .map_err(|error| execution(error.to_string()))?;
    let details = bash_details(&result);
    let mut output = format_output(
        &result,
        if result.cancelled || result.timed_out {
            ""
        } else {
            "(no output)"
        },
    );
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
        let mut status = format!("Command timed out after {seconds} seconds");
        if let (Some(exit_code), Some(after)) = (result.exit_code, result.shell_exit_after) {
            let after_seconds = (after.as_secs_f64() * 10.0).round() / 10.0;
            status.push_str(&format!(". The shell exited with code {exit_code} after {after_seconds} seconds, but background processes it started kept writing output until the timeout."));
        }
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&status);
        return Err(execution(output));
    }
    let Some(exit_code) = result.exit_code else {
        output.push_str("\n\nCommand terminated without an exit code");
        return Err(execution(output));
    };
    let wall_time_seconds = (started_at.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    let full_output = result
        .read_full_output(1024 * 1024)
        .await
        .map_err(|error| execution(error.to_string()))?;
    let mut structured_output = json!({
        "output":full_output.content, "truncated":full_output.truncated,
        "exit_code":exit_code, "wall_time_seconds":wall_time_seconds,
    });
    if full_output.truncated
        && let Some(path) = &result.full_output_path
    {
        structured_output["full_output_path"] = json!(path);
    }
    if exit_code != 0 {
        output.push_str(&format!("\n\nCommand exited with code {exit_code}"));
    }
    let mut tool_result = ToolResult::text(output);
    tool_result.details = details;
    tool_result.structured_content = Some(structured_output);
    tool_result.is_error = exit_code != 0;
    Ok(tool_result)
}

fn session_environment(
    context: &ToolContext,
    expose: bool,
) -> Result<BTreeMap<OsString, Option<OsString>>, ToolError> {
    let mut environment = [
        "PI_SESSION_ID",
        "PI_SESSION_FILE",
        "PI_PROVIDER",
        "PI_MODEL",
        "PI_REASONING_LEVEL",
    ]
    .into_iter()
    .map(|key| (OsString::from(key), None))
    .collect::<BTreeMap<_, _>>();
    if !expose {
        return Ok(environment);
    }
    let session_id = match context.session.id() {
        Ok(id) => id,
        Err(
            pi_plugin::PluginContextError::Unbound | pi_plugin::PluginContextError::Unavailable(_),
        ) => {
            return Ok(environment);
        }
        Err(error) => return Err(error.into()),
    };
    environment.insert("PI_SESSION_ID".into(), Some(session_id.into()));
    if let Some(path) = context.session.file()? {
        environment.insert("PI_SESSION_FILE".into(), Some(path.into_os_string()));
    }
    if let Some(model) = context.models.selection()? {
        environment.insert("PI_PROVIDER".into(), Some(model.provider.as_str().into()));
        environment.insert("PI_MODEL".into(), Some(model.model_id.as_str().into()));
    }
    if let Some(thinking_level) = context.models.thinking_level()? {
        environment.insert(
            "PI_REASONING_LEVEL".into(),
            Some(thinking_level.as_str().into()),
        );
    }
    Ok(environment)
}

fn prefixed_command(prefix: Option<&str>, command: &str) -> String {
    prefix.filter(|prefix| !prefix.is_empty()).map_or_else(
        || command.to_string(),
        |prefix| format!("{prefix}\n{command}"),
    )
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
    let start_line = end_line.saturating_sub(truncation.output_lines) + 1;
    if truncation.last_line_partial {
        output.push_str(&format!(
            "\n\n[Showing last {} of line {start_line} (line is {}). Full output: {full_path}]",
            format_size(truncation.output_bytes),
            format_size(truncation.last_line_bytes)
        ));
    } else {
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
    use pi_core::{AbortHandle, ModelSelection, ThinkingLevel};
    use pi_plugin::{
        ModelsContextAccess, PluginContextEpoch, PluginContextResult, SessionContextAccess,
        UiContextAccess,
    };

    struct SessionMetadata {
        file: Option<PathBuf>,
        model: Mutex<ModelSelection>,
    }

    impl SessionContextAccess for SessionMetadata {
        fn session_id(&self) -> PluginContextResult<String> {
            Ok("current-session".into())
        }

        fn session_file(&self) -> PluginContextResult<Option<PathBuf>> {
            Ok(self.file.clone())
        }
    }

    impl ModelsContextAccess for SessionMetadata {
        fn model_selection(&self) -> PluginContextResult<Option<ModelSelection>> {
            Ok(Some(self.model.lock().unwrap().clone()))
        }

        fn thinking_level(&self) -> PluginContextResult<Option<ThinkingLevel>> {
            Ok(Some(ThinkingLevel::High))
        }
    }

    impl UiContextAccess for SessionMetadata {}

    fn execution_error_message(error: ToolError) -> String {
        let ToolError::Execution(message) = error else {
            panic!("expected an execution error")
        };
        message
    }

    fn full_output_path_from_message(message: &str) -> PathBuf {
        let marker = "Full output: ";
        let start = message.find(marker).expect("full-output marker") + marker.len();
        let end = message[start..]
            .find(']')
            .map_or(message.len(), |offset| start + offset);
        PathBuf::from(&message[start..end])
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exports_current_session_metadata_to_each_shell_invocation() {
        let directory = tempfile::tempdir().unwrap();
        let metadata = Arc::new(SessionMetadata {
            file: Some(directory.path().join("session with spaces.jsonl")),
            model: Mutex::new(ModelSelection::new("provider-one", "model-one")),
        });
        let epoch = PluginContextEpoch::new(metadata.clone());
        let (_, signal) = AbortHandle::new();
        let context = ToolContext::with_plugin_context(
            directory.path().to_path_buf(),
            signal,
            epoch.context(),
        );
        for model in ["model-one", "model-two"] {
            *metadata.model.lock().unwrap() = ModelSelection::new("provider-one", model);
            let (updates, _) = ToolUpdateSink::channel();
            let result = BashTool.execute(
                context.clone(),
                ToolCallId::new("environment"),
                json!({"command":"printf '%s\\n' \"$PI_SESSION_ID\" \"$PI_SESSION_FILE\" \"$PI_PROVIDER\" \"$PI_MODEL\" \"$PI_REASONING_LEVEL\""}),
                updates,
            ).await.unwrap();
            let expected = format!(
                "current-session\n{}\nprovider-one\n{model}\nhigh\n",
                metadata.file.as_ref().unwrap().display()
            );
            assert!(
                matches!(&result.content[0], ContentBlock::Text(text) if text.text == expected)
            );
        }
    }

    #[test]
    fn session_environment_clears_absent_or_disabled_metadata() {
        let (_, signal) = AbortHandle::new();
        let context = ToolContext::standalone(std::env::current_dir().unwrap(), signal.clone());
        let standalone = session_environment(&context, true).unwrap();
        assert_eq!(standalone.len(), 5);
        assert!(standalone.values().all(Option::is_none));

        let epoch = PluginContextEpoch::new(Arc::new(SessionMetadata {
            file: None,
            model: Mutex::new(ModelSelection::new("provider", "model")),
        }));
        let context = ToolContext::with_plugin_context(
            std::env::current_dir().unwrap(),
            signal,
            epoch.context(),
        );
        let exposed = session_environment(&context, true).unwrap();
        assert_eq!(
            exposed.get(std::ffi::OsStr::new("PI_SESSION_FILE")),
            Some(&None)
        );
        assert!(
            session_environment(&context, false)
                .unwrap()
                .values()
                .all(Option::is_none)
        );
        let configured = ConfiguredBashTool {
            options: BashToolOptions {
                expose_session_environment: false,
                ..BashToolOptions::default()
            },
        };
        assert!(configured.spec().prompt_guidelines.is_empty());
        assert_eq!(BashTool.spec().prompt_guidelines.len(), 1);
    }

    #[tokio::test]
    async fn large_output_is_bounded_and_keeps_tail() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
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
        let structured = result.structured_content.as_ref().unwrap();
        assert_eq!(structured["output"], full_output);
        assert_eq!(structured["truncated"], false);
        assert!(structured.get("full_output_path").is_none());
        std::fs::remove_file(full_output_path).unwrap();
    }

    #[tokio::test]
    async fn captures_output_and_nonzero_status() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"command":"printf hello; printf error >&2; exit 7"}),
                updates,
            )
            .await
            .unwrap();
        let pi_core::ContentBlock::Text(text) = &result.content[0] else {
            panic!("expected text")
        };
        let text = &text.text;
        assert!(text.contains("hello"));
        assert!(text.contains("error"));
        assert!(text.contains("code 7"));
        assert!(result.is_error);
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["exit_code"], 7);
        assert_eq!(structured["truncated"], false);
        assert!(
            !structured["output"]
                .as_str()
                .unwrap()
                .contains("exited with code")
        );
        assert!(structured["wall_time_seconds"].as_f64().unwrap() >= 0.0);
    }

    #[tokio::test]
    async fn executes_simple_commands() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("simple"),
                json!({"command":"echo 'test output'"}),
                updates,
            )
            .await
            .unwrap();
        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text == "test output\n"
        ));
        assert!(result.details.is_none());
        assert_eq!(result.structured_content.as_ref().unwrap()["exit_code"], 0);
    }

    #[tokio::test]
    async fn nonzero_and_empty_results_match_the_shell_tool_contract() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let failed = BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal.clone()),
                ToolCallId::new("failed"),
                json!({"command":"echo out; exit 3"}),
                updates,
            )
            .await
            .unwrap();
        assert!(failed.is_error);
        assert!(matches!(
            &failed.content[0],
            ContentBlock::Text(text) if text.text == "out\n\n\nCommand exited with code 3"
        ));
        assert_eq!(
            failed.structured_content.as_ref().unwrap()["output"],
            "out\n"
        );
        assert_eq!(failed.structured_content.as_ref().unwrap()["exit_code"], 3);

        let (updates, _) = ToolUpdateSink::channel();
        let empty = BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("empty"),
                json!({"command":"true"}),
                updates,
            )
            .await
            .unwrap();
        assert!(matches!(
            &empty.content[0],
            ContentBlock::Text(text) if text.text == "(no output)"
        ));
        assert_eq!(empty.structured_content.as_ref().unwrap()["output"], "");
        assert_eq!(
            empty.structured_content.as_ref().unwrap()["truncated"],
            false
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn programmatic_output_links_complete_file_above_one_mebibyte() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool.execute(ToolContext::standalone(directory.path().into(), signal), ToolCallId::new("large"), json!({"command":"printf head; head -c 1100000 /dev/zero | tr '\\0' x; printf tail"}), updates).await.unwrap();
        let structured = result.structured_content.unwrap();
        let output = structured["output"].as_str().unwrap();
        assert_eq!(structured["truncated"], true);
        assert!(output.starts_with("head"));
        assert!(output.ends_with("tail"));
        assert!(output.contains("bytes omitted"));
        let path = structured["full_output_path"].as_str().unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), 1_100_008);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signal_exit_remains_a_structured_failed_result() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        for (name, exit_code) in [("KILL", 137), ("TERM", 143)] {
            let (updates, _) = ToolUpdateSink::channel();
            let result = BashTool
                .execute(
                    ToolContext::standalone(directory.path().into(), signal.clone()),
                    ToolCallId::new(format!("signal-{name}")),
                    json!({"command":format!("printf 'before-kill\\n'; kill -{name} $$")}),
                    updates,
                )
                .await
                .unwrap();
            assert!(result.is_error);
            assert!(matches!(
                &result.content[0],
                ContentBlock::Text(text)
                    if text.text == format!("before-kill\n\n\nCommand exited with code {exit_code}")
            ));
            assert_eq!(result.structured_content.unwrap()["exit_code"], exit_code);
        }
    }

    #[tokio::test]
    async fn has_no_default_timeout_and_accepts_empty_commands() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = BashTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_reports_shell_exit_when_descendants_keep_writing() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            BashTool.execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("background"),
                // Builtins avoid making post-exit output depend on interpreter
                // startup or spawning a sleep process before the 100ms grace.
                json!({"command":"(: > started; i=0; while [ $i -lt 500 ]; do printf tick; j=0; while [ $j -lt 1000 ]; do j=$((j+1)); done; i=$((i+1)); done) & while [ ! -f started ]; do :; done; exit 7", "timeout":0.3}),
                updates,
            ),
        )
        .await
        .expect("timeout must remain active while draining descendant output")
        .unwrap_err();
        let ToolError::Execution(message) = error else {
            panic!("expected an execution error")
        };
        assert!(message.contains("tick"));
        assert!(
            message.contains(
                "Command timed out after 0.3 seconds. The shell exited with code 7 after "
            )
        );
        assert!(message.ends_with(
            "seconds, but background processes it started kept writing output until the timeout."
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn empty_timeout_feedback_does_not_claim_no_output() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let error = BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("timeout"),
                json!({"command":"sleep 1", "timeout":0.05}),
                updates,
            )
            .await
            .unwrap_err();
        let ToolError::Execution(message) = error else {
            panic!("expected an execution error")
        };
        assert_eq!(message, "Command timed out after 0.05 seconds");
    }

    #[tokio::test]
    async fn supports_fractional_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let error = BashTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"command":"sleep 1","timeout":0.05}),
                updates,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out after 0.05 seconds"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn truncated_timeout_and_abort_errors_link_complete_output() {
        let directory = tempfile::tempdir().unwrap();

        let (_, timeout_signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let timeout_error = BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), timeout_signal),
                ToolCallId::new("timeout-output"),
                json!({
                    "command":"i=1; while [ $i -le 3000 ]; do echo $i; i=$((i+1)); done; sleep 30",
                    "timeout":0.5
                }),
                updates,
            )
            .await
            .unwrap_err();
        let timeout_message = execution_error_message(timeout_error);
        assert!(timeout_message.contains("Command timed out after 0.5 seconds"));
        assert!(timeout_message.contains("[Showing lines 1001-3000 of 3000. Full output: "));
        let timeout_path = full_output_path_from_message(&timeout_message);
        let timeout_output = std::fs::read_to_string(&timeout_path).unwrap();
        assert!(timeout_output.starts_with("1\n2\n3\n"));
        assert!(timeout_output.ends_with("2999\n3000\n"));
        std::fs::remove_file(timeout_path).unwrap();

        let ready = directory.path().join("ready");
        let (abort, abort_signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let cwd = directory.path().to_path_buf();
        let running = tokio::spawn(async move {
            BashTool
                .execute(
                    ToolContext::standalone(cwd, abort_signal),
                    ToolCallId::new("abort-output"),
                    json!({
                        "command":"i=1; while [ $i -le 3000 ]; do echo $i; i=$((i+1)); done; touch ready; sleep 30"
                    }),
                    updates,
                )
                .await
        });
        for _ in 0..200 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready.exists(), "shell did not finish producing output");
        abort.abort();
        let abort_message = execution_error_message(running.await.unwrap().unwrap_err());
        assert!(abort_message.contains("Command aborted"));
        assert!(abort_message.contains("[Showing lines 1001-3000 of 3000. Full output: "));
        let abort_path = full_output_path_from_message(&abort_message);
        let abort_output = std::fs::read_to_string(&abort_path).unwrap();
        assert!(abort_output.starts_with("1\n2\n3\n"));
        assert!(abort_output.ends_with("2999\n3000\n"));
        std::fs::remove_file(abort_path).unwrap();
    }

    #[tokio::test]
    async fn missing_cwd_and_shell_path_report_actionable_errors() {
        let directory = tempfile::tempdir().unwrap();
        let missing_cwd = directory.path().join("does-not-exist");
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let cwd_error = BashTool
            .execute(
                ToolContext::standalone(missing_cwd.clone(), signal.clone()),
                ToolCallId::new("cwd"),
                json!({"command":"echo test"}),
                updates,
            )
            .await
            .unwrap_err();
        assert!(cwd_error.to_string().contains(&format!(
            "Working directory does not exist: {}",
            missing_cwd.display()
        )));

        let missing_shell = directory.path().join("missing-shell");
        let (updates, _) = ToolUpdateSink::channel();
        let shell_error = ConfiguredBashTool {
            options: BashToolOptions::new(Some(missing_shell.clone()), None),
        }
        .execute(
            ToolContext::standalone(directory.path().into(), signal),
            ToolCallId::new("shell"),
            json!({"command":"echo test"}),
            updates,
        )
        .await
        .unwrap_err();
        assert!(shell_error.to_string().contains(&format!(
            "Custom shell path not found: {}",
            missing_shell.display()
        )));
    }

    #[test]
    fn chatty_output_updates_are_coalesced_and_keep_the_tail() {
        let mut state = BashUpdateState::default();
        let updates = (0..5_000)
            .filter_map(|line| state.push(&format!("line {line}\n")))
            .collect::<Vec<_>>();
        assert!(updates.len() < 25);
        assert!(state.tail.contains("line 4999\n"));
    }

    #[test]
    fn partial_long_line_notice_uses_the_first_retained_line() {
        let result = pi_shell::ShellResult {
            output: format!("{}END\n", "y".repeat(100)),
            exit_code: Some(0),
            cancelled: false,
            timed_out: false,
            shell_exit_after: None,
            truncated: true,
            truncation: Some(pi_shell::ShellTruncation {
                content: format!("{}END\n", "y".repeat(100)),
                truncated_by: TruncatedBy::Bytes,
                total_lines: 3,
                total_bytes: 219_020,
                output_lines: 2,
                output_bytes: 104,
                last_line_partial: true,
                last_line_bytes: 0,
            }),
            full_output_path: Some("/tmp/full-output".into()),
        };
        let output = format_output(&result, "");
        assert!(output.contains("[Showing last 104B of line 2"));
        assert!(!output.contains("of line 3"));
    }

    #[tokio::test]
    async fn tool_uses_the_context_cwd() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("cwd"),
                json!({"command":"printf marker > ctx-cwd-marker"}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("ctx-cwd-marker")).unwrap(),
            "marker"
        );
    }

    #[cfg(windows)]
    fn windows_executable_on_path(name: &str) -> Option<PathBuf> {
        let output = std::process::Command::new("where")
            .arg(name)
            .output()
            .ok()?;
        let decoded = String::from_utf8_lossy(&output.stdout);
        decoded
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(PathBuf::from)
    }

    #[cfg(windows)]
    fn bash_single_quoted_path(path: &std::path::Path) -> String {
        format!(
            "'{}'",
            path.to_string_lossy()
                .replace('\\', "/")
                .replace('\'', "'\"'\"'")
        )
    }

    #[cfg(windows)]
    fn cleanup_windows_process(pid_file: &std::path::Path) {
        use std::os::windows::process::CommandExt;

        let Ok(pid) = std::fs::read_to_string(pid_file) else {
            return;
        };
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let _ = std::process::Command::new(
            std::path::Path::new(&root)
                .join("System32")
                .join("taskkill.exe"),
        )
        .args(["/F", "/T", "/PID", pid.trim()])
        .creation_flags(0x08000000)
        .status();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_bash_tool_releases_quiet_inherited_handles() {
        let Some(node) = windows_executable_on_path("node.exe") else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("spawn-inherited-child.js");
        let pid_file = directory.path().join("descendant.pid");
        std::fs::write(
            &script,
            r#"const fs = require("fs");
const { spawn } = require("child_process");
const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)"], { stdio: "inherit", detached: true });
fs.writeFileSync(process.argv[2], String(child.pid));
child.unref();
console.log("child-exiting");
"#,
        )
        .unwrap();
        let command = format!(
            "{} {} {}",
            bash_single_quoted_path(&node),
            bash_single_quoted_path(&script),
            bash_single_quoted_path(&pid_file)
        );
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            BashTool.execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("windows-inherited-handles"),
                json!({"command":command}),
                updates,
            ),
        )
        .await;
        cleanup_windows_process(&pid_file);
        let result = result
            .expect("quiet inherited handles must not keep the bash tool open")
            .unwrap();
        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text.contains("child-exiting")
        ));
    }

    #[tokio::test]
    async fn configured_tool_prepends_the_command_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ConfiguredBashTool {
            options: BashToolOptions::new(None, Some("configured_value=from-settings".to_string())),
        }
        .execute(
            ToolContext::standalone(dir.path().to_path_buf(), signal),
            ToolCallId::new("1"),
            json!({"command":"printf '%s' \"$configured_value\""}),
            updates,
        )
        .await
        .unwrap();

        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text == "from-settings"
        ));
    }

    #[tokio::test]
    async fn configured_tool_includes_prefix_and_command_output() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ConfiguredBashTool {
            options: BashToolOptions::new(None, Some("echo prefix-output".to_string())),
        }
        .execute(
            ToolContext::standalone(directory.path().into(), signal),
            ToolCallId::new("prefix-output"),
            json!({"command":"echo command-output"}),
            updates,
        )
        .await
        .unwrap();

        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text.trim() == "prefix-output\ncommand-output"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_tool_uses_the_selected_shell_path() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ConfiguredBashTool {
            options: BashToolOptions::new(Some("/bin/sh".into()), None),
        }
        .execute(
            ToolContext::standalone(dir.path().to_path_buf(), signal),
            ToolCallId::new("1"),
            json!({"command":"printf '%s' \"$0\""}),
            updates,
        )
        .await
        .unwrap();

        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text == "/bin/sh"
        ));
    }
}
