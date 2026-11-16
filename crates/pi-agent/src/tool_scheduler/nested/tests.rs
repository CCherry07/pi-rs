use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AgentContext, AgentEvent, Message, PluginId, RunId, ToolCall, ToolCallId,
    ToolExecutionMode, ToolResult, ToolSpec,
};
use pi_plugin::{
    AgentPluginContext, NestedToolResult, Plugin, PluginError, RegistriesBuilder, Tool,
    ToolContext, ToolDiscovery, ToolError, ToolExposure, ToolResultEvent, ToolResultPatch,
    ToolUpdateSink,
};
use serde_json::{Value, json};

use super::{AgentEventSink, EventError, ToolScheduler};
use crate::tool_scheduler::tests::assistant;

fn spec(name: &str, mode: ToolExecutionMode) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        label: name.to_owned(),
        description: name.to_owned(),
        parameters: json!({"type": "object"}),
        execution_mode: mode,
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
    }
}

#[derive(Default)]
struct Observed {
    retained: Mutex<Option<ToolContext>>,
    results: Mutex<Vec<Result<NestedToolResult, String>>>,
    events: Mutex<Vec<AgentEvent>>,
    prepared: AtomicUsize,
    running: AtomicUsize,
    maximum: AtomicUsize,
}

#[async_trait]
impl AgentEventSink for Observed {
    async fn emit(
        &self,
        event: AgentEvent,
        _: pi_core::AbortSignal,
    ) -> Result<AgentEvent, EventError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(event)
    }
}

struct Invoker {
    names: Vec<String>,
    observed: Arc<Observed>,
}

#[async_trait]
impl Tool for Invoker {
    fn spec(&self) -> ToolSpec {
        spec("invoke", ToolExecutionMode::Parallel)
    }

    fn model_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        *self.observed.retained.lock().unwrap() = Some(context.clone());
        let results = futures::future::join_all(
            self.names
                .iter()
                .map(|name| context.execute_tool(name, json!({}), context.signal().clone())),
        )
        .await;
        *self.observed.results.lock().unwrap() = results
            .into_iter()
            .map(|result| result.map_err(|error| error.to_string()))
            .collect();
        Ok(ToolResult::text("outer only"))
    }
}

struct Child {
    name: &'static str,
    mode: ToolExecutionMode,
    model_only: bool,
    structured: bool,
    observed: Arc<Observed>,
}

#[async_trait]
impl Tool for Child {
    fn spec(&self) -> ToolSpec {
        spec(self.name, self.mode)
    }

    fn model_only(&self) -> bool {
        self.model_only
    }

    fn structured_output(&self, result: &ToolResult) -> Option<Value> {
        self.structured.then(|| result.details.clone()).flatten()
    }

    async fn prepare_arguments(&self, _: &ToolContext, input: Value) -> Result<Value, ToolError> {
        self.observed.prepared.fetch_add(1, Ordering::SeqCst);
        Ok(input)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        // This first slice intentionally does not allow lock-upgrading recursive
        // compositors. Ordinary nested tools receive no descendant capability.
        assert!(context.nested_tools().is_empty());
        assert!(
            context
                .execute_tool("active", json!({}), context.signal().clone())
                .await
                .is_err()
        );
        let running = self.observed.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.observed.maximum.fetch_max(running, Ordering::SeqCst);
        if self.mode == ToolExecutionMode::Sequential {
            assert_eq!(running, 1, "sequential call overlapped another call");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.observed.running.fetch_sub(1, Ordering::SeqCst);
        let mut result = ToolResult::text(self.name);
        result.details = Some(json!({"value": self.name}));
        Ok(result)
    }
}

async fn run(
    scheduler: ToolScheduler,
    observed: Arc<Observed>,
    active: &[&str],
    mode: ToolExecutionMode,
) -> super::super::ExecutedToolBatch {
    let (_handle, signal) = AbortHandle::new();
    scheduler
        .execute_batch(
            &RunId::new("nested-run"),
            &assistant(),
            Arc::new(AgentContext {
                system_prompt: String::new(),
                messages: vec![Message::assistant(assistant())],
                active_tools: active.iter().map(|name| (*name).to_owned()).collect(),
            }),
            vec![ToolCall::new("outer", "invoke", json!({}))],
            mode,
            signal,
            observed,
            1,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn nested_scope_filters_and_enforces_names_and_retires_with_parent() {
    let observed = Arc::new(Observed::default());
    let mut builder = RegistriesBuilder::new();
    builder
        .register_tool(
            PluginId::new("test"),
            Arc::new(Invoker {
                names: ["active", "deferred", "inactive", "model-only", "denied"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                observed: Arc::clone(&observed),
            }),
        )
        .unwrap();
    for (name, exposure, model_only, structured) in [
        ("active", ToolExposure::Direct, false, false),
        ("deferred", ToolExposure::Deferred, false, true),
        ("inactive", ToolExposure::Direct, false, false),
        ("model-only", ToolExposure::Deferred, true, false),
        ("denied", ToolExposure::Deferred, false, false),
    ] {
        builder
            .register_tool_with_discovery(
                PluginId::new("test"),
                Arc::new(Child {
                    name,
                    mode: ToolExecutionMode::Parallel,
                    model_only,
                    structured,
                    observed: Arc::clone(&observed),
                }),
                ToolDiscovery {
                    origin: Default::default(),
                    exposure,
                    namespace: None,
                },
            )
            .unwrap();
    }
    let (plugins, registries) = builder.register_plugins(Vec::new()).unwrap();
    let registries = registries.scope_tool_execution(
        &["invoke", "active", "deferred", "inactive", "model-only"].map(String::from),
        "test scope",
    );
    let scheduler = ToolScheduler::new(Arc::new(registries), Arc::new(plugins), ".".into(), 2);
    let batch = run(
        scheduler,
        Arc::clone(&observed),
        &["invoke", "active", "model-only"],
        ToolExecutionMode::Parallel,
    )
    .await;
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].tool_call_id.as_str(), "outer");
    assert_eq!(observed.prepared.load(Ordering::SeqCst), 2);
    {
        let results = observed.results.lock().unwrap();
        assert_eq!(results[0].as_ref().unwrap().call_id.as_str(), "outer/1");
        assert_eq!(results[1].as_ref().unwrap().call_id.as_str(), "outer/2");
        assert!(results[0].as_ref().unwrap().structured_content.is_none());
        assert_eq!(
            results[1].as_ref().unwrap().structured_content,
            Some(json!({"value":"deferred"}))
        );
        assert!(results[2..].iter().all(Result::is_err));
    }
    let retained = observed.retained.lock().unwrap().clone().unwrap();
    assert_eq!(
        retained
            .nested_tools()
            .iter()
            .map(|tool| tool.spec.name.as_str())
            .collect::<Vec<_>>(),
        ["active", "deferred"]
    );
    let (_handle, signal) = AbortHandle::new();
    assert!(matches!(
        retained.execute_tool("active", json!({}), signal).await,
        Err(ToolError::Aborted)
    ));
    assert_eq!(observed.prepared.load(Ordering::SeqCst), 2);
    let events = observed.events.lock().unwrap();
    assert!(events.iter().all(|event| match event {
        AgentEvent::MessageStart {
            message: Message::ToolResult(message),
        }
        | AgentEvent::MessageEnd {
            message: Message::ToolResult(message),
        } => message.tool_call_id.as_str() == "outer",
        AgentEvent::ToolExecutionStart { tool_call_id, .. }
        | AgentEvent::ToolExecutionUpdate { tool_call_id, .. }
        | AgentEvent::ToolExecutionEnd { tool_call_id, .. } => tool_call_id.as_str() == "outer",
        _ => true,
    }));
}

#[tokio::test]
async fn nested_parallelism_is_bounded_and_sequential_calls_are_exclusive() {
    for (mode, names, expected_max) in [
        (ToolExecutionMode::Parallel, vec!["read"; 8], 2),
        (ToolExecutionMode::Sequential, vec!["read"; 8], 1),
        (ToolExecutionMode::Parallel, vec!["write"; 8], 1),
        (
            ToolExecutionMode::Parallel,
            vec!["read", "read", "write", "read", "write"],
            2,
        ),
    ] {
        let observed = Arc::new(Observed::default());
        let mut builder = RegistriesBuilder::new();
        builder
            .register_tool(
                PluginId::new("test"),
                Arc::new(Invoker {
                    names: names.into_iter().map(String::from).collect(),
                    observed: Arc::clone(&observed),
                }),
            )
            .unwrap();
        for (name, mode) in [
            ("read", ToolExecutionMode::Parallel),
            ("write", ToolExecutionMode::Sequential),
        ] {
            builder
                .register_tool(
                    PluginId::new("test"),
                    Arc::new(Child {
                        name,
                        mode,
                        model_only: false,
                        structured: false,
                        observed: Arc::clone(&observed),
                    }),
                )
                .unwrap();
        }
        let (plugins, registries) = builder.register_plugins(Vec::new()).unwrap();
        let scheduler = ToolScheduler::new(Arc::new(registries), Arc::new(plugins), ".".into(), 2);
        let batch = run(
            scheduler,
            Arc::clone(&observed),
            &["invoke", "read", "write"],
            mode,
        )
        .await;
        assert!(!batch.messages[0].is_error);
        assert!(
            observed
                .results
                .lock()
                .unwrap()
                .iter()
                .all(|result| result.as_ref().is_ok_and(|result| !result.result.is_error))
        );
        assert_eq!(observed.maximum.load(Ordering::SeqCst), expected_max);
    }
}

struct WaitingChild {
    started: Arc<tokio::sync::Notify>,
    cancelled_on_drop: Arc<AtomicBool>,
}

struct ChildGuard {
    signal: pi_core::AbortSignal,
    cancelled_on_drop: Arc<AtomicBool>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.cancelled_on_drop
            .store(self.signal.is_aborted(), Ordering::SeqCst);
    }
}

#[async_trait]
impl Tool for WaitingChild {
    fn spec(&self) -> ToolSpec {
        spec("wait", ToolExecutionMode::Sequential)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let _guard = ChildGuard {
            signal: context.signal().clone(),
            cancelled_on_drop: Arc::clone(&self.cancelled_on_drop),
        };
        self.started.notify_one();
        std::future::pending().await
    }
}

type DetachedCall = tokio::task::JoinHandle<Result<NestedToolResult, ToolError>>;

struct CancellationInvoker {
    started: Arc<tokio::sync::Notify>,
    detached: Arc<Mutex<Option<DetachedCall>>>,
    cancel_inside: bool,
}

#[async_trait]
impl Tool for CancellationInvoker {
    fn spec(&self) -> ToolSpec {
        spec("invoke", ToolExecutionMode::Parallel)
    }

    fn model_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let (handle, signal) = AbortHandle::new();
        let child_context = context.clone();
        let task =
            tokio::spawn(
                async move { child_context.execute_tool("wait", json!({}), signal).await },
            );
        self.started.notified().await;
        if self.cancel_inside {
            handle.abort();
            let outcome = task.await.unwrap().unwrap();
            assert!(outcome.result.is_error);
            assert!(!context.signal().is_aborted());
            // Cancellation must release the exclusive gate and leave the
            // parent able to execute another tool.
            let next = context
                .execute_tool("active", json!({}), context.signal().clone())
                .await?;
            assert!(!next.result.is_error);
        } else {
            *self.detached.lock().unwrap() = Some(task);
        }
        Ok(ToolResult::text("outer finished"))
    }
}

#[tokio::test]
async fn nested_cancellation_releases_gate_and_parent_settlement_cancels_unawaited_work() {
    for cancel_inside in [true, false] {
        let observed = Arc::new(Observed::default());
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled_on_drop = Arc::new(AtomicBool::new(false));
        let detached = Arc::new(Mutex::new(None));
        let mut builder = RegistriesBuilder::new();
        builder
            .register_tool(
                PluginId::new("test"),
                Arc::new(CancellationInvoker {
                    started: Arc::clone(&started),
                    detached: Arc::clone(&detached),
                    cancel_inside,
                }),
            )
            .unwrap();
        builder
            .register_tool(
                PluginId::new("test"),
                Arc::new(WaitingChild {
                    started,
                    cancelled_on_drop: Arc::clone(&cancelled_on_drop),
                }),
            )
            .unwrap();
        builder
            .register_tool(
                PluginId::new("test"),
                Arc::new(Child {
                    name: "active",
                    mode: ToolExecutionMode::Parallel,
                    model_only: false,
                    structured: false,
                    observed: Arc::clone(&observed),
                }),
            )
            .unwrap();
        let (plugins, registries) = builder.register_plugins(Vec::new()).unwrap();
        let scheduler = ToolScheduler::new(Arc::new(registries), Arc::new(plugins), ".".into(), 1);
        let batch = tokio::time::timeout(
            Duration::from_secs(1),
            run(
                scheduler,
                Arc::clone(&observed),
                &["invoke", "wait", "active"],
                ToolExecutionMode::Parallel,
            ),
        )
        .await
        .expect("nested cancellation did not settle");
        assert!(!batch.messages[0].is_error);
        let task = detached.lock().unwrap().take();
        if let Some(task) = task {
            let result = tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();
            assert!(result.is_err() || result.is_ok_and(|result| result.result.is_error));
        }
        assert!(cancelled_on_drop.load(Ordering::SeqCst));
        let events = observed.events.lock().unwrap();
        let ended = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
            .unwrap();
        assert!(
            !events[ended + 1..]
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
        );
    }
}

struct AwaitingInvoker;

#[async_trait]
impl Tool for AwaitingInvoker {
    fn spec(&self) -> ToolSpec {
        spec("invoke", ToolExecutionMode::Parallel)
    }

    fn model_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.record_usage(&pi_core::Usage {
            input: 11,
            total_tokens: 11,
            ..Default::default()
        });
        context
            .execute_tool("wait", json!({}), context.signal().clone())
            .await?;
        Ok(ToolResult::text("finished"))
    }
}

type InterruptedCalls = Arc<Mutex<Vec<(String, Option<String>, bool)>>>;

struct CancellationHooks {
    calls: InterruptedCalls,
    alive: Arc<AtomicUsize>,
    stall: bool,
}

struct HookGuard(Arc<AtomicUsize>);

impl Drop for HookGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[pi_plugin::plugin]
impl Plugin for CancellationHooks {
    fn id(&self) -> PluginId {
        PluginId::new("cancellation-hooks")
    }

    async fn tool_result(
        &self,
        _: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        self.calls.lock().unwrap().push((
            event.tool_call.id.to_string(),
            event.parent_tool_call_id.map(|id| id.to_string()),
            event.interrupted,
        ));
        if event.tool_call.name == "wait" {
            self.alive.fetch_add(1, Ordering::SeqCst);
            let _guard = HookGuard(Arc::clone(&self.alive));
            if self.stall {
                std::future::pending::<()>().await;
            }
            // Force the finalization to survive beyond the dropped parent
            // execution future rather than completing in the cancelling poll.
            tokio::time::sleep(Duration::from_millis(10)).await;
            return Ok(ToolResultPatch {
                usage: Some(pi_core::Usage {
                    output: 7,
                    total_tokens: 7,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        Ok(ToolResultPatch::default())
    }
}

#[tokio::test]
async fn external_parent_abort_finalizes_nested_hook_once_and_bounds_stalled_cleanup() {
    for stall in [false, true] {
        let observed = Arc::new(Observed::default());
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicUsize::new(0));
        let mut builder = RegistriesBuilder::new();
        builder
            .register_tool(PluginId::new("test"), Arc::new(AwaitingInvoker))
            .unwrap();
        builder
            .register_tool(
                PluginId::new("test"),
                Arc::new(WaitingChild {
                    started: Arc::clone(&started),
                    cancelled_on_drop: Arc::clone(&cancelled),
                }),
            )
            .unwrap();
        let (plugins, registries) = builder
            .register_plugins(vec![Arc::new(CancellationHooks {
                calls: Arc::clone(&calls),
                alive: Arc::clone(&alive),
                stall,
            })])
            .unwrap();
        let scheduler = ToolScheduler::new(Arc::new(registries), Arc::new(plugins), ".".into(), 1);
        let (abort, signal) = AbortHandle::new();
        let run_id = RunId::new("external-cancel");
        let assistant = assistant();
        let execution = scheduler.execute_batch(
            &run_id,
            &assistant,
            Arc::new(AgentContext {
                system_prompt: String::new(),
                messages: vec![Message::assistant(assistant.clone())],
                active_tools: vec!["invoke".into(), "wait".into()],
            }),
            vec![ToolCall::new("outer", "invoke", json!({}))],
            ToolExecutionMode::Parallel,
            signal,
            observed.clone(),
            1,
        );
        let (batch, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(execution, async {
                started.notified().await;
                abort.abort();
            })
        })
        .await
        .expect("parent retained a stalled nested pipeline");
        let batch = batch.unwrap();
        assert_eq!(batch.messages.len(), 1);
        assert!(batch.messages[0].is_error);
        if !stall {
            let usage = batch.messages[0].usage.as_ref().unwrap();
            assert_eq!(usage.input, 11);
            assert_eq!(usage.output, 7);
            assert_eq!(usage.total_tokens, 18);
        } else {
            let usage = batch.messages[0].usage.as_ref().unwrap();
            assert_eq!(usage.input, 11);
            assert_eq!(usage.total_tokens, 11);
        }
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(alive.load(Ordering::SeqCst), 0);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                ("outer/1".into(), Some("outer".into()), true),
                ("outer".into(), None, true),
            ]
        );
        let events = observed.events.lock().unwrap();
        let ended = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
            .unwrap();
        assert!(
            !events[ended + 1..]
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
        );
    }
}
