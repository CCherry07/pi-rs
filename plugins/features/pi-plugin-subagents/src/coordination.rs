//! Feature-owned run receipts and request/reply mailboxes. No transport files:
//! managed children run in this process, but keep upstream tool semantics.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pi_core::{
    CustomMessageContent, CustomMessageInput, PluginContextHandle, SendMessageOptions, ToolResult,
    Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

pub(crate) use crate::run_state::{ManagedRun, RunMetadata, RunResult, RunState, TerminalState};
use crate::waiting::{WaitDeadline, WaitDecision};

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

/// The record owns its timer; completion, attention and owner cleanup cancel it.
struct DeadlineWatch {
    deadline: Instant,
    snapshot: WaitDeadline,
    identity: Arc<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DeadlineWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, PluginContextHandle>,
    runs: HashMap<String, ManagedRun>,
    pending: HashMap<String, PendingRequest>,
    deadline_watches: HashMap<(String, String), DeadlineWatch>,
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
        self.lock().sessions.insert(id, handle);
        self.wake();
    }

    /// Stores usage on the run snapshot, then attributes it once to the
    /// immediate parent session. Snapshot reporting does not depend on the
    /// parent ledger write succeeding.
    pub fn record_usage(
        &self,
        run_id: &str,
        owner: &str,
        usage: Usage,
        details: Value,
    ) -> Result<(), String> {
        let handle = {
            let mut state = self.lock();
            let run = state
                .runs
                .get_mut(run_id)
                .filter(|run| run.owner == owner)
                .ok_or_else(|| format!("No owned subagent run found for {run_id:?}."))?;
            run.set_usage(usage.clone());
            state.sessions.get(owner).cloned().ok_or_else(|| {
                "Subagent owner is no longer available for usage accounting.".to_string()
            })?
        };
        self.wake();
        handle
            .access_for_adapter()
            .map_err(|error| error.to_string())?
            .record_usage(usage, Some(details))
            .map_err(|error| error.to_string())
    }

    pub fn monitor_started(&self, run_id: &str) -> Option<Instant> {
        let deadline = self
            .lock()
            .runs
            .get_mut(run_id)
            .filter(|run| run.result.is_none())
            .and_then(ManagedRun::start_deadline);
        self.wake();
        deadline
    }

    pub fn bind_child_session(&self, run_id: &str, session_id: &str) {
        if let Some(run) = self.lock().runs.get_mut(run_id) {
            run.set_session_id(session_id);
        }
        self.wake();
    }

    pub fn reserve(&self, id: &str, run: ManagedRun) {
        self.lock().runs.insert(id.into(), run);
        self.wake();
    }

    pub fn launched(&self, id: &str, isolated_id: &str) {
        if let Some(run) = self.lock().runs.get_mut(id) {
            run.set_isolated_session_id(isolated_id);
            run.mark_running();
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
        if let Some(id) = prefix
            && state.runs.get(id).is_some_and(|run| run.owner == owner)
        {
            return Ok(vec![id.to_string()]);
        }
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

    pub fn wait_decision(
        &self,
        owner: &str,
        ids: &[String],
        all: bool,
    ) -> Result<WaitDecision, String> {
        let state = self.lock();
        let runs = ids
            .iter()
            .map(|id| {
                state
                    .runs
                    .get(id)
                    .filter(|run| run.owner == owner)
                    .ok_or_else(|| format!("No owned subagent run found for {id:?}."))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pending = state
            .pending
            .values()
            .filter(|request| request.owner == owner && request.deadline > Instant::now())
            .map(|request| request.request.clone())
            .collect();
        Ok(crate::waiting::decide(ids, &runs, pending, all))
    }

    /// Caller resolves the prefix once. Registration and timer ownership are atomic.
    pub fn arm_wait(self: &Arc<Self>, owner: &str, id: &str, timeout: Duration) -> RunResult {
        let key = (owner.to_string(), id.to_string());
        loop {
            let mut state = self.lock();
            let run = state
                .runs
                .get(id)
                .filter(|run| run.owner == owner)
                .ok_or_else(|| format!("No owned subagent run found for {id:?}."))?;
            let pending = state
                .pending
                .values()
                .filter(|request| {
                    request.owner == owner
                        && request.request.run_id == id
                        && request.deadline > Instant::now()
                })
                .map(|request| request.request.clone())
                .collect();
            if let WaitDecision::Ready(result) =
                crate::waiting::decide(&[id.to_string()], &[run], pending, false)
            {
                return *result;
            }
            if !run.is_detached() {
                return Err("nonBlocking requires a detached run. Start background work with subagent async:true.".into());
            }
            if let Some(watch) = state.deadline_watches.get(&key) {
                if watch.deadline > Instant::now() {
                    return Ok(crate::waiting::armed(id, watch.snapshot, true));
                }
                // A due timer may not have been polled yet. Settle it before rearming.
                let identity = watch.identity.clone();
                drop(state);
                self.expire_wait(&key, &identity);
                continue;
            }
            let deadline = Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| "timeoutMs is too large.".to_string())?;
            let snapshot = WaitDeadline {
                deadline_at: now_ms()
                    .saturating_add(timeout.as_millis().try_into().unwrap_or(u64::MAX)),
            };
            let identity = Arc::new(());
            let weak = Arc::downgrade(self);
            let timer_key = key.clone();
            let timer_identity = identity.clone();
            let task = tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;
                if let Some(coordination) = weak.upgrade() {
                    coordination.expire_wait(&timer_key, &timer_identity);
                }
            });
            state.deadline_watches.insert(
                key,
                DeadlineWatch {
                    deadline,
                    snapshot,
                    identity,
                    task,
                },
            );
            drop(state);
            self.wake();
            return Ok(crate::waiting::armed(id, snapshot, false));
        }
    }

    fn expire_wait(&self, key: &(String, String), identity: &Arc<()>) {
        let mut state = self.lock();
        let Some(watch) = state.deadline_watches.get(key) else {
            return;
        };
        // An old timer cannot remove a watch rearmed after attention or expiry.
        if !Arc::ptr_eq(&watch.identity, identity) || watch.deadline > Instant::now() {
            return;
        }
        let deadline_at = watch.snapshot.deadline_at;
        state.deadline_watches.remove(key);
        let (owner, id) = key;
        let notification = (|| {
            let run = state
                .runs
                .get(id)
                .filter(|run| run.owner == *owner && run.result.is_none())?;
            if state.pending.values().any(|request| {
                request.owner == *owner
                    && request.request.run_id == *id
                    && request.deadline > Instant::now()
            }) {
                return None;
            }
            let handle = state.sessions.get(owner)?.clone();
            let snapshot = run.snapshot(id, Vec::new());
            Some((
                handle,
                CustomMessageInput {
                    custom_type: "subagent-wait-expired".into(),
                    content: CustomMessageContent::Text(format!(
                        "Wait window for subagent {id} elapsed; work continues. Query subagent_supervisor status or wait on the same run."
                    )),
                    display: true,
                    details: Some(json!({"runId": id, "deadlineAt": deadline_at,
                        "timedOut": true, "state": snapshot.state})),
                },
            ))
        })();
        drop(state);
        self.wake();
        if let Some((handle, message)) = notification {
            Self::try_send(&handle, message, true);
        }
    }

    pub fn pending(&self, owner: &str) -> Vec<SupervisorRequest> {
        let mut state = self.lock();
        expire_requests(&mut state);
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
        self.detach_for(owner, id, false)
    }

    pub fn background(&self, owner: &str, id: &str) -> Result<ToolResult, String> {
        self.detach_for(owner, id, true)
    }

    fn detach_for(&self, owner: &str, id: &str, background: bool) -> Result<ToolResult, String> {
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
        run.detach();
        let mut result = ToolResult::text(if background {
            format!(
                "Subagent {id} is running in the background of this process. Continue other work or return control; completion and supervisor requests attempt a best-effort notification. Use subagent_supervisor status to inspect, or bg_wait when the result is needed in this turn. This receipt is not task completion."
            )
        } else {
            format!(
                "Detached for intercom coordination before task completion. Run: {id}. Reply with subagent_supervisor, then bg_wait({{\"id\":\"{id}\"}}). Keep using this run; do not launch a replacement."
            )
        });
        let pending_request_ids = pending
            .iter()
            .filter(|request| request.run_id == id)
            .map(|request| request.id.clone())
            .collect();
        let mut details = run.details(id, pending_request_ids);
        details["detachedReason"] = json!(if background {
            "background launch"
        } else {
            "intercom coordination"
        });
        details["background"] = json!(background);
        details["pending"] = json!(pending);
        result.details = Some(details);
        Ok(result)
    }

    #[cfg(test)]
    pub fn complete(&self, id: &str, result: RunResult) {
        let terminal = match &result {
            Ok(result) if !result.is_error => TerminalState::Completed,
            _ => TerminalState::Failed,
        };
        self.complete_with_state(id, terminal, result);
    }

    pub fn complete_with_state(&self, id: &str, terminal: TerminalState, result: RunResult) {
        let mut state = self.lock();
        let Some(run) = state.runs.get_mut(id).filter(|run| run.result.is_none()) else {
            return;
        };
        let owner = run.owner.clone();
        let notify = run.is_detached();
        let notification = notify.then(|| {
            let details = match &result {
                Ok(result) => json!({"runId":id,"content":result.content,"details":result.details,"isError":result.is_error}),
                Err(error) => json!({"runId":id,"error":error}),
            };
            CustomMessageInput {
                custom_type: "subagent-notify".into(),
                content: CustomMessageContent::Text(format!(
                    "Detached subagent {id} finished. {details}"
                )),
                display: true,
                details: Some(details),
            }
        });
        if !run.finish(terminal, result) {
            return;
        }
        state
            .deadline_watches
            .remove(&(owner.clone(), id.to_string()));
        let requests = state
            .pending
            .values()
            .filter(|request| request.request.run_id == id)
            .map(|request| request.request.id.clone())
            .collect::<Vec<_>>();
        for request_id in requests {
            state.pending.remove(&request_id);
        }
        let handle = state.sessions.get(&owner).cloned();
        drop(state);
        self.wake();
        if let (Some(handle), Some(notification)) = (handle, notification) {
            Self::try_send(&handle, notification, true);
        }
    }

    pub fn remove(&self, id: &str) {
        let mut state = self.lock();
        if let Some(run) = state.runs.remove(id) {
            run.abort.abort();
            state.deadline_watches.remove(&(run.owner, id.to_string()));
        }
        state
            .pending
            .retain(|_, pending| pending.request.run_id != id);
        drop(state);
        self.wake();
    }

    pub fn cancel_owner(&self, owner: &str) {
        let mut state = self.lock();
        state.deadline_watches.retain(|(id, _), _| id != owner);
        for run in state
            .runs
            .values_mut()
            .filter(|run| run.owner == owner && run.result.is_none())
        {
            run.mark_cancelling();
            run.abort.abort();
        }
        drop(state);
        self.wake();
    }

    pub fn cancelling(&self, owner: &str, id: &str) {
        if let Some(run) = self
            .lock()
            .runs
            .get_mut(id)
            .filter(|run| run.owner == owner && run.result.is_none())
        {
            run.mark_cancelling();
        }
        self.wake();
    }

    pub fn status(&self, owner: &str, prefix: Option<&str>) -> Result<Value, String> {
        let state = self.lock();
        let exact = prefix
            .and_then(|id| state.runs.get(id).filter(|run| run.owner == owner))
            .is_some();
        let mut runs = state
            .runs
            .iter()
            .filter(|(id, run)| {
                run.owner == owner
                    && prefix.is_none_or(|prefix| {
                        if exact {
                            id.as_str() == prefix
                        } else {
                            id.starts_with(prefix)
                        }
                    })
            })
            .map(|(id, run)| {
                let mut pending = state
                    .pending
                    .values()
                    .filter(|request| {
                        request.owner == owner
                            && request.request.run_id == *id
                            && request.deadline > Instant::now()
                    })
                    .map(|request| request.request.id.clone())
                    .collect::<Vec<_>>();
                pending.sort();
                let mut snapshot = run.snapshot(id, pending);
                snapshot.non_blocking_wait = state
                    .deadline_watches
                    .get(&(owner.to_string(), id.clone()))
                    .filter(|watch| watch.deadline > Instant::now())
                    .map(|watch| watch.snapshot);
                snapshot
            })
            .collect::<Vec<_>>();
        if prefix.is_some() && runs.len() != 1 {
            return Err("Run id must match exactly one run owned by this session.".into());
        }
        runs.sort_by(|a, b| a.run_id.cmp(&b.run_id));
        let mut summary = std::collections::BTreeMap::<RunState, usize>::new();
        for run in &runs {
            *summary.entry(run.state).or_default() += 1;
        }
        let pending = state
            .pending
            .values()
            .filter(|request| request.owner == owner && request.deadline > Instant::now())
            .count();
        Ok(json!({"active":true,"pending":pending,"summary":summary,"runs":runs}))
    }

    pub fn close_owner(&self, owner: &str) {
        let mut state = self.lock();
        state.sessions.remove(owner);
        state.deadline_watches.retain(|(id, _), _| id != owner);
        state.runs.retain(|_, run| {
            if run.owner == owner {
                run.abort.abort();
                false
            } else {
                true
            }
        });
        state.pending.retain(|_, pending| pending.owner != owner);
        drop(state);
        self.wake();
    }

    pub fn post(
        &self,
        request: SupervisorRequest,
        timeout: Duration,
    ) -> Result<(oneshot::Receiver<Result<String, String>>, bool), String> {
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
            state
                .deadline_watches
                .remove(&(owner.clone(), request.run_id.clone()));
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
        let message = CustomMessageInput {
            custom_type: "subagent_supervisor_request".into(),
            content: CustomMessageContent::Text(format!(
                "Subagent {} ({}) requests {}:\n{}\n{}\n{}",
                request.agent,
                request.run_id,
                request.reason,
                request.message,
                request
                    .interview
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                if request.expects_reply {
                    format!(
                        "Reply with subagent_supervisor({{\"action\":\"reply\",\"replyTo\":\"{}\",\"message\":\"...\"}}), then bg_wait on the same run.",
                        request.id
                    )
                } else {
                    "Progress update; no reply required.".into()
                }
            )),
            display: true,
            details: Some(json!(request)),
        };
        let handle = state.sessions.get(&owner).cloned();
        drop(state);
        self.wake();
        let delivered = handle
            .as_ref()
            .is_some_and(|handle| Self::try_send(handle, message, request.expects_reply));
        Ok((receiver, delivered))
    }

    pub fn withdraw(&self, id: &str) {
        let mut state = self.lock();
        state.pending.remove(id);
        drop(state);
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
        let pending = {
            let mut state = self.lock();
            state
                .pending
                .remove(&request.id)
                .ok_or_else(|| "Supervisor request was already resolved.".to_string())?
        };
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

    fn send(
        handle: &PluginContextHandle,
        message: CustomMessageInput,
        trigger_turn: bool,
    ) -> Result<(), String> {
        handle
            .access_for_adapter()
            .and_then(|access| {
                access.send_message(
                    message,
                    SendMessageOptions {
                        trigger_turn: Some(trigger_turn),
                        deliver_as: None,
                    },
                )
            })
            .map_err(|error| error.to_string())
    }

    fn try_send(
        handle: &PluginContextHandle,
        message: CustomMessageInput,
        trigger_turn: bool,
    ) -> bool {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::send(handle, message, trigger_turn)
        }))
        .is_ok_and(|result| result.is_ok())
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

fn expire_requests(state: &mut State) {
    let expired = state
        .pending
        .iter()
        .filter(|(_, request)| request.deadline <= Instant::now())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in expired {
        state.pending.remove(&id);
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
