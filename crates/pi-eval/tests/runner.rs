use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{PluginId, ToolCall, ToolCallId, ToolExecutionMode, ToolResult, ToolSpec};
use pi_eval::{
    ArtifactStore, EvalCase, EvalError, EvalExecutionOutcome, EvalFixture, EvalLimits, EvalRunner,
    EvalStep, EvalTranscriptEvent, ExactOutputGrader, PreparedEvalTarget, WorkspaceChangeKind,
};
use pi_plugin::{
    Plugin, RegisterContext, SessionPluginContext, SessionShutdownEvent, Tool, ToolContext,
    ToolError, ToolUpdateSink,
};
use pi_sdk::{
    AgentHost, ModelSelection, PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt,
    WorkspaceSnapshot,
};
use pi_session::MultiSessionManager;
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

#[derive(Clone)]
struct OrdersPlugin {
    shutdowns: Arc<AtomicUsize>,
}

#[pi_plugin::plugin]
impl Plugin for OrdersPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("orders")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(ReserveOrder))
    }

    async fn session_shutdown(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), pi_plugin::PluginError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ReserveOrder;

#[async_trait]
impl Tool for ReserveOrder {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "reserve_order".into(),
            label: "Reserve an order".into(),
            description: "Reserves an order and records the reservation receipt.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"orderId": {"type": "string"}},
                "required": ["orderId"]
            }),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let receipt = json!({"orderId": input["orderId"], "status": "reserved"});
        std::fs::write(context.cwd().join("receipt.json"), receipt.to_string())
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        Ok(ToolResult {
            details: Some(receipt.clone()),
            ..ToolResult::text(receipt.to_string())
        })
    }
}

#[tokio::test]
async fn domain_target_continues_after_tools_reloads_factories_and_persists_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture");
    for name in [".pi", "target", "node_modules", ".git"] {
        std::fs::create_dir_all(fixture.join(name)).unwrap();
        std::fs::write(fixture.join(name).join("data"), "fixture data").unwrap();
    }
    let prepares = Arc::new(AtomicUsize::new(0));
    let registrations = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let providers = Arc::new(Mutex::new(Vec::<Arc<ScriptedProvider>>::new()));
    let rendered_tools = Arc::new(Mutex::new(Vec::new()));
    let context_path = Arc::new(Mutex::new(None));
    let runner = EvalRunner::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
    let case = EvalCase::new(
        "orders/reservation",
        "Reserve an order through the order service",
    )
    .fixture(EvalFixture::Directory(fixture))
    .active_tools(["reserve_order"])
    .plugin({
        let registrations = Arc::clone(&registrations);
        let shutdowns = Arc::clone(&shutdowns);
        move || {
            registrations.fetch_add(1, Ordering::SeqCst);
            OrdersPlugin {
                shutdowns: Arc::clone(&shutdowns),
            }
        }
    })
    .step(EvalStep::PromptTemplate("Reserve {{order}}.".into()))
    .step(EvalStep::Reload)
    .step(EvalStep::Prompt("Confirm the reservation.".into()))
    .grader(ExactOutputGrader::new("Order ORD-42 is reserved."));
    let run = runner
        .run(&case, "order-service", 1, {
            let prepares = Arc::clone(&prepares);
            let providers = Arc::clone(&providers);
            let rendered_tools = Arc::clone(&rendered_tools);
            let context_path = Arc::clone(&context_path);
            move |context| async move {
                prepares.fetch_add(1, Ordering::SeqCst);
                assert!(context.session_path.parent().unwrap().is_dir());
                assert!(!context.root.join("home").exists());
                assert_eq!(
                    context.active_tools.as_deref(),
                    Some(&["reserve_order".into()][..])
                );
                for name in [".pi", "target", "node_modules", ".git"] {
                    assert!(context.workspace.join(name).join("data").exists());
                    std::fs::write(context.workspace.join(name).join("data"), "prepared data")
                        .unwrap();
                }
                *context_path.lock().unwrap() = Some(context.root.clone());
                std::fs::write(context.workspace.join("catalog.json"), "{\"stock\":42}").unwrap();
                let host = AgentHost::builder(ModelSelection::new("scripted", "test"), "unused")
                    .active_tools(context.active_tools.unwrap())
                    .system_prompt(SystemPrompt::dynamic(move |_: &WorkspaceSnapshot| {
                        let rendered_tools = Arc::clone(&rendered_tools);
                        Ok(PreparedSystemPrompt::new(
                            move |context: PromptContext<'_>| {
                                let selected = context
                                    .active_tools
                                    .iter()
                                    .map(|tool| tool.name.clone())
                                    .collect::<Vec<_>>();
                                // The preparer applies the requested tools before the first render.
                                assert_eq!(selected, ["reserve_order"]);
                                rendered_tools.lock().unwrap().push(selected);
                                Ok(PromptOutput::new("Handle order reservations."))
                            },
                        ))
                    }))
                    .provider_plugin_factory(move || {
                        let mut providers = providers.lock().unwrap();
                        let turns = if providers.is_empty() {
                            vec![
                                ScriptedTurn::ToolCalls(vec![ToolCall::new(
                                    "reserve-42",
                                    "reserve_order",
                                    json!({"orderId": "ORD-42"}),
                                )]),
                                ScriptedTurn::Text("Reserved order ORD-42.".into()),
                            ]
                        } else {
                            vec![ScriptedTurn::Text("Order ORD-42 is reserved.".into())]
                        };
                        let plugin = ScriptedProviderPlugin::scripted(turns);
                        providers.push(plugin.provider());
                        plugin
                    })
                    .build();
                let mut target =
                    PreparedEvalTarget::new(host.session_manager(), "scripted", "test");
                target.template_bindings = BTreeMap::from([("order".into(), "ORD-42".into())]);
                target.prompt_transform = Some(Arc::new(|prompt| {
                    Ok(format!("{prompt}\nRecord a receipt."))
                }));
                Ok(target)
            }
        })
        .await
        .unwrap();

    assert!(run.passed, "{run:#?}");
    assert_eq!(run.schema_version, 1);
    assert_eq!(run.observation.usage.tool_calls, 1);
    assert_eq!(
        run.observation.system_prompt.as_deref(),
        Some("Handle order reservations.\nRecord a receipt.")
    );
    assert_eq!(prepares.load(Ordering::SeqCst), 1);
    assert_eq!(registrations.load(Ordering::SeqCst), 2);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 2);
    assert_eq!(rendered_tools.lock().unwrap().len(), 2);
    let providers = providers.lock().unwrap();
    assert_eq!(providers.len(), 2);
    assert_eq!(providers[0].requests().len(), 2);
    assert_eq!(providers[1].requests().len(), 1);
    let continuation = &providers[0].requests()[1];
    assert!(continuation.messages.iter().any(|message| matches!(message, pi_core::Message::ToolResult(result) if result.tool_name == "reserve_order")));
    assert!(run.observation.transcript.iter().any(|event| matches!(event, EvalTranscriptEvent::Message {role, content} if role == "user" && content == "Reserve ORD-42.")));
    for name in [".pi/data", "target/data", "node_modules/data", ".git/data"] {
        assert!(
            run.observation
                .workspace_changes
                .iter()
                .any(|change| change.path == name && change.kind == WorkspaceChangeKind::Modified)
        );
    }
    for name in ["receipt.json", "catalog.json"] {
        assert!(
            run.observation
                .workspace_changes
                .iter()
                .any(|change| change.path == name && change.kind == WorkspaceChangeKind::Added)
        );
    }
    let session = run
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "session.jsonl")
        .unwrap();
    let session = std::fs::read_to_string(runner.artifacts().root().join(&session.path)).unwrap();
    assert!(session.contains("reserve-42"));
    assert!(session.contains("reserved"));
    assert!(!context_path.lock().unwrap().as_ref().unwrap().exists());
    let persisted = std::fs::read_to_string(runner.artifacts().root().join("runs.jsonl")).unwrap();
    let persisted: pi_eval::EvalRun = serde_json::from_str(persisted.trim()).unwrap();
    assert_eq!(persisted, run);
}

async fn assert_manager_closed(manager: &MultiSessionManager) {
    assert!(manager.sessions().is_empty());
    let directory = tempfile::tempdir().unwrap();
    let result = manager
        .create_session(directory.path(), directory.path().join("closed.jsonl"))
        .await;
    assert!(matches!(
        result,
        Err(pi_session::MultiSessionManagerError::Closed)
    ));
}

#[tokio::test]
async fn missing_template_bindings_are_errors_and_still_shutdown() {
    for name in ["home", "agent_dir", "workspace", "custom"] {
        let root = tempfile::tempdir().unwrap();
        let runner = EvalRunner::new(ArtifactStore::new(root.path()).unwrap());
        let host = AgentHost::builder(ModelSelection::new("scripted", "test"), "Respond")
            .provider_plugin_factory(|| {
                ScriptedProviderPlugin::scripted([ScriptedTurn::Text("unused".into())])
            })
            .build();
        let manager = host.session_manager();
        let case = EvalCase::new("templates/missing", "No implicit domain path bindings")
            .step(EvalStep::PromptTemplate(format!("{{{{{name}}}}}")));
        let run = runner
            .run(&case, "empty-bindings", 1, |_| async {
                Ok(PreparedEvalTarget::new(manager.clone(), "scripted", "test"))
            })
            .await
            .unwrap();
        assert_eq!(run.execution_outcome, EvalExecutionOutcome::Errored);
        assert!(run.observation.errors[0].contains("unbound prompt template token"));
        assert!(run.observation.transcript.is_empty());
        assert_manager_closed(&manager).await;
    }
}

#[tokio::test]
async fn session_creation_and_artifact_errors_close_prepared_manager() {
    for fail_creation in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join("artifacts");
        let runner = EvalRunner::new(ArtifactStore::new(&artifacts).unwrap());
        let mut builder = AgentHost::builder(ModelSelection::new("scripted", "test"), "Respond")
            .provider_plugin_factory(|| {
                ScriptedProviderPlugin::scripted([ScriptedTurn::Text("ready".into())])
            });
        if fail_creation {
            builder = builder.active_tools(vec!["missing-tool".into()]);
        }
        let manager = builder.build().session_manager();
        let case = EvalCase::new("cleanup/failure", "Always close prepared manager")
            .step(EvalStep::Prompt("Proceed".into()));
        let error = runner
            .run(&case, "failure", 1, |_| async {
                if !fail_creation {
                    std::fs::remove_dir(&artifacts).unwrap();
                    std::fs::write(&artifacts, "not a directory").unwrap();
                }
                Ok(PreparedEvalTarget::new(manager.clone(), "scripted", "test"))
            })
            .await
            .unwrap_err();
        if fail_creation {
            assert!(matches!(error, EvalError::Runtime(_)), "{error}");
        } else {
            assert!(matches!(error, EvalError::Artifact(_)), "{error}");
        }
        assert_manager_closed(&manager).await;
    }
}

#[tokio::test]
async fn timed_out_provider_is_drained_before_manager_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let runner = EvalRunner::new(ArtifactStore::new(root.path()).unwrap());
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let host = AgentHost::builder(ModelSelection::new("scripted", "test"), "Respond")
        .plugin_factory({
            let shutdowns = Arc::clone(&shutdowns);
            move || OrdersPlugin {
                shutdowns: Arc::clone(&shutdowns),
            }
        })
        .provider_plugin_factory(|| ScriptedProviderPlugin::scripted([ScriptedTurn::WaitForAbort]))
        .build();
    let manager = host.session_manager();
    let case = EvalCase::new("cleanup/timeout", "Drain the provider before cleanup")
        .limits(EvalLimits {
            step_timeout: Duration::from_millis(20),
        })
        .step(EvalStep::Prompt("Wait".into()));
    let run = runner
        .run(&case, "timeout", 1, |_| async {
            Ok(PreparedEvalTarget::new(manager.clone(), "scripted", "test"))
        })
        .await
        .unwrap();
    assert_eq!(run.execution_outcome, EvalExecutionOutcome::TimedOut);
    assert!(!run.passed);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_manager_closed(&manager).await;
}

struct RemoveWorkspace;

#[pi_plugin::plugin]
impl Plugin for RemoveWorkspace {
    fn id(&self) -> PluginId {
        PluginId::new("remove-workspace")
    }

    async fn before_agent_start(
        &self,
        context: pi_plugin::AgentPluginContext,
        _event: pi_plugin::BeforeAgentStartEvent,
    ) -> Result<pi_plugin::BeforeAgentStartPatch, pi_plugin::PluginError> {
        std::fs::remove_dir_all(context.cwd()).unwrap();
        Ok(pi_plugin::BeforeAgentStartPatch::default())
    }
}

#[tokio::test]
async fn observation_failure_shuts_down_the_running_session() {
    let root = tempfile::tempdir().unwrap();
    let runner = EvalRunner::new(ArtifactStore::new(root.path()).unwrap());
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let host = AgentHost::builder(ModelSelection::new("scripted", "test"), "Respond")
        .plugin_factory({
            let shutdowns = Arc::clone(&shutdowns);
            move || OrdersPlugin {
                shutdowns: Arc::clone(&shutdowns),
            }
        })
        .provider_plugin_factory(|| {
            ScriptedProviderPlugin::scripted([ScriptedTurn::Text("ready".into())])
        })
        .build();
    let manager = host.session_manager();
    let case = EvalCase::new(
        "cleanup/observation",
        "Snapshot failure still closes the session",
    )
    .plugin(|| RemoveWorkspace)
    .step(EvalStep::Prompt("Proceed".into()));
    let error = runner
        .run(&case, "missing-workspace", 1, |_| async {
            Ok(PreparedEvalTarget::new(manager.clone(), "scripted", "test"))
        })
        .await
        .unwrap_err();
    assert!(matches!(error, EvalError::Fixture(_)), "{error}");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_manager_closed(&manager).await;
}

#[derive(Default)]
struct CancellationProbe {
    tool_started: tokio::sync::Notify,
    tool_cleaned_with_workspace: std::sync::atomic::AtomicBool,
    shutdown_started: tokio::sync::Notify,
    release_shutdown: tokio::sync::Notify,
    shutdown_completed: tokio::sync::Notify,
    shutdowns: AtomicUsize,
}

struct CancellationPlugin(Arc<CancellationProbe>);

#[pi_plugin::plugin]
impl Plugin for CancellationPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("cancellation-probe")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(BlockingOrder(Arc::clone(&self.0))))
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), pi_plugin::PluginError> {
        self.0.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.0.shutdown_started.notify_one();
        self.0.before_cleanup(context.cwd()).await;
        Ok(())
    }
}

impl CancellationProbe {
    async fn before_cleanup(&self, workspace: &std::path::Path) {
        self.release_shutdown.notified().await;
        assert!(
            workspace.is_dir(),
            "workspace must survive the entire shutdown hook"
        );
        self.shutdown_completed.notify_one();
    }
}

struct BlockingOrder(Arc<CancellationProbe>);

#[async_trait]
impl Tool for BlockingOrder {
    fn spec(&self) -> ToolSpec {
        ReserveOrder.spec()
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        struct Cleanup {
            probe: Arc<CancellationProbe>,
            workspace: std::path::PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.probe
                    .tool_cleaned_with_workspace
                    .store(self.workspace.is_dir(), Ordering::SeqCst);
            }
        }
        let _cleanup = Cleanup {
            probe: Arc::clone(&self.0),
            workspace: context.cwd().to_path_buf(),
        };
        self.0.notify_tool_started();
        // Ignores the abort signal, so cancellation must own and join the task.
        std::future::pending().await
    }
}

impl CancellationProbe {
    fn notify_tool_started(&self) {
        self.tool_started.notify_one();
    }
}

async fn wait_for_signal(signal: &tokio::sync::Notify) {
    tokio::time::timeout(Duration::from_secs(5), signal.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn caller_cancellation_drains_blocked_tools_and_preserves_inflight_shutdown() {
    for cancel_during_tool in [true, false] {
        let artifacts = tempfile::tempdir().unwrap();
        let runner = EvalRunner::new(ArtifactStore::new(artifacts.path()).unwrap());
        let probe = Arc::new(CancellationProbe::default());
        let context_root = Arc::new(Mutex::new(None));
        let host = AgentHost::builder(ModelSelection::new("scripted", "test"), "Reserve an order")
            .plugin_factory({
                let probe = Arc::clone(&probe);
                move || CancellationPlugin(Arc::clone(&probe))
            })
            .provider_plugin_factory(move || {
                ScriptedProviderPlugin::scripted([if cancel_during_tool {
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "blocked-order",
                        "reserve_order",
                        json!({"orderId": "ORD-42"}),
                    )])
                } else {
                    ScriptedTurn::Text("ready".into())
                }])
            })
            .build();
        let manager = host.session_manager();
        let running = tokio::spawn({
            let manager = manager.clone();
            let context_root = Arc::clone(&context_root);
            async move {
                let case = EvalCase::new(
                    "cleanup/cancelled",
                    "Caller can cancel while a tool or shutdown is pending",
                )
                .step(EvalStep::Prompt("Reserve ORD-42".into()));
                runner
                    .run(&case, "cancelled", 1, |context| async move {
                        *context_root.lock().unwrap() = Some(context.root);
                        Ok(PreparedEvalTarget::new(manager, "scripted", "test"))
                    })
                    .await
            }
        });
        if cancel_during_tool {
            wait_for_signal(&probe.tool_started).await;
        } else {
            wait_for_signal(&probe.shutdown_started).await;
        }
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        if cancel_during_tool {
            wait_for_signal(&probe.shutdown_started).await;
            assert!(probe.tool_cleaned_with_workspace.load(Ordering::SeqCst));
        }
        let isolated_root = context_root.lock().unwrap().clone().unwrap();
        assert!(isolated_root.is_dir());
        assert_eq!(probe.shutdowns.load(Ordering::SeqCst), 1);
        probe.release_shutdown.notify_one();
        wait_for_signal(&probe.shutdown_completed).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while isolated_root.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_manager_closed(&manager).await;
    }
}
