#![forbid(unsafe_code)]

//! JavaScript tool composition in a fresh, resource-bounded native QuickJS VM.
//! The host owns every tool invocation; JavaScript receives only JSON values.

mod catalog;
mod declarations;
mod models;
mod output;
mod prompt;
mod source;
mod store;
mod vm;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::FutureExt;
use futures::stream::{FuturesUnordered, StreamExt};
use pi_core::{
    AbortHandle, ConstrainedSampling, ContentBlock, GrammarFormat, PluginId, ToolCallId,
    ToolExecutionMode, ToolResult, ToolSpec, ToolUpdate, Usage,
};
use pi_plugin::{
    NestedToolResult, Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolLoadout,
    ToolLoadoutChanges, ToolUpdateSink,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use catalog::Catalog;
use models::{ModelCalls, ModelRequest};
use output::{Output, prefix};
use source::Source;
use vm::{Outcome, Reply, ReplyReservation, VmInput};

pub const CODEMODE_TOOL_NAME: &str = "codemode";
pub use prompt::{CodemodeMode, CodemodeOptions, DEFAULT_CODEMODE_INLINE_BUDGET};

pub const CODEMODE_SOURCE_GRAMMAR: &str = r#"
start: options_source | plain_source
options_source: OPTIONS_LINE NEWLINE SOURCE
plain_source: SOURCE

OPTIONS_LINE: /[ \t]*\/\/ @options:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

/// Independently reusable feature. A new VM is created for every tool call;
/// only successful explicit `store()` writes survive through session entries.
#[derive(Debug, Default)]
pub struct CodemodePlugin;

#[pi_plugin::plugin]
impl Plugin for CodemodePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("codemode")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(codemode_tool())
    }
}

/// Factory-friendly configured codemode presentation; execution uses the same scoped VM.
#[derive(Debug, Clone, Default)]
pub struct ConfiguredCodemodePlugin {
    options: CodemodeOptions,
}

impl ConfiguredCodemodePlugin {
    pub fn new(options: CodemodeOptions) -> Self {
        Self { options }
    }
}

#[pi_plugin::plugin]
impl Plugin for ConfiguredCodemodePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("codemode")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(CodemodeTool {
            options: self.options.clone(),
        }))
    }
}

pub fn codemode_tool() -> Arc<dyn Tool> {
    Arc::new(CodemodeTool {
        options: CodemodeOptions::default(),
    })
}

struct CodemodeTool {
    options: CodemodeOptions,
}

#[async_trait]
impl Tool for CodemodeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: CODEMODE_TOOL_NAME.into(),
            label: CODEMODE_TOOL_NAME.into(),
            description: prompt::description(&[], &self.options),
            parameters: json!({"type":"object","properties":{"code":{"type":"string","description":"Raw JavaScript source."}},"required":["code"]}),
            output_schema: None,
            constrained_sampling: Some(ConstrainedSampling::Grammar {
                variants: [(GrammarFormat::OpenAiLark, CODEMODE_SOURCE_GRAMMAR.into())].into(),
            }),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: Some(prompt::PROMPT_SNIPPET.into()),
            prompt_guidelines: vec![prompt::PROMPT_GUIDELINE.into()],
        }
    }

    fn prepare_loadout(&self, loadout: &ToolLoadout) -> ToolLoadoutChanges {
        let (descriptions, hidden_declarations) =
            prompt::prepare_descriptions(&loadout.declared, &loadout.callable, &self.options);
        ToolLoadoutChanges {
            descriptions,
            hidden_declarations,
        }
    }

    fn model_only(&self) -> bool {
        true
    }

    fn reaches_tools(&self) -> bool {
        true
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        parse_input(input).map(|_| ())
    }

    async fn execute(
        &self,
        context: ToolContext,
        id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let source = parse_input(&input)?;
        let catalog = Arc::new(Catalog::new(context.nested_tools()).map_err(ToolError::Execution)?);
        let store = Value::Object(store::read(&context)?);
        run(context, id, source, catalog, store, updates).await
    }
}

fn parse_input(input: &Value) -> Result<Source, ToolError> {
    let fields = input
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("expected {code: string}".into()))?;
    if fields.len() != 1 {
        return Err(ToolError::InvalidArguments(
            "expected only the code field".into(),
        ));
    }
    let code = fields
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments("code must be a string".into()))?;
    Source::parse(code).map_err(ToolError::InvalidArguments)
}

struct CancelOnDrop(AbortHandle);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run(
    context: ToolContext,
    id: ToolCallId,
    source: Source,
    catalog: Arc<Catalog>,
    store: Value,
    updates: ToolUpdateSink,
) -> Result<ToolResult, ToolError> {
    static WORKERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));
    let started = Instant::now();
    // Admission is cancellable and bounded, including when a nested subagent
    // tries to reserve another VM while the current workers are occupied.
    let permit = tokio::select! {
        biased;
        _ = context.signal().wait() => return Err(ToolError::Aborted),
        permit = tokio::time::timeout(Duration::from_secs(60), WORKERS.clone().acquire_owned()) => {
            permit.map_err(|_| ToolError::Execution("Timed out waiting for a codemode worker".into()))?
                .map_err(|_| ToolError::Execution("Codemode workers unavailable".into()))?
        }
    };
    let deadline = Instant::now() + source.timeout;
    let (abort, signal) = AbortHandle::new();
    let cancel = CancelOnDrop(abort);
    let (requests_tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let (replies_tx, replies) = mpsc::channel();
    let output = Arc::new(Mutex::new(Output::default()));
    let input = VmInput {
        source,
        catalog,
        store,
        signal: signal.clone(),
        parent_signal: context.signal().clone(),
        output: output.clone(),
        requests: requests_tx,
        replies,
        deadline,
        memory_limit: vm::MEMORY_LIMIT,
    };
    let mut worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        vm::execute(input)
    });
    let mut pending = FuturesUnordered::new();
    let closing = Arc::new(AtomicBool::new(false));
    let mut calls = Vec::<Value>::new();
    let mut usage = None;
    let model_calls = Arc::new(ModelCalls::default());
    let generated_images = Arc::new(AtomicUsize::new(0));
    let reply_bytes = Arc::new(AtomicUsize::new(0));
    let mut outcome = loop {
        tokio::select! {
            biased;
            _ = context.signal().wait() => break Outcome { error: Some("Script cancelled".into()), writes: None },
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => break Outcome { error: Some("Script timed out".into()), writes: None },
            finished = &mut worker => break finished.unwrap_or_else(|error| Outcome { error: Some(format!("Script worker failed: {error}")), writes: None }),
            Some(request) = requests.recv() => {
                let call_id = request.model_operation.map_or_else(
                    || ToolCallId::new(format!("{id}/{}",request.id)),
                    |operation| ToolCallId::new(format!("{id}/models.{}/{}", operation.name(), request.id)),
                );
                let model_request = request.model_operation.map(|operation| ModelRequest::prepare(&context, operation, &request.arguments));
                let index = if request.model_operation.is_none_or(|operation| operation.is_inference()) && model_request.as_ref().is_none_or(Result::is_ok) {
                    let index = calls.len();
                    let (name, args) = match request.model_operation {
                        Some(operation) => (format!("models.{}", operation.name()), models::reference_summary(&request.arguments)),
                        None => (request.name.clone(), request.arguments.to_string()),
                    };
                    calls.push(json!({"id":call_id,"name":name,"args":prefix(&args,200),"status":"running"}));
                    publish(&updates, &calls);
                    Some(index)
                } else { None };
                let context = context.clone();
                let signal = signal.clone();
                let closing = closing.clone();
                let model_calls = model_calls.clone();
                let generated_images = generated_images.clone();
                pending.push(async move {
                    if closing.load(Ordering::Relaxed) {
                        return (index, request.id, 0, Err(ToolError::Aborted));
                    }
                    let started = Instant::now();
                    let result = match model_request {
                        Some(Ok(request)) => model_calls.execute(&context, call_id, request, signal, &generated_images).await,
                        Some(Err(error)) => Err(error),
                        None => context.execute_tool(&request.name, request.arguments, signal).await,
                    };
                    (index, request.id, started.elapsed().as_millis(), result)
                });
            }
            Some((index, request_id, elapsed, result)) = pending.next(), if !pending.is_empty() => {
                record_completion(&mut calls, &mut usage, index, elapsed, &result, false);
                let mut result = project(result);
                let bytes = result.as_ref().map_or_else(|error| error.len(), String::len);
                let reservation = match ReplyReservation::new(&reply_bytes, bytes) {
                    Some(reservation) => reservation,
                    None => {
                        result = Err("Queued tool results exceed 32 MiB; await smaller batches".into());
                        // Error replies are bounded by the 128-call cap.
                        ReplyReservation::new(&reply_bytes, 0).expect("zero-byte reply reservation")
                    }
                };
                if let Some(index) = index && let Err(error) = &result {
                    calls[index]["status"] = json!("error");
                    calls[index]["error"] = json!(prefix(error,500));
                }
                publish(&updates, &calls);
                let _ = replies_tx.send(Reply { id: request_id, result, reservation });
            }
        }
    };
    // Do not start calls that the VM queued but the host has not polled. Drain
    // already-ready completions before cancellation so their hooks and usage
    // are not lost merely because the worker-finished branch won the select.
    closing.store(true, Ordering::Relaxed);
    while let Some(Some((index, _, elapsed, result))) = pending.next().now_or_never() {
        record_completion(&mut calls, &mut usage, index, elapsed, &result, false);
    }
    // Started pipelines must observe cancellation and finish tool_result hooks.
    // The generic executor owns cancellation cleanup if this future is dropped
    // by the parent scheduler; on normal VM settlement we explicitly drain it.
    drop(cancel);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some((index, _, elapsed, result)) = pending.next().await {
            record_completion(&mut calls, &mut usage, index, elapsed, &result, true);
        }
    })
    .await;
    drop(pending);
    for call in &mut calls {
        if call["status"] == "running" {
            call["status"] = json!("cancelled");
        }
    }
    publish(&updates, &calls);
    if outcome.error.is_none() {
        if context.signal().is_aborted() {
            outcome.error = Some("Script cancelled".into());
        } else if let Some(writes) = outcome.writes.take()
            && let Err(error) = store::persist(&context, writes)
        {
            outcome.error = Some(error.to_string());
        }
    }
    let mut output = output
        .lock()
        .map_err(|_| ToolError::Execution("Output state unavailable".into()))?;
    let mut content = output.content(outcome.error.as_deref());
    let generated_images = generated_images.load(Ordering::Relaxed);
    if generated_images > 0
        && !content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_)))
    {
        let suffix = if generated_images == 1 { "" } else { "s" };
        content.push(ContentBlock::Text(pi_core::TextContent::new(format!(
            "Note: models.generateImages() returned {generated_images} image{suffix} that the script did not show. Show each image block of result.output with image(block)."
        ))));
    }
    Ok(ToolResult {
        structured_content: None,
        content,
        details: Some(
            json!({"calls":calls,"elapsedMs":started.elapsed().as_millis(),"truncated":output.truncated}),
        ),
        usage,
        added_tool_names: None,
        is_error: outcome.error.is_some(),
        terminate: false,
    })
}

fn record_completion(
    calls: &mut [Value],
    usage: &mut Option<Usage>,
    index: Option<usize>,
    elapsed: u128,
    result: &Result<NestedToolResult, ToolError>,
    cancelling: bool,
) {
    let Some(index) = index else {
        return;
    };
    let call = &mut calls[index];
    call["durationMs"] = json!(elapsed);
    match result {
        Ok(nested) => {
            call["id"] = json!(nested.call_id);
            add_usage(usage, nested.result.usage.as_ref());
            let model_result = nested
                .structured_content
                .as_ref()
                .and_then(|payload| payload.get("value"));
            if let Some(message) = model_result.and_then(|value| value["errorMessage"].as_str()) {
                call["error"] = json!(prefix(message, 500));
            }
            if let Some(value) = &nested.result.usage {
                call["cost"] = json!(value.cost.total);
            }
            call["status"] = json!(if !nested.result.is_error {
                "ok"
            } else if cancelling
                || model_result.is_some_and(|value| value["stopReason"] == "aborted")
            {
                "cancelled"
            } else {
                "error"
            });
        }
        Err(error) => {
            call["status"] = json!(if matches!(error, ToolError::Aborted) {
                "cancelled"
            } else {
                "error"
            });
            call["error"] = json!(prefix(&error.to_string(), 500));
        }
    }
}

fn publish(updates: &ToolUpdateSink, calls: &[Value]) {
    updates.send(ToolUpdate {
        content: Vec::new(),
        details: Some(json!({"calls":calls})),
    });
}

fn project(nested: Result<NestedToolResult, ToolError>) -> Result<String, String> {
    let nested = nested.map_err(|error| error.to_string())?;
    let value = if let Some(structured) = nested.structured_content {
        structured
    } else {
        let mut text = String::new();
        for block in &nested.result.content {
            if let ContentBlock::Text(block) = block {
                if text
                    .len()
                    .saturating_add(block.text.len())
                    .saturating_add(1)
                    > vm::MAX_RESULT_BYTES
                {
                    return Err(
                        "Tool result exceeds the 8 MiB script bridge limit; narrow the query"
                            .into(),
                    );
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&block.text);
            }
        }
        if nested.result.is_error {
            return Err(text);
        }
        Value::String(text)
    };
    struct LimitedJson(Vec<u8>);
    impl std::io::Write for LimitedJson {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > vm::MAX_RESULT_BYTES {
                return Err(std::io::Error::other(
                    "Tool result exceeds the 8 MiB script bridge limit; narrow the query",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = LimitedJson(Vec::new());
    serde_json::to_writer(&mut writer, &value).map_err(|error| error.to_string())?;
    String::from_utf8(writer.0).map_err(|error| error.to_string())
}

fn add_usage(total: &mut Option<Usage>, next: Option<&Usage>) {
    let Some(next) = next else {
        return;
    };
    let total = total.get_or_insert_with(Usage::default);
    total.input = total.input.saturating_add(next.input);
    total.output = total.output.saturating_add(next.output);
    total.cache_read = total.cache_read.saturating_add(next.cache_read);
    total.cache_write = total.cache_write.saturating_add(next.cache_write);
    total.total_tokens = total.total_tokens.saturating_add(next.total_tokens);
    if let Some(value) = next.cache_write_1h {
        total.cache_write_1h = Some(
            total
                .cache_write_1h
                .unwrap_or_default()
                .saturating_add(value),
        );
    }
    if let Some(value) = next.reasoning {
        total.reasoning = Some(total.reasoning.unwrap_or_default().saturating_add(value));
    }
    total.cost.input += next.cost.input;
    total.cost.output += next.cost.output;
    total.cost.cache_read += next.cost.cache_read;
    total.cost.cache_write += next.cost.cache_write;
    total.cost.total += next.cost.total;
}

#[cfg(test)]
mod tests;
