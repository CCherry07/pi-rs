use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use pi_agent::AgentOptions;
use pi_core::{
    AgentPlugin, AgentPluginContext, AgentSettledEvent, BeforeAgentStartEvent,
    BeforeAgentStartPatch, CustomMessageContent, IsolatedSessionRequest, ModelId, PluginError,
    PluginId, ProviderId,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionRuntimeRequest, AgentSessionRuntimeTarget, MultiSessionManager,
    PiSession,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct LifecycleProbe {
    panic_on_start: bool,
    started: Arc<Notify>,
    settled: Arc<Notify>,
    settled_count: Arc<AtomicUsize>,
    release_settled: Arc<Notify>,
    settled_dropped: Arc<Notify>,
    finished: Arc<AtomicBool>,
}

struct NotifyOnDrop(Arc<Notify>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[derive(Clone)]
struct PrepareGate {
    started: Arc<AtomicUsize>,
    both_started: Arc<Notify>,
    release: Arc<Notify>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for LifecycleProbe {
    fn id(&self) -> PluginId {
        PluginId::new("isolated-lifecycle-probe")
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        _event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        self.started.notify_one();
        assert!(!self.panic_on_start, "isolated prompt panic fixture");
        Ok(BeforeAgentStartPatch::default())
    }

    async fn agent_settled(
        &self,
        _context: AgentPluginContext,
        _event: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        let _dropped = NotifyOnDrop(Arc::clone(&self.settled_dropped));
        self.settled_count.fetch_add(1, Ordering::SeqCst);
        self.settled.notify_one();
        self.release_settled.notified().await;
        self.finished.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn manager(probe: LifecycleProbe) -> MultiSessionManager {
    manager_with_prepare_gate(probe, None)
}

fn manager_with_prepare_gate(
    probe: LifecycleProbe,
    prepare_gate: Option<PrepareGate>,
) -> MultiSessionManager {
    MultiSessionManager::new(move |request: AgentSessionRuntimeRequest| {
        let probe = probe.clone();
        let prepare_gate = prepare_gate.clone();
        async move {
            let AgentSessionRuntimeTarget::Create { cwd, path, .. } = request.target else {
                panic!("lifecycle tests only create sessions");
            };
            if path
                .components()
                .any(|component| component.as_os_str() == "isolated")
                && let Some(gate) = prepare_gate
            {
                if gate.started.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                    gate.both_started.notify_one();
                }
                gate.release.notified().await;
            }
            let runtime = request
                .generation_overlay
                .apply_to(PiRuntime::builder())
                .provider_plugin(ScriptedProviderPlugin::scripted([
                    ScriptedTurn::WaitForAbort,
                ]))
                .agent_plugin(probe)
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd,
                    ..AgentOptions::default()
                })
                .build()?;
            if let Some(initial_state) = request.initial_state {
                initial_state.apply_to(&runtime)?;
            }
            AgentSession::prepare_create(runtime, path).await
        }
    })
}

fn request() -> IsolatedSessionRequest {
    IsolatedSessionRequest::new(CustomMessageContent::Text("isolated lifecycle test".into()))
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("isolated lifecycle must settle")
}

async fn owner(manager: &MultiSessionManager, directory: &tempfile::TempDir) -> PiSession {
    manager
        .create_session(directory.path(), directory.path().join("owner.jsonl"))
        .await
        .unwrap()
}

#[tokio::test]
async fn parallel_isolated_launches_prepare_concurrently() {
    let directory = tempfile::tempdir().unwrap();
    let gate = PrepareGate {
        started: Arc::new(AtomicUsize::new(0)),
        both_started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let probe = LifecycleProbe::default();
    let manager = manager_with_prepare_gate(probe.clone(), Some(gate.clone()));
    let owner = owner(&manager, &directory).await;
    let first = tokio::spawn({
        let owner = owner.clone();
        async move { owner.launch_isolated_session(request()).await }
    });
    let second = tokio::spawn({
        let owner = owner.clone();
        async move { owner.launch_isolated_session(request()).await }
    });

    tokio::time::timeout(Duration::from_secs(2), gate.both_started.notified())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "parallel prepares did not overlap; started={}",
                gate.started.load(Ordering::SeqCst)
            )
        });
    gate.release.notify_waiters();
    let first = bounded(first).await.unwrap().unwrap();
    let second = bounded(second).await.unwrap().unwrap();
    for id in [&first, &second] {
        owner.abort_isolated_session(id).unwrap();
    }
    bounded(async {
        while probe.settled_count.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    probe.release_settled.notify_waiters();
    for id in [&first, &second] {
        assert!(
            bounded(owner.wait_for_isolated_session(id))
                .await
                .unwrap()
                .aborted
        );
    }
    bounded(manager.shutdown()).await.unwrap();
}

#[tokio::test]
async fn isolated_prompt_panic_publishes_a_repeatable_terminal_failure() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(LifecycleProbe {
        panic_on_start: true,
        ..LifecycleProbe::default()
    });
    let owner = owner(&manager, &directory).await;
    let id = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    let first = bounded(owner.wait_for_isolated_session(&id))
        .await
        .unwrap_err();
    let second = bounded(owner.wait_for_isolated_session(&id))
        .await
        .unwrap_err();
    assert!(
        first
            .to_string()
            .contains("isolated session prompt panicked"),
        "{first}"
    );
    assert!(first.to_string().contains("isolated prompt panic fixture"));
    assert_eq!(first.to_string(), second.to_string());
    bounded(manager.shutdown()).await.unwrap();
}

#[tokio::test]
async fn cancelled_isolated_launch_drains_the_started_prompt_before_finishing() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let mut launch = Box::pin(owner.launch_isolated_session(request()));
    // On this current-thread runtime the spawned prompt cannot run until this
    // first poll returns. Keep launch pending even after its child starts.
    poll_fn(|context| {
        assert!(launch.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    bounded(probe.started.notified()).await;
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.id() != owner.id())
        .unwrap();
    drop(launch);
    bounded(probe.settled.notified()).await;
    assert!(!probe.finished.load(Ordering::SeqCst));
    probe.release_settled.notify_one();
    bounded(manager.shutdown()).await.unwrap();
    assert!(probe.finished.load(Ordering::SeqCst));
    assert!(!child.current().runtime().agent().is_running());
    assert!(child.current().is_closed());
}

#[tokio::test]
async fn isolated_abort_waits_for_prompt_cleanup_and_preserves_the_terminal_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let id = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    owner.abort_isolated_session(&id).unwrap();
    bounded(probe.settled.notified()).await;
    let mut wait = Box::pin(owner.wait_for_isolated_session(&id));
    poll_fn(|context| {
        assert!(wait.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    probe.release_settled.notify_one();
    assert!(bounded(wait).await.unwrap().aborted);
    assert!(probe.finished.load(Ordering::SeqCst));
    assert!(
        bounded(owner.wait_for_isolated_session(&id))
            .await
            .unwrap()
            .aborted
    );
    bounded(manager.shutdown()).await.unwrap();
}

#[tokio::test]
async fn isolated_shutdown_drains_cleanup_before_returning() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let id = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    let observation = owner.observe_isolated_session(&id).unwrap();
    let mut shutdown = Box::pin(manager.shutdown());
    tokio::select! {
        biased;
        result = &mut shutdown => panic!("shutdown skipped active prompt: {result:?}"),
        () = bounded(probe.settled.notified()) => {}
    }
    assert!(!probe.finished.load(Ordering::SeqCst));
    probe.release_settled.notify_one();
    bounded(shutdown).await.unwrap();
    assert!(probe.finished.load(Ordering::SeqCst));
    assert!(!observation.snapshot().agent.is_running);
    assert!(manager.sessions().is_empty());
}

#[tokio::test]
async fn isolated_shutdown_signals_every_child_before_draining_any_prompt() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let first = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    let second = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    let mut shutdown = Box::pin(manager.shutdown());
    tokio::select! {
        biased;
        result = &mut shutdown => panic!("shutdown skipped active prompts: {result:?}"),
        () = bounded(async {
            while probe.settled_count.load(Ordering::SeqCst) != 2 {
                tokio::task::yield_now().await;
            }
        }) => {}
    }
    probe.release_settled.notify_waiters();
    bounded(shutdown).await.unwrap();
    for id in [first, second] {
        assert!(
            bounded(owner.wait_for_isolated_session(&id))
                .await
                .unwrap()
                .aborted
        );
    }
}

#[tokio::test]
async fn cancelled_isolated_shutdown_retains_the_task_for_a_second_drain() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let id = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    let mut shutdown = Box::pin(manager.shutdown());
    tokio::select! {
        biased;
        result = &mut shutdown => panic!("shutdown skipped active prompt: {result:?}"),
        () = bounded(probe.settled.notified()) => {}
    }
    drop(shutdown);
    let mut retry = Box::pin(manager.shutdown());
    poll_fn(|context| {
        assert!(retry.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    probe.release_settled.notify_one();
    bounded(retry).await.unwrap();
    assert!(probe.finished.load(Ordering::SeqCst));
    assert!(
        bounded(owner.wait_for_isolated_session(&id))
            .await
            .unwrap()
            .aborted
    );
    assert!(manager.sessions().is_empty());
}

#[tokio::test]
async fn closing_an_isolated_owner_drains_prompt_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let id = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    let mut wait = Box::pin(owner.wait_for_isolated_session(&id));
    poll_fn(|context| {
        assert!(wait.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut close = Box::pin(manager.close_session(&owner));
    tokio::select! {
        biased;
        result = &mut close => panic!("close skipped active prompt: {result:?}"),
        () = bounded(probe.settled.notified()) => {}
    }
    // Losing the first close future must not remove tree membership while a
    // child is still draining, so the owner can be closed again safely.
    drop(close);
    let mut close = Box::pin(manager.close_session(&owner));
    poll_fn(|context| {
        assert!(close.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    probe.release_settled.notify_one();
    bounded(close).await.unwrap();
    assert!(probe.finished.load(Ordering::SeqCst));
    assert!(bounded(wait).await.unwrap().aborted);
    assert!(manager.sessions().is_empty());
    bounded(manager.shutdown()).await.unwrap();
}

#[tokio::test]
async fn dropping_the_isolated_manager_cancels_rather_than_detaches_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let probe = LifecycleProbe::default();
    let manager = manager(probe.clone());
    let owner = owner(&manager, &directory).await;
    let id = bounded(owner.launch_isolated_session(request()))
        .await
        .unwrap();
    owner.abort_isolated_session(&id).unwrap();
    bounded(probe.settled.notified()).await;
    let mut wait = Box::pin(owner.wait_for_isolated_session(&id));
    poll_fn(|context| {
        assert!(wait.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(manager);
    bounded(probe.settled_dropped.notified()).await;
    assert!(!probe.finished.load(Ordering::SeqCst));
    assert!(bounded(wait).await.is_err());
}
