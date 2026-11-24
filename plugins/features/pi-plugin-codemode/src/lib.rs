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
mod workers;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
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

use catalog::Catalog;
use models::{ModelCalls, ModelRequest};
use output::{Output, preview};
use source::Source;
use vm::{Outcome, Reply, VmInput};

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
        run(
            context,
            id,
            source,
            catalog,
            store,
            updates,
            RunOptions {
                models: self.options.models,
                docs_path: self.options.docs_path.clone(),
                workers: workers::WORKERS.clone(),
            },
        )
        .await
    }
}

fn parse_input(input: &Value) -> Result<Source, ToolError> {
    let fields = input
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("expected {code: string}".into()))?;
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

struct RunOptions {
    models: bool,
    docs_path: std::path::PathBuf,
    workers: Arc<workers::Workers>,
}

async fn run(
    context: ToolContext,
    id: ToolCallId,
    mut source: Source,
    catalog: Arc<Catalog>,
    store: Value,
    updates: ToolUpdateSink,
    options: RunOptions,
) -> Result<ToolResult, ToolError> {
    let started = Instant::now();
    let timeout = source.timeout;
    let output_chars = source.output_chars;
    publish(&updates, &[]);
    let wait = timeout
        .unwrap_or(Duration::from_secs(60))
        .min(Duration::from_secs(60));
    let permit = tokio::select! {
        biased;
        _ = context.signal().wait() => None,
        permit = tokio::time::timeout(wait, options.workers.reserve()) => permit.ok(),
    };
    let Some(permit) = permit else {
        let error = if context.signal().is_aborted() {
            "Script aborted: Execution aborted".into()
        } else if timeout.is_some_and(|timeout| timeout <= Duration::from_secs(60)) {
            format!(
                "Script timed out: Execution timed out after {} ms while waiting for a free codemode worker",
                timeout.unwrap().as_millis()
            )
        } else {
            format!(
                "No codemode worker became free within 60 seconds. All {} workers, shared by every session in this process, are held by other scripts. Retry after they finish.",
                workers::max_workers()
            )
        };
        return Ok(ToolResult {
            content: vec![
                ContentBlock::Text(pi_core::TextContent::new(format!(
                    "Script failed\nWall time {:.1} seconds\nOutput:\n",
                    started.elapsed().as_secs_f64()
                ))),
                ContentBlock::Text(pi_core::TextContent::new(format!(
                    "Script error:\n{error}\n\nThe script did not start."
                ))),
            ],
            details: Some(json!({"calls":[]})),
            is_error: true,
            ..ToolResult::text("")
        });
    };
    // The coding-agent deadline includes admission. Match its rounded remaining
    // timeout when reporting a timeout after a worker was acquired.
    source.timeout = timeout.map(|timeout| {
        Duration::from_millis(
            ((timeout.as_secs_f64() - started.elapsed().as_secs_f64()) * 1000.0)
                .round()
                .max(1.0) as u64,
        )
    });
    let timeout = source.timeout;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let (abort, signal) = AbortHandle::new();
    let cancel = CancelOnDrop(abort);
    let (requests_tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let (replies_tx, replies) = mpsc::channel();
    let output = Arc::new(Mutex::new(Output::default()));
    let input = VmInput {
        source,
        catalog: catalog.clone(),
        store,
        signal: signal.clone(),
        parent_signal: context.signal().clone(),
        output: output.clone(),
        requests: requests_tx,
        replies,
        deadline,
        memory_limit: vm::MEMORY_LIMIT,
        models: options.models,
    };
    // QuickJS checks native-stack distance itself, so run it on a dedicated, explicitly bounded
    // OS stack rather than inheriting Tokio's platform/runtime-dependent blocking-thread stack.
    // The blocking task joins that worker: dropping the async handle cannot release the admission
    // permit or detach VM cleanup while native execution is still active.
    let mut worker = tokio::task::spawn_blocking(move || {
        match std::thread::Builder::new()
            .name("pi-codemode-vm".into())
            .stack_size(vm::WORKER_STACK_SIZE)
            .spawn(move || {
                let _permit = permit;
                vm::execute(input)
            }) {
            Ok(worker) => worker.join().unwrap_or_else(|_| Outcome {
                error: Some("Script sandbox failed: Script worker panicked".into()),
                writes: None,
            }),
            Err(error) => Outcome {
                error: Some(format!(
                    "Script sandbox failed: Script worker failed: {error}"
                )),
                writes: None,
            },
        }
    });
    let mut pending = FuturesUnordered::new();
    let closing = Arc::new(AtomicBool::new(false));
    let mut calls = Vec::<Value>::new();
    let mut usage = None;
    let model_calls = Arc::new(ModelCalls::default());
    let generated_images = Arc::new(AtomicUsize::new(0));
    let mut model_call_count = 0;
    let mut outcome = loop {
        tokio::select! {
            biased;
            _ = context.signal().wait() => break Outcome { error: Some("Script aborted: Execution aborted".into()), writes: None },
            _ = wait_until(deadline) => break Outcome { error: Some(vm::timeout_error(timeout)), writes: None },
            Some(request) = requests.recv() => {
                if request.discovery {
                    let result = catalog.query(&request.name, request.arguments_json.as_deref().unwrap_or("[]"))
                        .and_then(|json| serde_json::from_str::<Value>(&json).map_err(|error| error.to_string()))
                        .map(|envelope| (envelope["found"] == true).then(|| envelope["value"].to_string()));
                    let _ = replies_tx.send(Reply { id: request.id, result });
                    continue;
                }
                let model_request = request.model_operation.map(|operation| ModelRequest::prepare(&context, operation, &request.arguments, &options.docs_path));
                let call_id = request.model_operation.map_or_else(
                    || ToolCallId::new(format!("{id}/?")),
                    |operation| {
                        if operation.is_inference() && model_request.as_ref().is_some_and(Result::is_ok) { model_call_count += 1; }
                        ToolCallId::new(format!("{id}/models.{}/{model_call_count}", operation.name()))
                    },
                );
                let index = if request.model_operation.is_none_or(|operation| operation.is_inference()) && model_request.as_ref().is_none_or(Result::is_ok) {
                    let index = calls.len();
                    let (name, args) = match request.model_operation {
                        Some(operation) => (format!("models.{}", operation.name()), models::reference_summary(&request.arguments)),
                        None => (request.name.clone(), request.arguments_json.clone().unwrap_or_default()),
                    };
                    calls.push(json!({"id":call_id,"name":name,"args":preview(&args,200),"status":"running"}));
                    publish(&updates, &calls);
                    Some(index)
                } else { None };
                let context = context.clone();
                let signal = signal.clone();
                let closing = closing.clone();
                let model_calls = model_calls.clone();
                let generated_images = generated_images.clone();
                let started = Instant::now();
                pending.push(async move {
                    let model = request.model_operation.is_some();
                    if closing.load(Ordering::Relaxed) {
                        return (index, request.id, 0, model, Err(ToolError::Aborted));
                    }
                    let result = match model_request {
                        Some(Ok(request)) => model_calls.execute(&context, call_id, request, signal, &generated_images).await,
                        Some(Err(error)) => Err(error),
                        None => context.execute_tool(&request.name, request.arguments, signal).await,
                    };
                    (index, request.id, started.elapsed().as_millis(), model, result)
                });
            }
            Some((index, request_id, elapsed, model, result)) = pending.next(), if !pending.is_empty() => {
                record_completion(&mut calls, &mut usage, index, elapsed, &result, false, model);
                let result = project(result).map_err(|error| {
                    if error.is_empty() {
                        format!("Tool \"{}\" failed", index.and_then(|index| calls[index]["name"].as_str()).unwrap_or_default())
                    } else { error }
                }).map(|value| {
                    if model {
                        let envelope: Value = serde_json::from_str(&value).unwrap_or_default();
                        (envelope["found"] == true).then(|| envelope["value"].to_string())
                    } else { Some(value) }
                });
                if let Some(index) = index && let Err(error) = &result {
                    calls[index]["status"] = json!("error");
                    calls[index]["error"] = json!(preview(error,500));
                }
                publish(&updates, &calls);
                let _ = replies_tx.send(Reply { id: request_id, result });
            }
            finished = &mut worker => break finished.unwrap_or_else(|error| Outcome { error: Some(format!("Script sandbox failed: Script worker failed: {error}")), writes: None }),
        }
    };
    // The select drains queued requests and polls their pipelines before VM
    // completion, so even an unawaited call starts and observes cancellation.
    // Drain already-ready completions before cancellation so their hooks and
    // usage are not lost merely because worker completion won the select.
    closing.store(true, Ordering::Relaxed);
    while let Some(Some((index, _, elapsed, model, result))) = pending.next().now_or_never() {
        record_completion(
            &mut calls, &mut usage, index, elapsed, &result, false, model,
        );
    }
    // Started pipelines must observe cancellation and finish tool_result hooks.
    // The generic executor owns cancellation cleanup if this future is dropped
    // by the parent scheduler; on normal VM settlement we explicitly drain it.
    drop(cancel);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some((index, _, elapsed, model, result)) = pending.next().await {
            record_completion(&mut calls, &mut usage, index, elapsed, &result, true, model);
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
            outcome.error = Some("Script aborted: Execution aborted".into());
        } else if let Some(writes) = outcome.writes.take()
            && let Err(error) = store::persist(&context, writes)
        {
            outcome.error = Some(error.to_string());
        }
    }
    let error = outcome.error.as_ref().map(|error| {
        let summary = if calls.is_empty() {
            "No tool calls were made.".into()
        } else {
            format!(
                "Tool calls made before the failure (they are not undone): {}",
                calls
                    .iter()
                    .map(|call| format!(
                        "{} ({})",
                        call["name"].as_str().unwrap_or_default(),
                        call["status"].as_str().unwrap_or_default()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        format!("{error}\n\n{summary}")
    });
    let output = output
        .lock()
        .map_err(|_| ToolError::Execution("Output state unavailable".into()))?;
    let (content, full_output_path) = output.content(
        error.as_deref(),
        generated_images.load(Ordering::Relaxed),
        output_chars,
        started.elapsed().as_secs_f64(),
    );
    let mut details = json!({"calls":calls});
    if let Some(path) = full_output_path {
        details["fullOutputPath"] = json!(path);
    }
    Ok(ToolResult {
        structured_content: None,
        content,
        details: Some(details),
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
    model: bool,
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
            let model_result = model
                .then_some(&nested.structured_content)
                .and_then(Option::as_ref)
                .and_then(|payload| payload.get("value"));
            if let Some(message) = model_result.and_then(|value| value["errorMessage"].as_str()) {
                call["error"] = json!(preview(message, 500));
            }
            if model {
                if let Some(value) = &nested.result.usage {
                    call["cost"] = json!(value.cost.total);
                }
            } else if nested.result.is_error {
                let message = text_of(&nested.result);
                call["error"] = json!(preview(
                    &if message.is_empty() {
                        format!(
                            "Tool \"{}\" failed",
                            call["name"].as_str().unwrap_or_default()
                        )
                    } else {
                        message
                    },
                    500
                ));
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
            call["error"] = json!(preview(&error.to_string(), 500));
        }
    }
}

fn publish(updates: &ToolUpdateSink, calls: &[Value]) {
    updates.send(ToolUpdate {
        content: Vec::new(),
        details: Some(json!({"calls":calls})),
    });
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending::<()>().await,
    }
}

fn text_of(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(block) => Some(block.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn project(nested: Result<NestedToolResult, ToolError>) -> Result<String, String> {
    let nested = nested.map_err(|error| match error {
        ToolError::InvalidArguments(message) | ToolError::Execution(message) => message,
        other => other.to_string(),
    })?;
    let value = if let Some(structured) = nested.structured_content {
        structured
    } else {
        let text = text_of(&nested.result);
        if nested.result.is_error {
            return Err(text);
        }
        Value::String(text)
    };
    pi_core::json::to_string_with_max_depth(&value, vm::MAX_BRIDGE_DOCUMENT_DEPTH)
        .map_err(|error| error.to_string())
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
