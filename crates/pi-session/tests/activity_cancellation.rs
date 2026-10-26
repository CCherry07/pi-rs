use std::sync::Arc;
use std::time::Duration;

use pi_agent::AgentLoopStop;
use pi_core::PluginId;
use pi_plugin::Plugin;
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, CompactionEntry, CompactionSettings, PluginError,
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionCompactEvent,
    SessionPluginContext,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tokio::sync::Notify;

struct BlockingCompactionNotification {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[pi_plugin::plugin]
impl Plugin for BlockingCompactionNotification {
    fn id(&self) -> PluginId {
        PluginId::new("blocking-compaction-notification")
    }

    async fn session_before_compact(
        &self,
        _: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, PluginError> {
        Ok(Some(SessionBeforeCompactResult {
            cancel: false,
            compaction: Some(CompactionEntry {
                retained_entry_ids: Vec::new(),
                summary: "compacted history".into(),
                retained_tail: Vec::new(),
                tokens_before: event.preparation.tokens_before,
                details: None,
                usage: None,
            }),
        }))
    }

    async fn session_compact(
        &self,
        _: &SessionPluginContext,
        _: &SessionCompactEvent,
    ) -> Result<(), PluginError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn abort_during_compaction_notification_prevents_the_overflow_retry() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Error("context window token length exceeded".into()),
        ScriptedTurn::Text("unexpected retry".into()),
    ]);
    let requests = provider.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider)
        .plugin(BlockingCompactionNotification {
            entered: entered.clone(),
            release: release.clone(),
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        }),
    )
    .await
    .unwrap();
    let running = session.clone();
    let run = tokio::spawn(async move { running.prompt("recover this request").await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(!session.is_idle());
    session.abort();
    release.notify_one();
    let pi_session::PromptOutcome::Agent(outcome) =
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    else {
        panic!("expected an agent run");
    };
    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    assert_eq!(requests.requests().len(), 1);
    assert!(session.is_idle());
}
