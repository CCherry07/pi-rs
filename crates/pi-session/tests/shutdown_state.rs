use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_core::{ModelId, PluginContext, PluginId, PresentationMode, ProviderId, Usage};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntime, AgentSessionRuntimeFactory,
    AgentSessionRuntimeRequest, AgentSessionRuntimeTarget, LaneRecordEntry, PiPluginContext,
    PluginContextBinding, PreparedAgentSession, SessionError, SessionLog, SessionPlugin,
    SessionPluginContext, SessionPluginError, SessionPlugins, SessionShutdownEvent,
    SessionStartEvent,
};
use pi_test_support::ScriptedProviderPlugin;
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct CheckpointFactory {
    checkpoint: bool,
    starts: Arc<Mutex<Vec<Vec<Value>>>>,
    pause_shutdown: Option<Arc<tokio::sync::Notify>>,
}

struct CheckpointPlugin(CheckpointFactory);

#[pi_session::session_plugin]
impl SessionPlugin for CheckpointPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("checkpoint-fixture")
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> Result<(), SessionPluginError> {
        self.0
            .starts
            .lock()
            .unwrap()
            .push(context.session.entries()?);
        Ok(())
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), SessionPluginError> {
        if self.0.checkpoint {
            context
                .session
                .append_entry("checkpoint-fixture", Some(json!({"saved": true})))?;
        }
        context.session.record_usage(
            Usage {
                input: 7,
                total_tokens: 7,
                ..Usage::default()
            },
            Some(json!({"source": "shutdown-fixture"})),
        )?;
        if let Some(entered) = &self.0.pause_shutdown {
            entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl AgentSessionRuntimeFactory for CheckpointFactory {
    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError> {
        let cwd = match &request.target {
            AgentSessionRuntimeTarget::Create { cwd, .. } => cwd.clone(),
            AgentSessionRuntimeTarget::Open { path } => SessionLog::open(path)?.1.header.cwd,
            AgentSessionRuntimeTarget::Reuse { log } => log.header().cwd,
        };
        let access = Arc::new(PiPluginContext::new(
            PresentationMode::Print,
            true,
            PluginContextBinding::new(),
        ));
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_options(AgentOptions {
                cwd,
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .plugin_context(access.clone() as Arc<dyn PluginContext>)
            .build()?;
        let options = AgentSessionOptions::default()
            .plugins(SessionPlugins::new().plugin(CheckpointPlugin(self.clone())));
        let prepared = match request.target {
            AgentSessionRuntimeTarget::Create { path, .. } => {
                AgentSession::prepare_create_with_options(runtime, path, options).await?
            }
            AgentSessionRuntimeTarget::Open { path } => {
                AgentSession::prepare_open_with_options(runtime, path, options).await?
            }
            AgentSessionRuntimeTarget::Reuse { log } => {
                AgentSession::prepare_reuse_with_options(runtime, log, options).await?
            }
        };
        access.bind_generation_session(prepared.session());
        Ok(prepared)
    }
}

#[tokio::test]
async fn shutdown_checkpoint_is_available_when_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let factory = CheckpointFactory {
        checkpoint: true,
        ..CheckpointFactory::default()
    };
    let runtime = AgentSessionRuntime::create(
        factory.clone(),
        AgentSessionRuntimeTarget::create(directory.path(), &path),
    )
    .await
    .unwrap();
    runtime.session().log().materialize().unwrap();
    runtime.shutdown().await.unwrap();
    let reopened =
        AgentSessionRuntime::create(factory.clone(), AgentSessionRuntimeTarget::open(&path))
            .await
            .unwrap();
    assert!(factory.starts.lock().unwrap()[1].iter().any(|entry| {
        entry["customType"] == "checkpoint-fixture" && entry["data"]["saved"] == true
    }));
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_retains_shutdown_records_and_continuous_log_sequences() {
    for materialized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = AgentSessionRuntime::create(
            CheckpointFactory::default(),
            AgentSessionRuntimeTarget::create(directory.path(), &path),
        )
        .await
        .unwrap();
        if materialized {
            runtime.session().log().materialize().unwrap();
        }
        runtime.reload().await.unwrap();
        let current = runtime.session();
        assert_eq!(
            current
                .log()
                .load()
                .unwrap()
                .records
                .iter()
                .filter(|record| { matches!(record.record, LaneRecordEntry::Usage(_)) })
                .count(),
            1,
            "the replacement must include final plugin writes (materialized={materialized})",
        );
        current.append_custom_entry("after-reload", None).unwrap();
        assert_eq!(path.exists(), materialized);
        if materialized {
            SessionLog::open(&path).expect("reload must not reuse an already written sequence");
        }
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reload_start_sees_the_previous_plugins_shutdown_checkpoint() {
    for materialized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let factory = CheckpointFactory {
            checkpoint: true,
            ..CheckpointFactory::default()
        };
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &path),
        )
        .await
        .unwrap();
        if materialized {
            runtime.session().log().materialize().unwrap();
        }
        runtime.reload().await.unwrap();
        assert!(
            factory.starts.lock().unwrap()[1]
                .iter()
                .any(|entry| { entry["customType"] == "checkpoint-fixture" }),
            "new plugin must read previous checkpoint (materialized={materialized})"
        );
        assert_eq!(path.exists(), materialized);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_shutdown_retires_the_plugin_context_after_its_final_write() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let runtime = AgentSessionRuntime::create(
        CheckpointFactory {
            checkpoint: true,
            pause_shutdown: Some(entered.clone()),
            ..CheckpointFactory::default()
        },
        AgentSessionRuntimeTarget::create(directory.path(), directory.path().join("session.jsonl")),
    )
    .await
    .unwrap();
    let session = runtime.session();
    let retained = pi_core::CommandContextParts::new(
        session
            .runtime()
            .plugin_context_handle(pi_core::PluginContextScope::Command),
    );
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(session.append_custom_entry("external-write", None).is_err());
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    assert!(session.is_closed());
    assert!(
        retained
            .session
            .append_entry("retired-write", None)
            .is_err()
    );
    session.shutdown().await;
    let entries = session.log().load().unwrap().entries;
    assert_eq!(entries.iter().filter(|entry| matches!(
        &entry.entry,
        pi_session::SessionEntry::Custom(custom) if custom.custom_type == "checkpoint-fixture"
    )).count(), 1);
}

#[tokio::test]
async fn resuming_the_current_file_keeps_shutdown_writes_and_sequence_ownership() {
    for alias in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let factory = CheckpointFactory {
            checkpoint: true,
            ..CheckpointFactory::default()
        };
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &path),
        )
        .await
        .unwrap();
        runtime.session().log().materialize().unwrap();
        let target = if alias {
            directory.path().join(".").join("session.jsonl")
        } else {
            path.clone()
        };
        runtime.switch_session(target).await.unwrap();
        assert!(
            factory.starts.lock().unwrap()[1]
                .iter()
                .any(|entry| { entry["customType"] == "checkpoint-fixture" }),
            "same-file resume must include shutdown checkpoint (alias={alias})"
        );
        runtime
            .session()
            .append_custom_entry("after-resume", None)
            .unwrap();
        SessionLog::open(&path).unwrap();
        runtime.shutdown().await.unwrap();
    }
}
