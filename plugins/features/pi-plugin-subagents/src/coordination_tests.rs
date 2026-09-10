use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, CustomMessageContent, CustomMessageInput, IsolatedContextMode,
    ModelsContextAccess, PluginContextEpoch, PluginContextError, PluginContextHandle,
    PluginContextResult, PluginContextScope, SendMessageOptions, SessionContextAccess, ToolResult,
    UiContextAccess,
};
use serde_json::json;

use super::{Coordination, ManagedRun, RunMetadata, RunResult};

#[derive(Default)]
struct Access {
    attempts: AtomicUsize,
    reject: AtomicBool,
    accepted: Mutex<Vec<(CustomMessageInput, SendMessageOptions)>>,
}

impl Access {
    fn messages(&self) -> Vec<(CustomMessageInput, SendMessageOptions)> {
        self.accepted.lock().unwrap().clone()
    }
}

#[async_trait]
impl SessionContextAccess for Access {
    fn send_message(
        &self,
        message: CustomMessageInput,
        options: SendMessageOptions,
    ) -> PluginContextResult<()> {
        assert_eq!(options.deliver_as, None);
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.reject.load(Ordering::SeqCst) {
            return Err(PluginContextError::Unbound);
        }
        self.accepted.lock().unwrap().push((message, options));
        Ok(())
    }
}

#[async_trait]
impl ModelsContextAccess for Access {}
#[async_trait]
impl UiContextAccess for Access {}

fn adapter() -> (Arc<Access>, PluginContextEpoch) {
    let access = Arc::new(Access::default());
    let epoch = PluginContextEpoch::new(access.clone());
    (access, epoch)
}

fn handle(epoch: &PluginContextEpoch) -> PluginContextHandle {
    epoch.handle(PluginContextScope::Base)
}

fn reserve(coordination: &Coordination, owner: &str, id: &str) -> AbortHandle {
    let (abort, _) = AbortHandle::new();
    coordination.reserve(
        id,
        ManagedRun::new(
            owner.into(),
            RunMetadata::new("worker".into(), 1, IsolatedContextMode::Fresh),
            abort.clone(),
            None,
        ),
    );
    abort
}

fn detached(coordination: &Coordination, owner: &str, id: &str) {
    reserve(coordination, owner, id);
    let receipt = coordination.detach(owner, id).unwrap();
    let details = receipt.details.unwrap();
    assert_eq!(details["detached"], true);
    assert_eq!(details["waitMode"], "detached");
}

fn assert_notification(message: &CustomMessageInput, id: &str, result: &RunResult) {
    assert_eq!(message.custom_type, "subagent-notify");
    assert!(message.display);
    let details = match result {
        Ok(result) => json!({
            "runId":id,
            "content":result.content,
            "details":result.details,
            "isError":result.is_error,
        }),
        Err(error) => json!({"runId":id,"error":error}),
    };
    assert_eq!(message.details, Some(details.clone()));
    assert_eq!(
        message.content,
        CustomMessageContent::Text(format!("Detached subagent {id} finished. {details}"))
    );
}

#[test]
fn snapshots_are_owner_scoped_and_keep_state_activity_and_wait_mode_separate() {
    let coordination = Coordination::default();
    reserve(&coordination, "owner", "run");
    reserve(&coordination, "owner", "run-longer");
    reserve(&coordination, "other", "foreign");
    let starting = coordination.status("owner", Some("run")).unwrap();
    assert_eq!(starting["runs"][0]["state"], "starting");
    assert_eq!(starting["runs"][0]["activityState"], "normal");
    assert_eq!(starting["runs"][0]["waitMode"], "foreground");

    coordination.launched("run", "isolated");
    coordination.background("owner", "run").unwrap();
    let status = coordination.status("owner", Some("run")).unwrap();
    assert_eq!(status["runs"].as_array().unwrap().len(), 1);
    assert_eq!(status["runs"][0]["state"], "running");
    assert_eq!(status["runs"][0]["activityState"], "normal");
    assert_eq!(status["runs"][0]["waitMode"], "detached");
    assert_eq!(status["runs"][0]["detached"], true);
    assert_eq!(status["runs"][0]["isolatedSessionId"], "isolated");
    assert!(coordination.status("owner", Some("foreign")).is_err());
    assert!(coordination.status("owner", Some("ru")).is_err());

    coordination.cancelling("owner", "run");
    assert_eq!(
        coordination.status("owner", Some("run")).unwrap()["runs"][0]["state"],
        "cancelling"
    );
    assert!(coordination.run("owner", "run").unwrap().result.is_none());
    coordination.complete_with_state(
        "run",
        super::TerminalState::Cancelled,
        Err("cancelled".into()),
    );
    let status = coordination.status("owner", Some("run")).unwrap();
    assert_eq!(status["runs"][0]["state"], "cancelled");
    assert_eq!(status["runs"][0]["activityState"], "normal");
    assert!(status["runs"][0]["finishedAt"].is_u64());
    assert_eq!(status, coordination.status("owner", Some("run")).unwrap());
}

#[test]
fn detached_completion_preserves_the_first_terminal_result_and_notifies_once() {
    let mut tool_error = ToolResult::text("tool failed");
    tool_error.is_error = true;
    tool_error.details = Some(json!({"reason":"failure"}));
    for terminal in [
        Ok(ToolResult::text("first success")),
        Ok(tool_error),
        Err("cancelled or timed out".into()),
    ] {
        let coordination = Coordination::default();
        let (access, epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&epoch));
        detached(&coordination, "owner", "run");

        coordination.complete("run", terminal.clone());
        coordination.complete("run", Ok(ToolResult::text("late success")));
        coordination.complete("run", Err("late failure".into()));

        assert_eq!(
            coordination.run("owner", "run").unwrap().result,
            Some(terminal.clone())
        );
        assert_eq!(coordination.detach("owner", "run"), terminal);
        assert_eq!(access.attempts.load(Ordering::SeqCst), 1);
        let messages = access.messages();
        assert_eq!(messages[0].1.trigger_turn, Some(true));
        assert_notification(&messages[0].0, "run", &terminal);
    }
}

#[test]
fn completion_before_detach_returns_the_result_without_a_notification() {
    let coordination = Coordination::default();
    let (access, epoch) = adapter();
    coordination.bind_session("owner".into(), handle(&epoch));
    reserve(&coordination, "owner", "run");
    let terminal = Ok(ToolResult::text("done"));
    coordination.complete("run", terminal.clone());

    assert_eq!(coordination.detach("owner", "run"), terminal);
    assert_eq!(
        coordination.status("owner", Some("run")).unwrap()["runs"][0]["waitMode"],
        "foreground"
    );
    assert!(access.messages().is_empty());
}

#[test]
fn rejected_notification_does_not_lose_the_terminal_status_or_retry_on_rebind() {
    let coordination = Coordination::default();
    let (access, epoch) = adapter();
    access.reject.store(true, Ordering::SeqCst);
    coordination.bind_session("owner".into(), handle(&epoch));
    detached(&coordination, "owner", "run");
    coordination.complete("run", Ok(ToolResult::text("retained")));

    assert_eq!(access.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        coordination.status("owner", Some("run")).unwrap()["runs"][0]["state"],
        "completed"
    );
    access.reject.store(false, Ordering::SeqCst);
    coordination.bind_session("owner".into(), handle(&epoch));
    assert_eq!(access.attempts.load(Ordering::SeqCst), 1);
    assert!(access.messages().is_empty());
}

#[test]
fn closing_an_owner_aborts_and_removes_only_its_runs() {
    let coordination = Coordination::default();
    let owned = reserve(&coordination, "owner", "run");
    let foreign = reserve(&coordination, "other", "foreign");

    coordination.close_owner("owner");

    assert!(owned.is_aborted());
    assert!(!foreign.is_aborted());
    assert!(coordination.run("owner", "run").is_err());
    assert!(coordination.run("other", "foreign").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_nonblocking_registrations_share_one_timer() {
    let coordination = Arc::new(Coordination::default());
    detached(&coordination, "owner", "run");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let coordination = coordination.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            coordination
                .arm_wait("owner", "run", std::time::Duration::from_secs(60))
                .unwrap()
                .details
                .unwrap()
        }));
    }
    let first = tasks.remove(0).await.unwrap();
    let second = tasks.remove(0).await.unwrap();
    assert_eq!(first["deadlineAt"], second["deadlineAt"]);
    assert_ne!(first["reused"], second["reused"]);
    assert_eq!(coordination.lock().deadline_watches.len(), 1);
    coordination.close_owner("owner");
}

#[tokio::test(start_paused = true)]
async fn nonblocking_cleanup_cancels_timers_and_preserves_other_owners() {
    for close in [false, true] {
        let coordination = Arc::new(Coordination::default());
        let (access, epoch) = adapter();
        let (other, other_epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&epoch));
        coordination.bind_session("other".into(), handle(&other_epoch));
        detached(&coordination, "owner", "run");
        detached(&coordination, "other", "foreign");
        for (owner, id) in [("owner", "run"), ("other", "foreign")] {
            coordination
                .arm_wait(owner, id, std::time::Duration::from_secs(1))
                .unwrap();
        }
        let timer = coordination.lock().deadline_watches[&("owner".into(), "run".into())]
            .task
            .abort_handle();
        if close {
            coordination.close_owner("owner");
        } else {
            coordination.cancel_owner("owner");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(timer.is_finished());
        assert!(access.messages().is_empty());
        assert_eq!(other.messages().len(), 1);
        assert_eq!(other.messages()[0].0.custom_type, "subagent-wait-expired");
        assert!(coordination.lock().deadline_watches.is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn due_nonblocking_registration_rearms_without_a_stale_timer_consuming_it() {
    let coordination = Arc::new(Coordination::default());
    let (access, epoch) = adapter();
    coordination.bind_session("owner".into(), handle(&epoch));
    detached(&coordination, "owner", "run");
    coordination
        .arm_wait("owner", "run", std::time::Duration::from_secs(1))
        .unwrap();
    let key = ("owner".into(), "run".into());
    let identity = coordination.lock().deadline_watches[&key].identity.clone();
    // Model a delayed timer at its due deadline, without depending on executor ordering.
    coordination
        .lock()
        .deadline_watches
        .get_mut(&key)
        .unwrap()
        .deadline = tokio::time::Instant::now();
    let rearmed = coordination
        .arm_wait("owner", "run", std::time::Duration::from_secs(10))
        .unwrap()
        .details
        .unwrap();
    assert_eq!(rearmed["reused"], false);
    assert_eq!(access.messages().len(), 1);
    coordination.expire_wait(&key, &identity);
    assert_eq!(coordination.lock().deadline_watches.len(), 1);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(access.messages().len(), 1);
    tokio::time::sleep(std::time::Duration::from_secs(9)).await;
    assert_eq!(access.messages().len(), 2);
    assert!(coordination.run("owner", "run").unwrap().result.is_none());
}

#[tokio::test(start_paused = true)]
async fn rejected_nonblocking_reminder_settles_once_and_allows_rearming() {
    let coordination = Arc::new(Coordination::default());
    let (access, epoch) = adapter();
    access.reject.store(true, Ordering::SeqCst);
    coordination.bind_session("owner".into(), handle(&epoch));
    detached(&coordination, "owner", "run");
    coordination
        .arm_wait("owner", "run", std::time::Duration::from_secs(1))
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(access.attempts.load(Ordering::SeqCst), 1);
    assert!(
        coordination.status("owner", Some("run")).unwrap()["runs"][0]
            .get("nonBlockingWait")
            .is_none()
    );
    assert!(coordination.run("owner", "run").unwrap().result.is_none());
    access.reject.store(false, Ordering::SeqCst);
    let rearmed = coordination
        .arm_wait("owner", "run", std::time::Duration::from_secs(1))
        .unwrap()
        .details
        .unwrap();
    assert_eq!(rearmed["reused"], false);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(access.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(access.messages().len(), 1);
}

#[tokio::test]
async fn nonblocking_timer_does_not_retain_its_coordination_owner() {
    let coordination = Arc::new(Coordination::default());
    detached(&coordination, "owner", "run");
    coordination
        .arm_wait("owner", "run", std::time::Duration::from_secs(60))
        .unwrap();
    let weak = Arc::downgrade(&coordination);
    let timer = coordination.lock().deadline_watches[&("owner".into(), "run".into())]
        .task
        .abort_handle();
    drop(coordination);
    assert!(weak.upgrade().is_none());
    tokio::task::yield_now().await;
    assert!(timer.is_finished());
}

#[tokio::test(start_paused = true)]
async fn nonblocking_watch_ignores_sibling_attention_and_rearms_after_its_own_reply() {
    let coordination = Arc::new(Coordination::default());
    let (access, epoch) = adapter();
    coordination.bind_session("owner".into(), handle(&epoch));
    for id in ["run", "sibling"] {
        detached(&coordination, "owner", id);
    }
    let timeout = std::time::Duration::from_secs(1);
    coordination.arm_wait("owner", "run", timeout).unwrap();
    for id in ["sibling", "run"] {
        let (receiver, _) = coordination
            .post(
                super::SupervisorRequest {
                    id: format!("ask-{id}"),
                    run_id: id.into(),
                    agent: "worker".into(),
                    child_index: 0,
                    tool_call_id: "ask".into(),
                    reason: super::SupervisorReason::NeedDecision,
                    message: "Choose".into(),
                    expects_reply: true,
                    created_at: super::now_ms(),
                    expires_at: None,
                    interview: None,
                },
                std::time::Duration::from_secs(60),
            )
            .unwrap();
        let result = coordination
            .arm_wait("owner", "run", timeout)
            .unwrap()
            .details
            .unwrap();
        if id == "sibling" {
            assert_eq!(result["reused"], true);
        } else {
            assert_eq!(result["state"], "needs_attention");
            assert!(coordination.lock().deadline_watches.is_empty());
        }
        coordination
            .reply("owner", Some(&format!("ask-{id}")), None, "continue")
            .unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), "continue");
    }
    let rearmed = coordination
        .arm_wait("owner", "run", timeout)
        .unwrap()
        .details
        .unwrap();
    assert_eq!(rearmed["reused"], false);
    coordination.complete("run", Err("failed after decision".into()));
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let messages = access.messages();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2].0.custom_type, "subagent-notify");
    assert!(coordination.lock().deadline_watches.is_empty());
}
