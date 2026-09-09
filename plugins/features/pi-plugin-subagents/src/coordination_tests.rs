use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, CustomMessageContent, CustomMessageInput, ModelsContextAccess, PluginContextEpoch,
    PluginContextError, PluginContextHandle, PluginContextResult, PluginContextScope,
    SendMessageOptions, SessionContextAccess, ToolResult, UiContextAccess,
};
use serde_json::json;

use super::{Coordination, ManagedRun, RunResult};

type DeliveryAction = Box<dyn FnOnce() -> PluginContextResult<()> + Send>;

#[derive(Default)]
struct Access {
    attempts: AtomicUsize,
    accepted: Mutex<Vec<CustomMessageInput>>,
    next: Mutex<Option<DeliveryAction>>,
}

impl Access {
    fn on_next(&self, action: impl FnOnce() -> PluginContextResult<()> + Send + 'static) {
        *self.next.lock().unwrap() = Some(Box::new(action));
    }

    fn messages(&self) -> Vec<CustomMessageInput> {
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
        assert_eq!(options.trigger_turn, Some(true));
        assert_eq!(options.deliver_as, None);
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let action = self.next.lock().unwrap().take();
        if let Some(action) = action {
            action()?;
        }
        self.accepted.lock().unwrap().push(message);
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

fn reserve(coordination: &Coordination, owner: &str, id: &str) {
    coordination.reserve(
        id,
        ManagedRun {
            owner: owner.into(),
            details: json!({"runId":id,"agent":"worker"}),
            abort: AbortHandle::new().0,
            result: None,
            detached: false,
        },
    );
}

fn detached(coordination: &Coordination, owner: &str, id: &str) {
    reserve(coordination, owner, id);
    let receipt = coordination.detach(owner, id).unwrap();
    assert_eq!(receipt.details.unwrap()["detached"], true);
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
fn detached_completion_preserves_first_success_or_error_and_notifies_once() {
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
        coordination.bind_session("owner".into(), handle(&epoch));

        assert_eq!(
            coordination.run("owner", "run").unwrap().result,
            Some(terminal.clone())
        );
        assert_eq!(coordination.detach("owner", "run"), terminal);
        assert_eq!(access.attempts.load(Ordering::SeqCst), 1);
        assert_notification(&access.messages()[0], "run", &terminal);
    }
}

#[test]
fn completion_before_detach_returns_terminal_result_without_notification() {
    for terminal in [Ok(ToolResult::text("done")), Err("failed".into())] {
        let coordination = Arc::new(Coordination::default());
        let (access, epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&epoch));
        reserve(&coordination, "owner", "run");
        let (completed, wait) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                coordination.complete("run", terminal.clone());
                completed.send(()).unwrap();
            });
            wait.recv().unwrap();
            assert_eq!(coordination.detach("owner", "run"), terminal);
        });
        coordination.bind_session("owner".into(), handle(&epoch));
        assert!(!coordination.run("owner", "run").unwrap().detached);
        assert!(access.messages().is_empty());
    }
}

#[test]
fn unavailable_and_retired_generations_retain_completion_until_owner_rebind() {
    for retired in [false, true] {
        let coordination = Coordination::default();
        let (old, old_epoch) = adapter();
        if retired {
            coordination.bind_session("owner".into(), handle(&old_epoch));
            old_epoch.retire();
        }
        detached(&coordination, "owner", "run");
        let terminal = Ok(ToolResult::text("retained"));
        coordination.complete("run", terminal.clone());
        assert_eq!(
            coordination.run("owner", "run").unwrap().result,
            Some(terminal.clone())
        );
        assert!(old.messages().is_empty());

        let (other, other_epoch) = adapter();
        coordination.bind_session("other-owner".into(), handle(&other_epoch));
        assert!(other.messages().is_empty());
        let (current, current_epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&current_epoch));
        coordination.bind_session("owner".into(), handle(&current_epoch));
        assert_eq!(current.messages().len(), 1);
        assert_notification(&current.messages()[0], "run", &terminal);
    }
}

#[test]
fn panicking_delivery_can_be_retried_after_rebind() {
    let coordination = Coordination::default();
    let (old, old_epoch) = adapter();
    old.on_next(|| panic!("delivery adapter panic"));
    coordination.bind_session("owner".into(), handle(&old_epoch));
    detached(&coordination, "owner", "run");
    let terminal = Err("child failed".into());
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        coordination.complete("run", terminal.clone())
    }));
    let (current, epoch) = adapter();
    coordination.bind_session("owner".into(), handle(&epoch));
    assert_eq!(current.messages().len(), 1);
    assert_notification(&current.messages()[0], "run", &terminal);
}

#[test]
fn rejected_delivery_is_retained_until_same_handle_is_ready_and_rebound() {
    let coordination = Coordination::default();
    let (access, epoch) = adapter();
    access.on_next(|| Err(PluginContextError::Unbound));
    coordination.bind_session("owner".into(), handle(&epoch));
    detached(&coordination, "owner", "run");
    let terminal = Err("child failed".into());
    coordination.complete("run", terminal.clone());
    coordination.complete("run", Ok(ToolResult::text("must not replace failure")));
    assert_eq!(access.attempts.load(Ordering::SeqCst), 1);
    assert!(access.messages().is_empty());

    coordination.bind_session("owner".into(), handle(&epoch));
    coordination.bind_session("owner".into(), handle(&epoch));
    assert_eq!(access.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(access.messages().len(), 1);
    assert_notification(&access.messages()[0], "run", &terminal);
    assert_eq!(
        coordination.run("owner", "run").unwrap().result,
        Some(terminal)
    );
}

#[test]
fn reentrant_rebind_waits_for_acceptance_before_deciding_whether_to_retry() {
    for rejected in [false, true] {
        let coordination = Arc::new(Coordination::default());
        let (old, old_epoch) = adapter();
        let (middle, middle_epoch) = adapter();
        let (current, current_epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&old_epoch));
        detached(&coordination, "owner", "run");
        let terminal = Ok(ToolResult::text("first terminal"));
        let expected = terminal.clone();
        let callback_coordination = Arc::clone(&coordination);
        let current_handle = handle(&current_epoch);
        old.on_next(move || {
            // These calls would deadlock if delivery retained the mailbox lock.
            assert_eq!(
                callback_coordination.run("owner", "run").unwrap().result,
                Some(expected)
            );
            callback_coordination.complete("run", Err("duplicate during delivery".into()));
            callback_coordination.bind_session("owner".into(), handle(&middle_epoch));
            callback_coordination.bind_session("owner".into(), current_handle);
            if rejected {
                Err(PluginContextError::Retired)
            } else {
                Ok(())
            }
        });

        coordination.complete("run", terminal.clone());
        coordination.bind_session("owner".into(), handle(&current_epoch));
        assert_eq!(old.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(middle.attempts.load(Ordering::SeqCst), 0);
        let accepted = if rejected {
            current.messages()
        } else {
            old.messages()
        };
        assert_eq!(accepted.len(), 1);
        assert_notification(&accepted[0], "run", &terminal);
        assert_eq!(old.messages().len() + current.messages().len(), 1);
        assert_eq!(
            coordination.run("owner", "run").unwrap().result,
            Some(terminal)
        );
    }
}

#[test]
fn detach_before_completion_and_concurrent_rebind_do_not_duplicate_inflight_delivery() {
    let coordination = Arc::new(Coordination::default());
    let (old, old_epoch) = adapter();
    let (current, current_epoch) = adapter();
    coordination.bind_session("owner".into(), handle(&old_epoch));
    reserve(&coordination, "owner", "run");
    let (entered, wait_for_delivery) = mpsc::channel();
    let (release, wait_for_release) = mpsc::channel();
    old.on_next(move || {
        entered.send(()).unwrap();
        wait_for_release.recv().unwrap();
        Ok(())
    });
    let terminal = Ok(ToolResult::text("done"));
    let receipt = coordination.detach("owner", "run").unwrap();
    assert_eq!(receipt.details.unwrap()["detached"], true);
    thread::scope(|scope| {
        let completion = scope.spawn(|| coordination.complete("run", terminal.clone()));
        wait_for_delivery.recv().unwrap();
        assert_eq!(coordination.detach("owner", "run"), terminal);
        coordination.complete("run", Err("late result".into()));
        coordination.bind_session("owner".into(), handle(&current_epoch));
        assert!(current.messages().is_empty());
        release.send(()).unwrap();
        completion.join().unwrap();
    });
    coordination.bind_session("owner".into(), handle(&current_epoch));
    assert_eq!(old.messages().len(), 1);
    assert_notification(&old.messages()[0], "run", &terminal);
    assert!(current.messages().is_empty());
}

#[test]
fn remove_and_close_discard_pending_notifications_without_affecting_other_owners() {
    for close in [false, true] {
        let coordination = Coordination::default();
        detached(&coordination, "owner", "run");
        detached(&coordination, "other", "other-run");
        coordination.complete("run", Err("discard me".into()));
        let retained = Ok(ToolResult::text("keep me"));
        coordination.complete("other-run", retained.clone());
        if close {
            coordination.close_owner("owner");
        } else {
            coordination.remove("run");
        }
        coordination.complete("run", Ok(ToolResult::text("late completion")));
        let (access, epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&epoch));
        assert!(access.messages().is_empty());
        assert!(coordination.run("owner", "run").is_err());
        coordination.bind_session("other".into(), handle(&epoch));
        assert_eq!(access.messages().len(), 1);
        assert_notification(&access.messages()[0], "other-run", &retained);
    }
}

#[test]
fn remove_and_close_during_failed_delivery_do_not_resurrect_notification() {
    for close in [false, true] {
        let coordination = Arc::new(Coordination::default());
        let (old, old_epoch) = adapter();
        let (current, current_epoch) = adapter();
        coordination.bind_session("owner".into(), handle(&old_epoch));
        detached(&coordination, "owner", "run");
        let callback_coordination = Arc::clone(&coordination);
        let current_handle = handle(&current_epoch);
        old.on_next(move || {
            // Rebind first marks the in-flight failure for retry, but removal
            // must still cancel that retry when the adapter returns.
            callback_coordination.bind_session("owner".into(), current_handle);
            if close {
                callback_coordination.close_owner("owner");
            } else {
                callback_coordination.remove("run");
            }
            Err(PluginContextError::Retired)
        });
        coordination.complete("run", Ok(ToolResult::text("discard me")));
        coordination.bind_session("owner".into(), handle(&current_epoch));
        coordination.complete("run", Err("late completion".into()));
        assert!(coordination.run("owner", "run").is_err());
        assert_eq!(old.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(current.attempts.load(Ordering::SeqCst), 0);
        assert!(old.messages().is_empty());
    }
}

#[test]
fn old_acknowledgement_cannot_clear_replacement_run_notification() {
    let coordination = Arc::new(Coordination::default());
    let (old, old_epoch) = adapter();
    let (current, current_epoch) = adapter();
    let (old_entered, wait_for_old) = mpsc::channel();
    let (release_old, wait_for_old_release) = mpsc::channel();
    old.on_next(move || {
        old_entered.send(()).unwrap();
        wait_for_old_release.recv().unwrap();
        Ok(())
    });
    let (current_entered, wait_for_current) = mpsc::channel();
    let (release_current, wait_for_current_release) = mpsc::channel();
    current.on_next(move || {
        current_entered.send(()).unwrap();
        wait_for_current_release.recv().unwrap();
        Err(PluginContextError::Unbound)
    });
    coordination.bind_session("owner".into(), handle(&old_epoch));
    detached(&coordination, "owner", "run");
    let terminal = Ok(ToolResult::text("replacement result"));
    thread::scope(|scope| {
        let original = scope.spawn(|| {
            coordination.complete("run", Ok(ToolResult::text("original result")));
        });
        wait_for_old.recv().unwrap();
        coordination.remove("run");
        coordination.bind_session("owner".into(), handle(&current_epoch));
        detached(&coordination, "owner", "run");
        let replacement = scope.spawn(|| coordination.complete("run", terminal.clone()));
        wait_for_current.recv().unwrap();
        // The old attempt returns while the replacement is in flight. Its
        // acceptance belongs only to the removed run, not to this new receipt.
        release_old.send(()).unwrap();
        original.join().unwrap();
        release_current.send(()).unwrap();
        replacement.join().unwrap();
    });
    assert_eq!(current.attempts.load(Ordering::SeqCst), 1);
    assert!(current.messages().is_empty());
    coordination.bind_session("owner".into(), handle(&current_epoch));
    assert_eq!(current.messages().len(), 1);
    assert_notification(&current.messages()[0], "run", &terminal);
    assert_eq!(
        coordination.run("owner", "run").unwrap().result,
        Some(terminal)
    );
}
