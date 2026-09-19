use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_core::{ModelId, PluginId, ProviderId, Usage};
use pi_plugin::Plugin;
use pi_plugin::{PluginContext, PresentationMode};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, LaneRecordEntry, MultiSessionManager, PiPluginContext,
    PluginContextBinding, PluginError, PreparedSessionGeneration, SessionError,
    SessionGenerationFactory, SessionGenerationRequest, SessionLog, SessionPluginContext,
    SessionShutdownEvent, SessionStartEvent,
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

#[pi_plugin::plugin]
impl Plugin for CheckpointPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("checkpoint-fixture")
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> Result<(), PluginError> {
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
    ) -> Result<(), PluginError> {
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
impl SessionGenerationFactory for CheckpointFactory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        let access = Arc::new(PiPluginContext::new(
            PresentationMode::Print,
            true,
            PluginContextBinding::new(),
        ));
        let runtime = PiRuntime::builder()
            .plugin(CheckpointPlugin(self.clone()))
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_options(AgentOptions {
                cwd: request.cwd,
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .plugin_context(access.clone() as Arc<dyn PluginContext>)
            .build()?;
        let options = AgentSessionOptions::default();
        Ok(PreparedSessionGeneration::new(runtime, options)
            .bind_session(move |session| access.bind_generation_session(session)))
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
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(directory.path(), &path)
        .await
        .unwrap();
    session.current().log().materialize().unwrap();
    manager.close_session(&session).await.unwrap();
    let _reopened = manager.open_session(&path).await.unwrap();
    assert!(factory.starts.lock().unwrap()[1].iter().any(|entry| {
        entry["customType"] == "checkpoint-fixture" && entry["data"]["saved"] == true
    }));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_retains_shutdown_records_and_continuous_log_sequences() {
    for materialized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let manager = MultiSessionManager::new(CheckpointFactory::default());
        let session = manager
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        if materialized {
            session.current().log().materialize().unwrap();
        }
        session.reload().await.unwrap();
        let current = session.current();
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
        manager.shutdown().await.unwrap();
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
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        if materialized {
            session.current().log().materialize().unwrap();
        }
        session.reload().await.unwrap();
        assert!(
            factory.starts.lock().unwrap()[1]
                .iter()
                .any(|entry| { entry["customType"] == "checkpoint-fixture" }),
            "new plugin must read previous checkpoint (materialized={materialized})"
        );
        assert_eq!(path.exists(), materialized);
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_shutdown_retires_the_plugin_context_after_its_final_write() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let manager = MultiSessionManager::new(CheckpointFactory {
        checkpoint: true,
        pause_shutdown: Some(entered.clone()),
        ..CheckpointFactory::default()
    });
    let owner = manager
        .create_session(directory.path(), directory.path().join("session.jsonl"))
        .await
        .unwrap();
    let session = owner.current();
    let retained = pi_plugin::CommandContextParts::new(
        session
            .runtime()
            .plugin_context_handle(pi_plugin::PluginContextScope::Command),
    );
    let shutdown = tokio::spawn({
        let manager = manager.clone();
        let owner = owner.clone();
        async move { manager.close_session(&owner).await }
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
    manager.shutdown().await.unwrap();
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
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        session.current().log().materialize().unwrap();
        let target = if alias {
            directory.path().join(".").join("session.jsonl")
        } else {
            path.clone()
        };
        session.resume_session(target).await.unwrap();
        assert!(
            factory.starts.lock().unwrap()[1]
                .iter()
                .any(|entry| { entry["customType"] == "checkpoint-fixture" }),
            "same-file resume must include shutdown checkpoint (alias={alias})"
        );
        session
            .current()
            .append_custom_entry("after-resume", None)
            .unwrap();
        SessionLog::open(&path).unwrap();
        manager.shutdown().await.unwrap();
    }
}
