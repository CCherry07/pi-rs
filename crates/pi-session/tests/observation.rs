use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pi_agent::AgentOptions;
use pi_core::PluginId;
use pi_plugin::{Plugin, PluginError, SessionPluginContext};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionEvent, AgentSessionOptions, AgentSessionReplacement, CreateSessionOptions,
    MultiSessionManager, NoticeLevel, PreparedSessionGeneration, SessionBeforeSwitchEvent,
    SessionBeforeSwitchResult, SessionError, SessionGenerationFactory, SessionGenerationRequest,
    SessionObservation, SessionResetReason, SessionStartEvent, SessionStartReason, SessionUpdate,
};
use pi_test_support::ScriptedProviderPlugin;
use tokio::sync::Notify;

#[derive(Default)]
struct Fixture {
    fail: AtomicBool,
    cancel: AtomicBool,
    block_reload_start: AtomicBool,
    start_entered: Notify,
    start_release: Notify,
}

struct Factory(Arc<Fixture>);
struct Lifecycle(Arc<Fixture>);

#[async_trait::async_trait]
impl SessionGenerationFactory for Factory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        if self.0.fail.load(Ordering::Acquire) {
            return Err(SessionError::Runtime("preparation failed".into()));
        }
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .plugin(Lifecycle(Arc::clone(&self.0)))
            .agent_options(AgentOptions {
                cwd: request.cwd,
                ..AgentOptions::default()
            })
            .build()?;
        Ok(PreparedSessionGeneration::new(
            runtime,
            AgentSessionOptions::default(),
        ))
    }
}

#[pi_plugin::plugin]
impl Plugin for Lifecycle {
    fn id(&self) -> PluginId {
        PluginId::new("observation-lifecycle")
    }

    async fn session_before_switch(
        &self,
        _: &SessionPluginContext,
        _: &SessionBeforeSwitchEvent,
    ) -> Result<Option<SessionBeforeSwitchResult>, PluginError> {
        Ok(Some(SessionBeforeSwitchResult {
            cancel: self.0.cancel.load(Ordering::Acquire),
        }))
    }

    async fn session_start(
        &self,
        _: &SessionPluginContext,
        event: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        if event.reason == SessionStartReason::Reload
            && self.0.block_reload_start.load(Ordering::Acquire)
        {
            self.0.start_entered.notify_one();
            self.0.start_release.notified().await;
        }
        Ok(())
    }
}

async fn next(observation: &mut SessionObservation) -> SessionUpdate {
    tokio::time::timeout(Duration::from_secs(2), observation.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn replacements_coalesce_and_never_relabel_old_generation_events() {
    let directory = tempfile::tempdir().unwrap();
    let manager = MultiSessionManager::new(Factory(Arc::default()));
    let handle = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("first.jsonl"),
        ))
        .await
        .unwrap();
    let mut observation = handle.observe();
    let first = observation.current();
    let first_id = handle.id();
    first.notify_plugin("old event".into(), NoticeLevel::Info);
    handle.reload().await.unwrap();
    handle.reload().await.unwrap();

    let SessionUpdate::Reset {
        version,
        reason,
        snapshot,
    } = next(&mut observation).await
    else {
        panic!("replacement must precede buffered old events");
    };
    assert_eq!(reason, SessionResetReason::Replacement);
    assert_eq!(version.epoch, 2);
    assert_eq!(version.revision, snapshot.revision);
    assert_eq!(handle.id(), first_id);
    assert!(!Arc::ptr_eq(&first, &observation.current()));
    assert!(Arc::ptr_eq(&handle.current(), &observation.current()));

    observation
        .current()
        .notify_plugin("new event".into(), NoticeLevel::Info);
    let SessionUpdate::Event { version, event } = next(&mut observation).await else {
        panic!("expected event")
    };
    assert_eq!(version.epoch, 2);
    assert!(
        matches!(event, AgentSessionEvent::PluginNotice { message, .. } if message == "new event")
    );

    handle
        .new_session(directory.path(), directory.path().join("second.jsonl"))
        .await
        .unwrap();
    assert!(matches!(
        next(&mut observation).await,
        SessionUpdate::Reset {
            version: pi_session::SessionVersion { epoch: 3, .. },
            reason: SessionResetReason::Replacement,
            ..
        }
    ));
    assert_ne!(observation.current().history().header().id, first_id);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_and_cancelled_replacements_keep_the_current_epoch_and_stream() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Arc::new(Fixture::default());
    let manager = MultiSessionManager::new(Factory(Arc::clone(&fixture)));
    let handle = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("first.jsonl"),
        ))
        .await
        .unwrap();
    let mut observation = handle.observe();
    let first = observation.current();
    fixture.fail.store(true, Ordering::Release);
    assert!(handle.reload().await.is_err());
    fixture.fail.store(false, Ordering::Release);
    fixture.cancel.store(true, Ordering::Release);
    assert_eq!(
        handle
            .new_session(directory.path(), directory.path().join("cancelled.jsonl"))
            .await
            .unwrap(),
        AgentSessionReplacement::Cancelled
    );
    first.notify_plugin("still active".into(), NoticeLevel::Info);
    assert!(
        matches!(next(&mut observation).await, SessionUpdate::Event {
        version: pi_session::SessionVersion { epoch: 0, .. },
        event: AgentSessionEvent::PluginNotice { message, .. }
    } if message == "still active")
    );
    assert!(Arc::ptr_eq(&observation.current(), &first));
    assert!(!first.is_closed());
    assert!(!directory.path().join("cancelled.jsonl").exists());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn retired_generation_during_reload_is_not_final_closure() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Arc::new(Fixture::default());
    let manager = MultiSessionManager::new(Factory(Arc::clone(&fixture)));
    let handle = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("session.jsonl"),
        ))
        .await
        .unwrap();
    let mut observation = handle.observe();
    fixture.block_reload_start.store(true, Ordering::Release);
    let reload = tokio::spawn({
        let handle = handle.clone();
        async move { handle.reload().await }
    });
    tokio::time::timeout(Duration::from_secs(2), fixture.start_entered.notified())
        .await
        .unwrap();
    assert!(observation.current().is_closed());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), observation.recv())
            .await
            .is_err()
    );
    fixture.start_release.notify_one();
    reload.await.unwrap().unwrap();
    assert!(matches!(
        next(&mut observation).await,
        SessionUpdate::Reset {
            reason: SessionResetReason::Replacement,
            ..
        }
    ));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn lag_resets_state_and_then_resumes_after_the_snapshot_revision() {
    let directory = tempfile::tempdir().unwrap();
    let manager = MultiSessionManager::new(Factory(Arc::default()));
    let handle = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("session.jsonl"),
        ))
        .await
        .unwrap();
    let mut observation = handle.observe();
    let session = handle.current();
    for index in 0..1024 {
        session.notify_plugin(format!("notice {index}"), NoticeLevel::Info);
    }
    session.set_name(Some("latest name".into())).await.unwrap();
    let SessionUpdate::Reset {
        version,
        reason: SessionResetReason::Lagged { missed_events },
        snapshot,
    } = next(&mut observation).await
    else {
        panic!("lag must explicitly reset the consumer");
    };
    assert!(missed_events > 0);
    assert_eq!(version.epoch, 0);
    assert_eq!(snapshot.name.as_deref(), Some("latest name"));
    assert_eq!(snapshot.revision, session.snapshot().revision);
    assert_eq!(version.revision, snapshot.revision);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), observation.recv())
            .await
            .is_err()
    );
    session.notify_plugin("after reset".into(), NoticeLevel::Info);
    let SessionUpdate::Event {
        version: next_version,
        event,
    } = next(&mut observation).await
    else {
        panic!("expected live event")
    };
    assert!(next_version.revision > version.revision);
    assert!(
        matches!(event, AgentSessionEvent::PluginNotice { message, .. } if message == "after reset")
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn close_drains_final_events_and_terminates_even_when_handles_are_retained() {
    let directory = tempfile::tempdir().unwrap();
    let manager = MultiSessionManager::new(Factory(Arc::default()));
    let handle = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("session.jsonl"),
        ))
        .await
        .unwrap();
    let mut observation = handle.observe();
    // Dropping a pending recv must leave the next event available.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), observation.recv())
            .await
            .is_err()
    );
    handle
        .current()
        .notify_plugin("final notice".into(), NoticeLevel::Info);
    manager.close_session(&handle).await.unwrap();
    assert!(
        matches!(next(&mut observation).await, SessionUpdate::Event { event: AgentSessionEvent::PluginNotice { message, .. }, .. } if message == "final notice")
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), observation.recv())
            .await
            .unwrap()
            .is_none()
    );
    assert!(handle.current().is_closed());
    assert!(handle.observe().recv().await.is_none());
    manager.shutdown().await.unwrap();
}
