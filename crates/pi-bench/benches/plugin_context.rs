use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pi_bench::{BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure, parameter_map};
use pi_core::{
    AbortHandle, AssistantMessage, ContentBlock, Message, ModelId, PluginId, ProviderId, RunId,
    StopReason, TextContent, Usage, UserMessage,
};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsContextHookBatchInvocation,
    JsContextHookBatchResult, JsGenerationManifest, JsHookManifest, JsHostOperation, JsInvocation,
    JsPluginGeneration, PluginContextHandle,
};
use pi_plugin::{
    AgentPluginContext, ContextEvent, ContextPatch, Plugin, PluginDriver, PluginError,
};
use serde_json::Value;

struct NoopContext(usize);
#[pi_plugin::plugin]
impl Plugin for NoopContext {
    fn id(&self) -> PluginId {
        PluginId::new(format!("context-{}", self.0))
    }
    async fn context(
        &self,
        _: AgentPluginContext,
        event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        black_box(event.messages.len());
        Ok(ContextPatch::default())
    }
}

#[derive(Default)]
struct EncodeOnlyDispatcher {
    calls: AtomicU64,
    bytes: AtomicU64,
}

#[async_trait]
impl JsCallbackDispatcher for EncodeOnlyDispatcher {
    async fn invoke(
        &self,
        _: JsInvocation,
        _: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        Err(JsCallbackError::new(
            "context benchmark expected the batch transport",
        ))
    }

    async fn invoke_context_hook_batch(
        &self,
        invocation: JsContextHookBatchInvocation,
        _: PluginContextHandle,
    ) -> Result<JsContextHookBatchResult, JsCallbackError> {
        let processed = invocation.callbacks.len();
        let encoded = serde_json::to_vec(&JsHostOperation::InvokeContextHookBatch { invocation })
            .map_err(|error| JsCallbackError::new(error.to_string()))?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.bytes
            .fetch_add(encoded.len() as u64, Ordering::Relaxed);
        black_box(encoded);
        Ok(JsContextHookBatchResult {
            processed,
            replacement: None,
            errors: Vec::new(),
        })
    }
}

fn js_plugins(count: usize, dispatcher: Arc<EncodeOnlyDispatcher>) -> Vec<Arc<dyn Plugin>> {
    JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "bench-context".into(),
            agent_plugins: (0..count)
                .map(|index| JsAgentPluginManifest {
                    id: format!("context-{index}"),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: vec![JsHookManifest {
                        name: "context".into(),
                        callback_id: format!("callback-{index}"),
                    }],
                })
                .collect(),
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap()
    .plugins()
}

fn assistant(bytes: usize) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text(TextContent::new("a".repeat(bytes)))],
        api: "scripted".into(),
        provider: ProviderId::new("scripted"),
        model: ModelId::new("test"),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        deferred: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp_ms: 1,
    }
}

fn main() -> BenchResult<()> {
    let config = BenchConfig::from_env(5, 30)?;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut report = BenchmarkReport::new("plugin_context");
    let (_, signal) = AbortHandle::new();
    let run = RunId::new("bench-context");
    for size in [64, 4_096] {
        let messages: Vec<_> = (0..size)
            .map(|index| {
                if index % 2 == 0 {
                    Message::User(UserMessage::text("u".repeat(512), index as i64))
                } else {
                    Message::assistant(assistant(512))
                }
            })
            .collect();
        for kind in ["native", "js-encode-only"] {
            for count in [0, 1, 4, 16] {
                let dispatcher = Arc::new(EncodeOnlyDispatcher::default());
                let plugins = if kind == "native" {
                    (0..count)
                        .map(|index| Arc::new(NoopContext(index)) as Arc<dyn Plugin>)
                        .collect()
                } else {
                    js_plugins(count, dispatcher.clone())
                };
                let driver = PluginDriver::new(plugins)?;
                let mut case = measure(
                    format!("{kind}_{count}_hooks_{size}_messages"),
                    config,
                    fixture_hash(format!("plugin-context:v1:{size}:512:mixed")),
                    parameter_map(&[
                        ("messages", size.to_string()),
                        ("payload_bytes", "512".into()),
                        ("hooks", count.to_string()),
                        ("path", kind.into()),
                    ]),
                    || {
                        let result = executor
                            .block_on(driver.context(
                                &run,
                                Path::new("/bench"),
                                &signal,
                                messages.clone(),
                            ))
                            .map_err(|error| error.to_string())?;
                        if result.len() != size {
                            return Err("context hooks changed the fixture".to_string());
                        }
                        black_box(result);
                        Ok(())
                    },
                )?;
                let iterations = (config.warmup_iterations + config.sample_iterations) as u64;
                case.parameters.insert(
                    "encoded_bytes_per_operation".into(),
                    (dispatcher.bytes.load(Ordering::Relaxed) / iterations).to_string(),
                );
                case.parameters.insert(
                    "host_calls_per_operation".into(),
                    (dispatcher.calls.load(Ordering::Relaxed) / iterations).to_string(),
                );
                report.push(case);
            }
        }
    }
    report.finish()
}
