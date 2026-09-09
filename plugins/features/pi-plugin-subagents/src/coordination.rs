//! Session-owned run receipts and request/reply mailboxes. No transport files:
//! managed children run in this process, but keep upstream tool semantics.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pi_core::{
    AbortHandle, CustomMessageContent, CustomMessageInput, PluginContextHandle, SendMessageOptions,
    ToolResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

pub(crate) type RunResult = Result<ToolResult, String>;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SupervisorReason {
    NeedDecision,
    InterviewRequest,
    ProgressUpdate,
}

impl std::fmt::Display for SupervisorReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NeedDecision => "need_decision",
            Self::InterviewRequest => "interview_request",
            Self::ProgressUpdate => "progress_update",
        })
    }
}

#[derive(Clone)]
pub(crate) struct ManagedRun {
    pub owner: String,
    pub details: Value,
    pub abort: AbortHandle,
    pub result: Option<RunResult>,
    pub detached: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupervisorRequest {
    pub id: String,
    pub run_id: String,
    pub agent: String,
    pub child_index: usize,
    pub tool_call_id: String,
    pub reason: SupervisorReason,
    pub message: String,
    pub expects_reply: bool,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interview: Option<Value>,
}

struct PendingRequest {
    request: SupervisorRequest,
    owner: String,
    deadline: Instant,
    reply: oneshot::Sender<Result<String, String>>,
}

enum NotificationDelivery {
    Pending,
    InFlight {
        // Identity prevents an old adapter call from acknowledging a replacement
        // notification after remove/close and reuse of the run id.
        attempt: Arc<()>,
        retry_on_failure: bool,
    },
}

struct PendingNotification {
    owner: String,
    message: CustomMessageInput,
    delivery: NotificationDelivery,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, PluginContextHandle>,
    runs: HashMap<String, ManagedRun>,
    pending: HashMap<String, PendingRequest>,
    notifications: HashMap<String, PendingNotification>,
}

pub(crate) struct Coordination {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Default for Coordination {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: watch::channel(0).0,
        }
    }
}

impl Coordination {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wake(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn bind_session(&self, id: String, handle: PluginContextHandle) {
        let mut state = self.lock();
        state.sessions.insert(id.clone(), handle);
        let mut ready = Vec::new();
        for (run_id, notification) in &mut state.notifications {
            if notification.owner != id {
                continue;
            }
            match &mut notification.delivery {
                NotificationDelivery::Pending => ready.push(run_id.clone()),
                NotificationDelivery::InFlight {
                    retry_on_failure, ..
                } => *retry_on_failure = true,
            }
        }
        drop(state);
        self.wake();
        for run_id in ready {
            self.deliver_completion(&run_id);
        }
    }

    pub fn reserve(&self, id: &str, run: ManagedRun) {
        self.lock().runs.insert(id.into(), run);
        self.wake();
    }

    pub fn launched(&self, id: &str, isolated_id: &str) {
        if let Some(run) = self.lock().runs.get_mut(id) {
            run.details["isolatedSessionId"] = json!(isolated_id);
        }
        self.wake();
    }

    pub fn run(&self, owner: &str, id: &str) -> Result<ManagedRun, String> {
        self.lock()
            .runs
            .get(id)
            .filter(|run| run.owner == owner)
            .cloned()
            .ok_or_else(|| format!("No owned subagent run found for {id:?}."))
    }

    pub fn run_ids(&self, owner: &str, prefix: Option<&str>) -> Result<Vec<String>, String> {
        let state = self.lock();
        let mut ids = state
            .runs
            .iter()
            .filter(|(id, run)| {
                run.owner == owner
                    && prefix.map_or(run.result.is_none(), |prefix| id.starts_with(prefix))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        if prefix.is_some() && ids.len() != 1 {
            return Err("Run id must match exactly one run owned by this session.".into());
        }
        Ok(ids)
    }

    pub fn pending(&self, owner: &str) -> Vec<SupervisorRequest> {
        let mut state = self.lock();
        state
            .pending
            .retain(|_, pending| pending.deadline > Instant::now());
        let mut requests = state
            .pending
            .values()
            .filter(|pending| pending.owner == owner)
            .map(|pending| pending.request.clone())
            .collect::<Vec<_>>();
        requests.sort_by(|a, b| a.id.cmp(&b.id));
        requests
    }

    pub fn detach(&self, owner: &str, id: &str) -> Result<ToolResult, String> {
        let pending = self.pending(owner);
        let mut state = self.lock();
        let run = state
            .runs
            .get_mut(id)
            .filter(|run| run.owner == owner)
            .ok_or_else(|| "Subagent run is no longer available.".to_string())?;
        if let Some(result) = &run.result {
            return result.clone();
        }
        // Publishing a retained receipt is the ownership handoff. A caller may
        // release its cancellation guard only after this succeeds.
        run.detached = true;
        let mut result = ToolResult::text(format!(
            "Detached for intercom coordination before task completion. Run: {id}. Reply with subagent_supervisor, then bg_wait({{\"id\":\"{id}\"}}). Keep using this run; do not launch a replacement."
        ));
        let mut details = run.details.clone();
        details["state"] = json!("detached");
        details["detached"] = json!(true);
        details["detachedReason"] = json!("intercom coordination");
        details["activityState"] = json!(if pending.iter().any(|p| p.run_id == id) {
            "needs_attention"
        } else {
            "running"
        });
        details["pending"] = json!(pending);
        result.details = Some(details);
        Ok(result)
    }

    pub fn complete(&self, id: &str, result: RunResult) {
        let mut state = self.lock();
        let Some(run) = state.runs.get_mut(id).filter(|run| run.result.is_none()) else {
            return;
        };
        let notification = run.detached.then(|| {
            let details = match &result {
                Ok(result) => json!({"runId":id,"content":result.content,"details":result.details,"isError":result.is_error}),
                Err(error) => json!({"runId":id,"error":error}),
            };
            PendingNotification {
                owner: run.owner.clone(),
                message: CustomMessageInput {
                    custom_type: "subagent-notify".into(),
                    content: CustomMessageContent::Text(format!("Detached subagent {id} finished. {}", details)),
                    display: true,
                    details: Some(details),
                },
                delivery: NotificationDelivery::Pending,
            }
        });
        run.result = Some(result);
        if let Some(notification) = notification {
            state.notifications.insert(id.into(), notification);
        }
        state
            .pending
            .retain(|_, pending| pending.request.run_id != id);
        drop(state);
        self.wake();
        self.deliver_completion(id);
    }

    fn deliver_completion(&self, id: &str) {
        loop {
            let mut state = self.lock();
            let Some(notification) = state.notifications.get(id) else {
                return;
            };
            if !matches!(notification.delivery, NotificationDelivery::Pending) {
                return;
            }
            let Some(handle) = state.sessions.get(&notification.owner).cloned() else {
                return;
            };
            let notification = state
                .notifications
                .get_mut(id)
                .expect("pending notification");
            let message = notification.message.clone();
            let attempt = Arc::new(());
            notification.delivery = NotificationDelivery::InFlight {
                attempt: Arc::clone(&attempt),
                retry_on_failure: false,
            };
            drop(state);

            // A reentrant rebind must not send a second copy while this adapter
            // may still accept the first. No mailbox lock crosses this call.
            let delivery = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Self::send(&handle, message)
            }))
            .unwrap_or_else(|_| Err("Completion delivery adapter panicked.".into()));
            let mut state = self.lock();
            let Some(notification) = state.notifications.get_mut(id) else {
                return;
            };
            let NotificationDelivery::InFlight {
                attempt: current,
                retry_on_failure,
            } = &notification.delivery
            else {
                return;
            };
            if !Arc::ptr_eq(current, &attempt) {
                return;
            }
            if delivery.is_ok() {
                state.notifications.remove(id);
                return;
            }
            let retry = *retry_on_failure;
            notification.delivery = NotificationDelivery::Pending;
            if !retry {
                return;
            }
            // A bind raced with the failed attempt. Retry its current handle
            // now; otherwise retain the notification until a future bind.
        }
    }

    pub fn remove(&self, id: &str) {
        let mut state = self.lock();
        if let Some(run) = state.runs.remove(id) {
            run.abort.abort();
        }
        state.notifications.remove(id);
        state
            .pending
            .retain(|_, pending| pending.request.run_id != id);
        drop(state);
        self.wake();
    }

    pub fn cancel_owner(&self, owner: &str) {
        for run in self.lock().runs.values().filter(|run| run.owner == owner) {
            run.abort.abort();
        }
    }

    pub fn close_owner(&self, owner: &str) {
        let mut state = self.lock();
        state.sessions.remove(owner);
        state.runs.retain(|_, run| {
            if run.owner == owner {
                run.abort.abort();
                false
            } else {
                true
            }
        });
        state.pending.retain(|_, pending| pending.owner != owner);
        state
            .notifications
            .retain(|_, notification| notification.owner != owner);
        drop(state);
        self.wake();
    }

    pub fn post(
        &self,
        request: SupervisorRequest,
        timeout: Duration,
    ) -> Result<oneshot::Receiver<Result<String, String>>, String> {
        let (sender, receiver) = oneshot::channel();
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "Supervisor timeout is too large.".to_string())?;
        let mut state = self.lock();
        let run = state
            .runs
            .get(&request.run_id)
            .filter(|run| run.result.is_none())
            .ok_or_else(|| "Supervisor channel is no longer active.".to_string())?;
        let owner = run.owner.clone();
        if request.expects_reply {
            state.pending.insert(
                request.id.clone(),
                PendingRequest {
                    request: request.clone(),
                    owner: owner.clone(),
                    deadline,
                    reply: sender,
                },
            );
        }
        drop(state);
        // Never hold the mailbox lock while calling a session adapter.
        let delivery = self.deliver(&owner, CustomMessageInput {
            custom_type: "subagent_supervisor_request".into(),
            content: CustomMessageContent::Text(format!(
                "Subagent {} ({}) requests {}:\n{}\n{}\n{}",
                request.agent, request.run_id, request.reason, request.message,
                request.interview.as_ref().map(ToString::to_string).unwrap_or_default(),
                if request.expects_reply { format!("Reply with subagent_supervisor({{\"action\":\"reply\",\"replyTo\":\"{}\",\"message\":\"...\"}}), then bg_wait on the same run.", request.id) } else { "Progress update; no reply required.".into() }
            )),
            display: true,
            details: Some(json!(request)),
        });
        if let Err(error) = delivery {
            // Blocking asks remain discoverable through the detach receipt and
            // pending tool even when a generation is momentarily retiring.
            if !request.expects_reply {
                return Err(error);
            }
        }
        self.wake();
        Ok(receiver)
    }

    pub fn withdraw(&self, id: &str) {
        self.lock().pending.remove(id);
        self.wake();
    }

    pub fn reply(
        &self,
        owner: &str,
        reply_to: Option<&str>,
        to: Option<&str>,
        message: &str,
    ) -> Result<SupervisorRequest, String> {
        if message.trim().is_empty() {
            return Err("message is required for supervisor replies.".into());
        }
        let candidates = self
            .pending(owner)
            .into_iter()
            .filter(|request| {
                if let Some(id) = reply_to {
                    return request.id == id;
                }
                to.is_none_or(|to| {
                    request.id.to_lowercase().starts_with(&to.to_lowercase())
                        || request.agent.eq_ignore_ascii_case(to)
                })
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err("Reply must identify exactly one pending supervisor request owned by this session. Use replyTo.".into());
        }
        let request = candidates.into_iter().next().expect("one candidate");
        let pending = self
            .lock()
            .pending
            .remove(&request.id)
            .ok_or_else(|| "Supervisor request was already resolved.".to_string())?;
        if pending.deadline <= Instant::now() {
            self.wake();
            return Err("Supervisor request has expired.".into());
        }
        pending
            .reply
            .send(Ok(message.trim().into()))
            .map_err(|_| "Supervisor request is no longer waiting for a reply.".to_string())?;
        self.wake();
        Ok(request)
    }

    fn deliver(&self, owner: &str, message: CustomMessageInput) -> Result<(), String> {
        let handle = self
            .lock()
            .sessions
            .get(owner)
            .cloned()
            .ok_or_else(|| "Supervisor session is unavailable.".to_string())?;
        Self::send(&handle, message)
    }

    fn send(handle: &PluginContextHandle, message: CustomMessageInput) -> Result<(), String> {
        handle
            .access_for_adapter()
            .and_then(|access| {
                access.send_message(
                    message,
                    SendMessageOptions {
                        trigger_turn: Some(true),
                        deliver_as: None,
                    },
                )
            })
            .map_err(|error| error.to_string())
    }

    pub fn abort_isolated(
        &self,
        owner: &str,
        id: pi_core::IsolatedSessionId,
    ) -> Result<(), String> {
        let handle = self
            .lock()
            .sessions
            .get(owner)
            .cloned()
            .ok_or_else(|| "Owner session is closed.".to_string())?;
        handle
            .access_for_adapter()
            .and_then(|access| access.abort_isolated_session(handle.scope(), id))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "coordination_tests.rs"]
mod tests;

pub(crate) fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
