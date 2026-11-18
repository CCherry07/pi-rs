//! Product hooks execute at the session mutation boundary used by RPC and embedded clients.

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use pi_agent::AgentOptions;
use pi_core::{ModelId, ModelSpec, PluginId, ProviderId, ThinkingLevel, WorkspaceSpec};
use pi_plugin::{
    ModelSelectEvent, ModelSelectSource, Plugin, PluginError, ProviderPlugin,
    ProviderRegisterContext, SessionPluginContext, ThinkingLevelSelectEvent, UserBashEvent,
    UserBashOperations, UserBashOverride, UserBashRequest, UserBashResult,
};
use pi_runtime::{PiRuntime, PiRuntimeBuilder};
use pi_session::{AgentSession, AgentSessionEvent, AgentSessionOptions, ShellExecutionOptions};
use pi_test_support::ScriptedProviderPlugin;

fn runtime_builder(cwd: &Path) -> PiRuntimeBuilder {
    PiRuntime::builder()
        .workspace(WorkspaceSpec::from_cwd(cwd))
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
}

enum BashBehavior {
    Pass,
    Fail,
    Result(UserBashResult),
    Operations(Arc<dyn UserBashOperations>),
}

struct BashPlugin {
    name: &'static str,
    behavior: BashBehavior,
    calls: Arc<Mutex<Vec<(String, UserBashEvent)>>>,
}

#[pi_plugin::plugin]
impl Plugin for BashPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(self.name)
    }

    async fn user_bash(
        &self,
        _context: &SessionPluginContext,
        event: &UserBashEvent,
    ) -> Result<Option<UserBashOverride>, PluginError> {
        self.calls
            .lock()
            .unwrap()
            .push((self.name.into(), event.clone()));
        match &self.behavior {
            BashBehavior::Pass => Ok(None),
            BashBehavior::Fail => Err(PluginError::Failure("remote backend unavailable".into())),
            BashBehavior::Result(result) => Ok(Some(UserBashOverride::Result(result.clone()))),
            BashBehavior::Operations(operations) => {
                Ok(Some(UserBashOverride::Operations(operations.clone())))
            }
        }
    }
}

#[tokio::test]
async fn user_bash_first_result_is_recorded_once_without_local_executor_or_materialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let complete = UserBashResult {
        output: "remote output".into(),
        exit_code: Some(7),
        cancelled: false,
        truncated: true,
        full_output_path: Some(directory.path().join("remote-output")),
    };
    let mut builder = runtime_builder(directory.path());
    for (name, behavior) in [
        ("pass", BashBehavior::Pass),
        ("complete", BashBehavior::Result(complete.clone())),
        ("later", BashBehavior::Fail),
    ] {
        builder = builder.plugin(BashPlugin {
            name,
            behavior,
            calls: calls.clone(),
        });
    }
    let session = AgentSession::create(
        builder.build().unwrap(),
        &path,
        AgentSessionOptions::default(),
    )
    .await
    .unwrap();
    let result = session
        .execute_shell(
            "remote command",
            ShellExecutionOptions {
                exclude_from_context: true,
                ..ShellExecutionOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result.output, complete.output);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.full_output_path, complete.full_output_path);
    assert!(result.truncated);
    assert!(!path.exists());
    assert!(session.snapshot().bash.is_none());
    assert!(session.snapshot().agent.messages.is_empty());
    let document = session.history().shared_document().unwrap();
    let messages = document.messages();
    assert_eq!(messages.len(), 1);
    let wire = serde_json::to_value(&messages[0]).unwrap();
    assert_eq!(wire["role"], "bashExecution");
    assert_eq!(wire["command"], "remote command");
    assert_eq!(wire["output"], "remote output");
    assert_eq!(wire["excludeFromContext"], true);
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        ["pass", "complete"]
    );
    assert_eq!(calls[1].1.cwd, directory.path());
    assert!(calls[1].1.exclude_from_context);
}

#[tokio::test]
async fn user_bash_failure_stops_later_hooks_and_leaves_no_history() {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut builder = runtime_builder(directory.path());
    for name in ["failing", "later"] {
        builder = builder.plugin(BashPlugin {
            name,
            behavior: BashBehavior::Fail,
            calls: calls.clone(),
        });
    }
    let session = AgentSession::create(
        builder.build().unwrap(),
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let error = session
        .execute_shell("touch should-not-run", Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("remote backend unavailable"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        session
            .history()
            .shared_document()
            .unwrap()
            .messages()
            .is_empty()
    );
    assert!(session.snapshot().bash.is_none());
    assert!(
        session
            .catalog()
            .plugin_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.hook.as_str() == "user_bash")
    );
}

struct StreamingOperations {
    wait_for_abort: bool,
}

#[async_trait::async_trait]
impl UserBashOperations for StreamingOperations {
    async fn execute(&self, request: UserBashRequest) -> Result<Option<i32>, PluginError> {
        assert_eq!(request.command, "remote command");
        (request.on_data)("远程 output\n".as_bytes().to_vec());
        if self.wait_for_abort {
            request.signal.wait().await;
            return Err(PluginError::Failure("operation aborted".into()));
        }
        Ok(Some(23))
    }
}

#[tokio::test]
async fn user_bash_operations_stream_and_cancel_through_the_session() {
    for wait_for_abort in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let session = AgentSession::create(
            runtime_builder(directory.path())
                .plugin(BashPlugin {
                    name: "remote",
                    behavior: BashBehavior::Operations(Arc::new(StreamingOperations {
                        wait_for_abort,
                    })),
                    calls: Arc::new(Mutex::new(Vec::new())),
                })
                .build()
                .unwrap(),
            directory.path().join("session.jsonl"),
            Default::default(),
        )
        .await
        .unwrap();
        let mut events = session.subscribe().events;
        let execution = {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .execute_shell(
                        "remote command",
                        ShellExecutionOptions {
                            id: Some("rpc-request".into()),
                            ..Default::default()
                        },
                    )
                    .await
            })
        };
        let delta = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let AgentSessionEvent::BashExecutionUpdate {
                    delta, request_id, ..
                } = events.recv().await.unwrap().event
                {
                    assert_eq!(request_id.as_deref(), Some("rpc-request"));
                    break delta;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(delta, "远程 output\n");
        if wait_for_abort {
            session.abort_shell();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.output, delta);
        assert_eq!(result.cancelled, wait_for_abort);
        assert_eq!(
            result.exit_code,
            if wait_for_abort { None } else { Some(23) }
        );
        assert_eq!(
            session
                .history()
                .shared_document()
                .unwrap()
                .messages()
                .len(),
            1
        );
        assert!(session.snapshot().bash.is_none());
    }
}

struct ModelCatalog;

#[pi_plugin::provider_plugin]
impl ProviderPlugin for ModelCatalog {
    fn id(&self) -> PluginId {
        PluginId::new("model-catalog")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        let mut reasoning = ModelSpec::new("scripted", "reasoning", "Reasoning", "scripted");
        reasoning.reasoning = true;
        context.register_model(reasoning)?;
        context.register_model(ModelSpec::new("scripted", "plain", "Plain", "scripted"))
    }
}

struct ModelObserver {
    session: Arc<Mutex<Weak<AgentSession>>>,
    events: Arc<Mutex<Vec<ModelSelectEvent>>>,
}

#[pi_plugin::plugin]
impl Plugin for ModelObserver {
    fn id(&self) -> PluginId {
        PluginId::new("model-observer")
    }

    async fn model_select(
        &self,
        _context: &SessionPluginContext,
        event: &ModelSelectEvent,
    ) -> Result<(), PluginError> {
        let session = self.session.lock().unwrap().upgrade().unwrap();
        let state = session.snapshot().agent;
        assert_eq!(state.provider_id, event.model.provider);
        assert_eq!(state.model_id, event.model.id);
        if !event.model.reasoning {
            assert_eq!(state.thinking_level, ThinkingLevel::Off);
        }
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn model_select_observes_committed_state_and_distinguishes_set_from_cycle() {
    let directory = tempfile::tempdir().unwrap();
    let retained = Arc::new(Mutex::new(Weak::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let runtime = runtime_builder(directory.path())
        .provider_plugin(ModelCatalog)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("reasoning"),
            thinking_level: ThinkingLevel::High,
            ..Default::default()
        })
        .plugin(ModelObserver {
            session: retained.clone(),
            events: events.clone(),
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    *retained.lock().unwrap() = Arc::downgrade(&session);
    session
        .set_model_with_hooks("scripted".into(), "reasoning".into())
        .await
        .unwrap();
    assert!(events.lock().unwrap().is_empty());
    session
        .set_model_with_hooks("scripted".into(), "plain".into())
        .await
        .unwrap();
    let cycled = session.cycle_model_with_hooks().await.unwrap().unwrap();
    assert!(
        session
            .set_model_with_hooks("missing".into(), "plain".into())
            .await
            .is_err()
    );
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].source, ModelSelectSource::Set);
    assert_eq!(events[0].model.id.as_str(), "plain");
    assert_eq!(
        events[0].previous_model.as_ref().unwrap().id.as_str(),
        "reasoning"
    );
    assert_eq!(events[1].source, ModelSelectSource::Cycle);
    assert_eq!(events[1].model, cycled.model);
    assert_eq!(
        events[1].previous_model.as_ref().unwrap().id.as_str(),
        "plain"
    );
}

struct ThinkingObserver {
    started: tokio::sync::mpsc::UnboundedSender<ThinkingLevelSelectEvent>,
    release: Arc<tokio::sync::Notify>,
}

#[pi_plugin::plugin]
impl Plugin for ThinkingObserver {
    fn id(&self) -> PluginId {
        PluginId::new("thinking-observer")
    }

    async fn thinking_level_select(
        &self,
        _context: &SessionPluginContext,
        event: &ThinkingLevelSelectEvent,
    ) -> Result<(), PluginError> {
        self.started.send(*event).unwrap();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn thinking_level_observers_do_not_block_sync_setters_and_include_model_clamping() {
    let directory = tempfile::tempdir().unwrap();
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime_builder(directory.path())
        .provider_plugin(ModelCatalog)
        .agent_options(AgentOptions {
            provider_id: "scripted".into(),
            model_id: "reasoning".into(),
            thinking_level: ThinkingLevel::High,
            ..Default::default()
        })
        .plugin(ThinkingObserver {
            started,
            release: release.clone(),
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    // Native notifications can call synchronous capabilities from a thread without a Tokio runtime.
    let setter = session.clone();
    std::thread::spawn(move || setter.set_thinking_level(ThinkingLevel::Low).unwrap())
        .join()
        .unwrap();
    assert_eq!(session.snapshot().agent.thinking_level, ThinkingLevel::Low);
    let event = tokio::time::timeout(Duration::from_secs(2), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        event,
        ThinkingLevelSelectEvent {
            level: ThinkingLevel::Low,
            previous_level: ThinkingLevel::High
        }
    );
    session.set_thinking_level(ThinkingLevel::Low).unwrap();
    session
        .set_model_with_hooks("scripted".into(), "plain".into())
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        event,
        ThinkingLevelSelectEvent {
            level: ThinkingLevel::Off,
            previous_level: ThinkingLevel::Low
        }
    );
    assert!(observed.try_recv().is_err());
    release.notify_waiters();
}
