//! Automatic replacement must obey the same admission/queue/cancellation boundary
//! as a normal run, without adopting explicit reload's permission to abort work.
use pi_agent::AgentOptions;
use pi_core::{Message, ModelId, PluginId, ProviderId, UserMessage};
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Plugin, PluginError,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, CreateSessionOptions, MultiSessionManager, PreparedSessionGeneration,
    SessionError, SessionGenerationRequest,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, Semaphore};

struct Gate {
    entered: Notify,
    release: Semaphore,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
        }
    }
}
impl Gate {
    async fn pause(&self) {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
    }
    fn resume(&self) {
        self.release.add_permits(1);
    }
}
#[derive(Default)]
struct Control {
    revision: AtomicUsize,
    builds: AtomicUsize,
    pause_reload: AtomicBool,
    pause_input: AtomicBool,
    reload: Gate,
    input: Gate,
    providers: Mutex<Vec<Arc<ScriptedProvider>>>,
}
struct PauseInput(Arc<Control>);
#[pi_plugin::plugin]
impl Plugin for PauseInput {
    fn id(&self) -> PluginId {
        PluginId::new("pause-input")
    }
    async fn before_agent_start(
        &self,
        _: AgentPluginContext,
        _: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        if self.0.pause_input.swap(false, Ordering::SeqCst) {
            self.0.input.pause().await;
        }
        Ok(BeforeAgentStartPatch::default())
    }
}
fn manager(control: Arc<Control>) -> MultiSessionManager {
    MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let control = control.clone();
        async move {
            let index = control.builds.fetch_add(1, Ordering::SeqCst);
            if index > 0 && control.pause_reload.swap(false, Ordering::SeqCst) {
                control.reload.pause().await;
            }
            let baseline = control.revision.load(Ordering::SeqCst);
            let provider = ScriptedProviderPlugin::scripted(
                (0..4).map(|_| ScriptedTurn::Text(format!("revision {baseline}"))),
            );
            control.providers.lock().unwrap().push(provider.provider());
            let runtime = PiRuntime::builder()
                .provider_plugin(provider)
                .plugin(PauseInput(control.clone()))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd: request.cwd,
                    ..AgentOptions::default()
                })
                .build()?;
            Ok(PreparedSessionGeneration::new(
                runtime,
                AgentSessionOptions::default().generation_freshness(Arc::new(move || {
                    Ok(control.revision.load(Ordering::SeqCst) != baseline)
                })),
            ))
        }
    })
}
async fn assert_pending<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|context| {
        assert!(future.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

fn user(text: &str) -> Vec<Message> {
    vec![Message::User(UserMessage::text(text, 1))]
}

#[tokio::test]
async fn abort_while_preparing_a_refresh_never_submits_the_cancelled_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("draft.jsonl");
    let control = Arc::new(Control::default());
    let manager = manager(control.clone());
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    control.revision.store(1, Ordering::SeqCst);
    control.pause_reload.store(true, Ordering::SeqCst);
    let before = session.current();
    let prompt = tokio::spawn(async move { before.prompt("cancel this input").await });
    control.reload.entered.notified().await;
    session.abort();
    control.reload.resume();
    assert!(matches!(
        prompt.await.unwrap(),
        Err(SessionError::Cancelled(_))
    ));
    assert!(
        control
            .providers
            .lock()
            .unwrap()
            .iter()
            .all(|provider| provider.requests().is_empty())
    );
    assert!(!path.exists());
    session
        .current()
        .prompt("retry after cancel")
        .await
        .unwrap();
    assert_eq!(control.builds.load(Ordering::SeqCst), 2);
    let journal = std::fs::read_to_string(path).unwrap();
    assert!(!journal.contains("cancel this input"));
    assert!(journal.contains("retry after cancel"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_submitters_share_one_fresh_candidate_and_are_each_admitted_once() {
    let directory = tempfile::tempdir().unwrap();
    let control = Arc::new(Control::default());
    let manager = manager(control.clone());
    let session = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("draft.jsonl"),
        ))
        .await
        .unwrap();
    let before = session.current();
    control.revision.store(1, Ordering::SeqCst);
    control.pause_reload.store(true, Ordering::SeqCst);
    let first_session = before.clone();
    let first = tokio::spawn(async move { first_session.run_messages(user("first")).await });
    control.reload.entered.notified().await;
    let mut second = Box::pin(before.run_messages(user("second")));
    assert_pending(second.as_mut()).await;
    control.reload.resume();
    let (first, second) = tokio::join!(first, second);
    first.unwrap().unwrap();
    second.unwrap();
    assert_eq!(control.builds.load(Ordering::SeqCst), 2);
    {
        let providers = control.providers.lock().unwrap();
        assert!(providers[0].requests().is_empty());
        let requests = providers[1].requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1]
                .messages
                .iter()
                .filter(|message| matches!(message, Message::User(_)))
                .count(),
            2
        );
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_waiting_submission_rechecks_freshness_after_the_active_input_without_aborting_it() {
    let directory = tempfile::tempdir().unwrap();
    let control = Arc::new(Control::default());
    let manager = manager(control.clone());
    let session = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("draft.jsonl"),
        ))
        .await
        .unwrap();
    let before = session.current();
    control.pause_input.store(true, Ordering::SeqCst);
    let first_session = before.clone();
    let first = tokio::spawn(async move { first_session.prompt("already admitted").await });
    control.input.entered.notified().await;
    control.revision.store(1, Ordering::SeqCst);
    let mut second = Box::pin(before.run_messages(user("waiting input")));
    assert_pending(second.as_mut()).await;
    assert_eq!(
        control.builds.load(Ordering::SeqCst),
        1,
        "do not refresh/abort a busy generation"
    );
    control.input.resume();
    first.await.unwrap().unwrap();
    second.await.unwrap();
    assert_eq!(control.builds.load(Ordering::SeqCst), 2);
    {
        let providers = control.providers.lock().unwrap();
        assert_eq!(providers[0].requests().len(), 1);
        assert_eq!(providers[1].requests().len(), 1);
        let history = serde_json::to_string(&providers[1].requests()[0].messages).unwrap();
        assert!(
            history.contains("revision 0"),
            "the previous input must finish normally"
        );
        assert!(history.contains("waiting input"));
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_inputs_keep_their_order_across_freshness_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let control = Arc::new(Control::default());
    let manager = manager(control.clone());
    let session = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("draft.jsonl"),
        ))
        .await
        .unwrap();
    let before = session.current();
    before.queue_next_run("first queued").await.unwrap();
    before.queue_next_run("second queued").await.unwrap();
    control.revision.store(1, Ordering::SeqCst);
    before.run_next_queued().await.unwrap();
    session.current().run_next_queued().await.unwrap();
    assert!(session.current().snapshot().queue.items.is_empty());
    let provider = control.providers.lock().unwrap()[1].clone();
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let first = serde_json::to_string(&requests[0].messages).unwrap();
    let second = serde_json::to_string(&requests[1].messages).unwrap();
    assert!(first.contains("first queued"));
    assert!(!first.contains("second queued"));
    assert!(second.find("first queued").unwrap() < second.find("second queued").unwrap());
    manager.shutdown().await.unwrap();
}
