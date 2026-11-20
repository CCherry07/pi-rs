//! Reliable observation is independent of the lossy snapshot/broadcast frontend contract.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use pi_agent::AgentOptions;
use pi_core::AbortSignal;
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, NoticeLevel, RevisionedAgentSessionEvent,
    SessionEventObserver,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tokio::sync::Notify;

async fn session(turns: Vec<ScriptedTurn>) -> (tempfile::TempDir, Arc<AgentSession>) {
    let directory = tempfile::tempdir().unwrap();
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted(turns))
        .agent_options(AgentOptions {
            cwd: directory.path().to_path_buf(),
            provider_id: "scripted".into(),
            model_id: "test".into(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let mut options = AgentSessionOptions::default();
    options.in_memory = true;
    let session = AgentSession::create(runtime, directory.path().join("memory.jsonl"), options)
        .await
        .unwrap();
    (directory, session)
}

struct QueueReader {
    session: Weak<AgentSession>,
    observed: Arc<AtomicBool>,
    reads: Mutex<Vec<(u64, bool)>>,
}
#[async_trait::async_trait]
impl SessionEventObserver for QueueReader {
    fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
        if matches!(event.event, AgentSessionEvent::QueueUpdate { .. }) {
            let session = self.session.upgrade().unwrap();
            let pending = session.has_pending_messages();
            self.reads.lock().unwrap().push((event.revision, pending));
            self.observed.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[test]
fn queue_observer_can_read_activity_without_blocking_the_publishing_executor() {
    // The watchdog is outside the executor: a synchronous mutex deadlock would
    // also prevent a Tokio timeout on the publishing thread from being polled.
    let (completed, result) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (_directory, session) = session(vec![
                ScriptedTurn::Text("one".into()),
                ScriptedTurn::Text("two".into()),
                ScriptedTurn::Text("three".into()),
            ])
            .await;
            let observed = Arc::new(AtomicBool::new(false));
            let observer = Arc::new(QueueReader {
                session: Arc::downgrade(&session),
                observed: observed.clone(),
                reads: Mutex::default(),
            });
            let _guard = session.observe_events(observer.clone());
            session.clear_queue().unwrap();
            assert!(observed.load(Ordering::SeqCst));
            // Exercise idle enqueue/activation, explicit next-run admission, and
            // queue edits as well as empty clear. Every callback reads activity.
            session
                .queue_user_input("queued steer", pi_plugin::InputStreamingBehavior::Steer)
                .await
                .unwrap();
            session
                .queue_user_input(
                    "queued follow-up",
                    pi_plugin::InputStreamingBehavior::FollowUp,
                )
                .await
                .unwrap();
            session.prompt("start").await.unwrap();
            session.queue_next_run("first").await.unwrap();
            session.queue_next_run("second").await.unwrap();
            let first = session.snapshot().queue.items[0].entry_id.clone();
            session.move_queued(&first, None).unwrap();
            let cancel = session.snapshot().queue.items[0].entry_id.clone();
            session.cancel_queued(&cancel).unwrap();
            session.run_next_queued().await.unwrap();
            session.clear_queue().unwrap();
            {
                let reads = observer.reads.lock().unwrap();
                assert!(reads.iter().any(|(_, pending)| *pending));
                assert!(!reads.last().unwrap().1);
                assert!(reads.windows(2).all(|pair| pair[0].0 < pair[1].0));
            }
            session.shutdown().await;
        });
        completed.send(()).unwrap();
    });
    match result.recv_timeout(Duration::from_secs(10)) {
        Ok(()) => worker.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => worker.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("queue observer deadlocked the publishing executor")
        }
    }
}

struct ReentrantObserver {
    session: Weak<AgentSession>,
    messages: Mutex<Vec<String>>,
    first: AtomicBool,
}
#[async_trait::async_trait]
impl SessionEventObserver for ReentrantObserver {
    fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
        if let AgentSessionEvent::PluginNotice { message, .. } = &event.event {
            self.messages.lock().unwrap().push(message.clone());
            let session = self.session.upgrade().unwrap();
            let _ = session.snapshot(); // The publisher must release its snapshot lock first.
            let _ = session.history().load().unwrap();
            if self.first.swap(false, Ordering::SeqCst) {
                session.notify_plugin("nested".into(), NoticeLevel::Info);
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn synchronous_observers_are_reentrant_ordered_and_do_not_share_broadcast_lag() {
    let (_directory, session) = session(vec![]).await;
    let mut broadcast = session.subscribe();
    let observer = Arc::new(ReentrantObserver {
        session: Arc::downgrade(&session),
        messages: Mutex::new(vec![]),
        first: AtomicBool::new(true),
    });
    let guard = session.observe_events(observer.clone());
    session.notify_plugin("outer".into(), NoticeLevel::Info);
    assert_eq!(*observer.messages.lock().unwrap(), ["outer", "nested"]);
    for i in 0..600 {
        session.notify_plugin(i.to_string(), NoticeLevel::Info);
    }
    assert_eq!(observer.messages.lock().unwrap().len(), 602);
    assert!(matches!(
        broadcast.events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    drop(guard);
    session.notify_plugin("not observed".into(), NoticeLevel::Info);
    assert_eq!(observer.messages.lock().unwrap().len(), 602);
}

#[derive(Default)]
struct GateObserver {
    events: Mutex<Vec<AgentSessionEvent>>,
    waiting: Notify,
    release: Notify,
    entered: AtomicBool,
}
#[async_trait::async_trait]
impl SessionEventObserver for GateObserver {
    fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
        self.events.lock().unwrap().push(event.event.clone());
        Ok(())
    }
    async fn flush(&self, _: AbortSignal) -> Result<(), String> {
        if !self.entered.swap(true, Ordering::SeqCst) {
            self.waiting.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

async fn exercise_backpressure(cancel: bool) {
    let (_directory, session) = session(vec![ScriptedTurn::Text("done".into())]).await;
    let observer = Arc::new(GateObserver::default());
    let guard = session.observe_events(observer.clone());
    let running = session.clone();
    let run = tokio::spawn(async move { running.prompt("hello").await });
    tokio::time::timeout(Duration::from_secs(3), observer.waiting.notified())
        .await
        .unwrap();
    assert_eq!(
        observer.events.lock().unwrap().len(),
        1,
        "producer advanced before its observer acknowledged output"
    );
    if cancel {
        session.abort();
    } else {
        drop(guard); // Unsubscription releases an acknowledgement already waiting in the observer.
    }
    tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    session.shutdown().await;
    assert_eq!(
        Arc::strong_count(&observer),
        1,
        "retired session kept its observer alive"
    );
}

#[tokio::test]
async fn dropping_shutdown_during_its_publication_fence_releases_acknowledgements() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    struct Collector(Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl SessionEventObserver for Collector {
        fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
            if let AgentSessionEvent::PluginNotice { message, .. } = &event.event {
                self.0.lock().unwrap().push(message.clone());
            }
            Ok(())
        }
    }
    struct Gate {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    #[async_trait::async_trait]
    impl SessionEventObserver for Gate {
        fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
            if matches!(&event.event, AgentSessionEvent::PluginNotice { message, .. } if message == "held")
            {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
    let (_directory, session) = session(vec![]).await;
    let collected = Arc::new(Collector(Mutex::default()));
    let _collector = session.observe_events(collected.clone());
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let gate = Arc::new(Gate {
        entered,
        release: Mutex::new(released),
    });
    let _gate = session.observe_events(gate.clone());
    let publishing = session.clone();
    let producer =
        std::thread::spawn(move || publishing.notify_plugin("held".into(), NoticeLevel::Info));
    entering.recv_timeout(Duration::from_secs(5)).unwrap();
    session.notify_plugin("tail".into(), NoticeLevel::Info);
    let (_abort, signal) = pi_core::AbortHandle::new();
    let mut acknowledged = Box::pin(session.flush_events(signal));
    assert!(
        poll_fn(|cx| Poll::Ready(acknowledged.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let mut closing = Box::pin(session.shutdown());
    assert!(
        poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(closing); // Direct runtime cancellation, not manager complete-on-drop.
    assert!(session.is_closed());
    tokio::time::timeout(Duration::from_secs(3), acknowledged)
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    producer.join().unwrap();
    assert_eq!(*collected.0.lock().unwrap(), ["held"]);
    assert_eq!(Arc::strong_count(&gate), 1);
    assert_eq!(Arc::strong_count(&collected), 1);
}

#[tokio::test]
async fn replaced_observers_do_not_inherit_a_cancelled_cohorts_checkpoint() {
    rotate_observers(true, FirstPoll::BeforeRotation).await;
}

#[tokio::test]
async fn surviving_observer_keeps_its_checkpoint_obligation() {
    rotate_observers(false, FirstPoll::BeforeRotation).await;
}

#[tokio::test]
async fn late_first_poll_does_not_inherit_a_replacement_observers_past() {
    rotate_observers(true, FirstPoll::AfterRotation).await;
}

#[tokio::test]
async fn late_first_poll_still_waits_for_a_surviving_observer() {
    rotate_observers(false, FirstPoll::AfterRotation).await;
}

#[tokio::test]
async fn replacement_observer_still_backpressures_its_own_new_publication() {
    rotate_observers(true, FirstPoll::AfterNewPublication).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FirstPoll {
    BeforeRotation,
    AfterRotation,
    AfterNewPublication,
}

async fn rotate_observers(remove_all: bool, first_poll: FirstPoll) {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    struct Recorder(Mutex<Vec<String>>, std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl SessionEventObserver for Recorder {
        fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
            if let AgentSessionEvent::PluginNotice { message, .. } = &event.event {
                self.0.lock().unwrap().push(message.clone());
            }
            Ok(())
        }
        async fn flush(&self, _: AbortSignal) -> Result<(), String> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    struct Gate {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    #[async_trait::async_trait]
    impl SessionEventObserver for Gate {
        fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
            if matches!(&event.event, AgentSessionEvent::PluginNotice { message, .. } if message == "held")
            {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
    let (_directory, session) = session(vec![]).await;
    let old = Arc::new(Recorder(Mutex::default(), Default::default()));
    let mut old_guard = Some(session.observe_events(old.clone()));
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let gate = Arc::new(Gate {
        entered,
        release: Mutex::new(released),
    });
    let gate_guard = session.observe_events(gate.clone());
    let publishing = session.clone();
    let producer =
        std::thread::spawn(move || publishing.notify_plugin("held".into(), NoticeLevel::Info));
    entering.recv_timeout(Duration::from_secs(5)).unwrap();
    session.notify_plugin("tail".into(), NoticeLevel::Info);
    let (abort, signal) = pi_core::AbortHandle::new();
    let mut acknowledged = Box::pin(session.flush_events(signal));
    if first_poll == FirstPoll::BeforeRotation {
        assert!(
            poll_fn(|cx| Poll::Ready(acknowledged.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }
    // No yield between removing the old cohort and replacing it in the same hub.
    drop(gate_guard);
    if remove_all {
        drop(old_guard.take());
    }
    let new = Arc::new(Recorder(Mutex::default(), Default::default()));
    let new_guard = session.observe_events(new.clone());
    if first_poll == FirstPoll::AfterNewPublication {
        session.notify_plugin("new-pending".into(), NoticeLevel::Info);
    }
    let polled = poll_fn(|cx| Poll::Ready(acknowledged.as_mut().poll(cx))).await;
    let was_pending = polled.is_pending();
    release.send(()).unwrap();
    producer.join().unwrap();
    match polled {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => tokio::time::timeout(Duration::from_secs(1), acknowledged)
            .await
            .expect("old checkpoint waited for a replacement observer")
            .unwrap(),
    }
    assert!(!abort.is_aborted());
    assert_eq!(old.1.load(Ordering::SeqCst), usize::from(!remove_all));
    assert_eq!(
        new.1.load(Ordering::SeqCst),
        usize::from(first_poll == FirstPoll::AfterNewPublication)
    );
    if remove_all {
        assert_eq!(*old.0.lock().unwrap(), ["held"]);
    } else {
        assert!(
            was_pending,
            "one cancelled lease released a still-live observer's checkpoint"
        );
        assert_eq!(*old.0.lock().unwrap(), ["held", "tail"]);
    }
    let mut expected = Vec::new();
    if first_poll == FirstPoll::AfterNewPublication {
        assert!(
            was_pending,
            "new publication bypassed its observer's checkpoint"
        );
        expected.push("new-pending".to_string());
    }
    assert_eq!(
        *new.0.lock().unwrap(),
        expected,
        "new observer received old publications"
    );
    drop(old_guard.take());
    // This event occurs only after the old flush completed; it cannot rescue it.
    session.notify_plugin("future".into(), NoticeLevel::Info);
    expected.push("future".to_string());
    assert_eq!(*new.0.lock().unwrap(), expected);
    drop(new_guard);
    session.shutdown().await;
    assert_eq!(Arc::strong_count(&old), 1);
    assert_eq!(Arc::strong_count(&gate), 1);
}

#[tokio::test]
async fn current_thread_backpressure_releases_on_abort_and_unsubscription() {
    exercise_backpressure(false).await;
    exercise_backpressure(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multithread_backpressure_releases_on_abort_and_unsubscription() {
    exercise_backpressure(false).await;
    exercise_backpressure(true).await;
}
