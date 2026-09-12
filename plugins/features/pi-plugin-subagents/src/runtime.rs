use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::FutureExt;
use pi_core::{
    ContentBlock, CustomMessageContent, CustomMessageInput, IsolatedMessageDelivery,
    IsolatedSessionHandle, IsolatedSessionOutcome, IsolatedSessionTurnHandle, PluginContextHandle,
    SendMessageOptions, SessionContext, ToolResult, Usage,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

use crate::profiles::SubagentProfile;

pub(crate) const DEFAULT_MAX_DEPTH: usize = 4;
const DEFAULT_MAX_SPAWNS_PER_ROOT: usize = 64;
const DEFAULT_MAX_ACTIVE_AGENTS: usize = 8;
const MARKER_PREFIX: &str = "<!-- pi-rs-agent:";
const MARKER_SUFFIX: &str = " -->";

#[derive(Clone)]
pub struct SubagentRuntime {
    inner: Arc<RuntimeInner>,
}

#[derive(Clone)]
pub(crate) struct WeakSubagentRuntime(Weak<RuntimeInner>);

impl WeakSubagentRuntime {
    fn upgrade(&self) -> Option<SubagentRuntime> {
        self.0.upgrade().map(|inner| SubagentRuntime { inner })
    }
}

struct RuntimeInner {
    runtime_id: String,
    limits: RuntimeLimits,
    state: Mutex<RuntimeState>,
    changed: watch::Sender<u64>,
    monitors: Mutex<HashMap<String, (String, Arc<MonitorTask>)>>,
    desktop: Mutex<crate::desktop::DesktopState>,
}

struct MonitorTask {
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    abort: tokio::task::AbortHandle,
}

impl Drop for MonitorTask {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

impl MonitorTask {
    async fn drain(&self) {
        let mut task = self.task.lock().await;
        if let Some(task) = task.as_mut() {
            let _ = task.await;
        }
        task.take();
    }
}

#[derive(Debug, Clone, Copy)]
struct RuntimeLimits {
    max_depth: usize,
    max_spawns_per_root: usize,
    max_active_agents: usize,
}

#[derive(Default)]
struct RuntimeState {
    sessions: HashMap<String, OwnerSession>,
    agents: HashMap<String, AgentRecord>,
    assignments: HashMap<String, String>,
    roots: HashMap<String, RootBudget>,
    inboxes: HashMap<String, Vec<AgentMessage>>,
    waiters: HashMap<String, usize>,
}

#[derive(Clone)]
struct OwnerSession {
    session: SessionContext,
}

#[derive(Default)]
struct RootBudget {
    spawns: usize,
}

struct AgentRecord {
    id: String,
    parent_session_id: String,
    parent_agent_id: Option<String>,
    root_session_id: String,
    child_session_id: Option<String>,
    depth: usize,
    max_depth: usize,
    profile: SubagentProfile,
    task: Option<String>,
    handle: Option<IsolatedSessionHandle>,
    current_turn: Option<IsolatedSessionTurnHandle>,
    state: AgentState,
    last_result: Option<AgentTurnResult>,
    usage: Usage,
    warnings: Vec<String>,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentState {
    Starting,
    Running,
    Interrupting,
    Idle,
    Failed,
    Interrupted,
    TimedOut,
}

impl AgentState {
    fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Interrupting)
    }

    fn is_settled(self) -> bool {
        !self.is_active()
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentTurnResult {
    turn_id: String,
    text: String,
    is_error: bool,
    aborted: bool,
    usage: Usage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentSnapshot {
    pub id: String,
    pub agent: String,
    pub parent_agent_id: Option<String>,
    pub child_session_id: Option<String>,
    pub depth: usize,
    pub state: AgentState,
    pub current_turn_id: Option<String>,
    pub last_result: Option<Value>,
    pub usage: Usage,
    pub warnings: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentMessage {
    pub id: String,
    pub from: String,
    pub message: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitMode {
    Any,
    All,
}

pub(crate) enum WaitEvaluation {
    Pending,
    Ready {
        agents: Vec<AgentSnapshot>,
        messages: Vec<AgentMessage>,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LaunchError {
    #[error("agent nesting limit reached (depth {depth}, maximum {maximum})")]
    Depth { depth: usize, maximum: usize },
    #[error("agent spawn budget reached for this root session ({used}/{maximum})")]
    SpawnBudget { used: usize, maximum: usize },
    #[error("agent concurrency limit reached ({active}/{maximum} active)")]
    Concurrency { active: usize, maximum: usize },
    #[error("agent profile {profile:?} does not authorize nested delegation")]
    NestedDelegationDisabled { profile: String },
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    #[error("agent {0} is not a direct child of the current session")]
    NotOwned(String),
    #[error("agent {0} has not finished launching")]
    NotLaunched(String),
    #[error("agent {0} is already bound to another child session")]
    AlreadyBound(String),
}

#[derive(Debug)]
pub(crate) struct LaunchTicket {
    id: String,
    depth: usize,
}

impl LaunchTicket {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn child_prompt(&self, task: &str) -> String {
        format!("{}\n{task}", marker(&self.id))
    }
}

pub(crate) struct WaitRegistration {
    runtime: WeakSubagentRuntime,
    session_id: String,
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let mut state = runtime.lock();
        if let Some(count) = state.waiters.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.waiters.remove(&self.session_id);
            }
        }
    }
}

impl Default for SubagentRuntime {
    fn default() -> Self {
        Self::with_limits(RuntimeLimits {
            max_depth: DEFAULT_MAX_DEPTH,
            max_spawns_per_root: DEFAULT_MAX_SPAWNS_PER_ROOT,
            max_active_agents: DEFAULT_MAX_ACTIVE_AGENTS,
        })
    }
}

impl SubagentRuntime {
    fn with_limits(limits: RuntimeLimits) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                runtime_id: Uuid::now_v7().to_string(),
                limits,
                state: Mutex::new(RuntimeState::default()),
                changed: watch::channel(0).0,
                monitors: Mutex::new(HashMap::new()),
                desktop: Mutex::new(crate::desktop::DesktopState::default()),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, RuntimeState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wake(&self) {
        self.publish_desktop();
        self.inner
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changed.subscribe()
    }

    pub(crate) fn default_max_depth(&self) -> usize {
        self.inner.limits.max_depth
    }

    pub(crate) fn bind_session(&self, id: String, session: SessionContext) {
        let needs_history = self
            .inner
            .desktop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .needs_restore(&id);
        let history = if needs_history {
            session
                .snapshot()
                .map(|snapshot| {
                    snapshot
                        .branch()
                        .iter()
                        .map(|entry| entry.raw().clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.inner
            .desktop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .restore(&id, history);
        self.lock().sessions.insert(id, OwnerSession { session });
        self.wake();
    }

    pub(crate) fn suspend_desktop(&self, id: &str) {
        // Serializes with publication so shutdown cannot race a captured old context.
        self.inner
            .desktop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .suspend(id);
    }

    pub(crate) fn set_task(&self, id: &str, task: &str) {
        if let Some(agent) = self.lock().agents.get_mut(id) {
            agent.task = Some(task.to_string());
        }
        self.wake();
    }

    fn publish_desktop(&self) {
        let mut desktop = self
            .inner
            .desktop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owners = {
            let state = self.lock();
            state.sessions.iter().map(|(owner, registered)| {
                let agents = state.agents.values().filter(|agent| agent.parent_session_id == *owner)
                    .map(|agent| (agent.id.clone(), json!({
                        "agentId":agent.id,"agent":agent.profile.name,"task":agent.task,
                        "state":agent.state,"updatedAt":agent.updated_at,
                        "session":{"sessionId":agent.child_session_id,
                            "isolatedSessionId":agent.handle.as_ref().map(|handle| handle.id().as_str()),
                            "ownerSessionId":owner},
                        "totalTokens":agent.usage.total_tokens
                    }))).collect();
                (owner.clone(), registered.session.clone(), agents)
            }).collect::<Vec<_>>()
        };
        // append_entry can acquire the session journal lock; never keep the runtime lock here.
        for (owner, session, agents) in owners {
            desktop.publish(&self.inner.runtime_id, &owner, &session, agents);
        }
    }

    pub(crate) fn begin_launch(
        &self,
        parent_session_id: &str,
        profile: SubagentProfile,
        configured_max_depth: usize,
    ) -> Result<LaunchTicket, LaunchError> {
        let mut state = self.lock();
        let (root_session_id, parent_agent_id, parent_depth, inherited_max_depth) =
            if let Some(parent_id) = state.assignments.get(parent_session_id).cloned() {
                let parent = state
                    .agents
                    .get(&parent_id)
                    .ok_or_else(|| LaunchError::UnknownAgent(parent_id.clone()))?;
                if !parent.profile.allow_nested_subagents {
                    return Err(LaunchError::NestedDelegationDisabled {
                        profile: parent.profile.name.clone(),
                    });
                }
                (
                    parent.root_session_id.clone(),
                    Some(parent.id.clone()),
                    parent.depth,
                    parent.max_depth,
                )
            } else {
                (parent_session_id.to_string(), None, 0, configured_max_depth)
            };
        if parent_depth >= inherited_max_depth {
            return Err(LaunchError::Depth {
                depth: parent_depth,
                maximum: inherited_max_depth,
            });
        }
        let active = state
            .agents
            .values()
            .filter(|agent| agent.state.is_active())
            .count();
        if active >= self.inner.limits.max_active_agents {
            return Err(LaunchError::Concurrency {
                active,
                maximum: self.inner.limits.max_active_agents,
            });
        }
        let budget = state.roots.entry(root_session_id.clone()).or_default();
        if budget.spawns >= self.inner.limits.max_spawns_per_root {
            return Err(LaunchError::SpawnBudget {
                used: budget.spawns,
                maximum: self.inner.limits.max_spawns_per_root,
            });
        }
        budget.spawns += 1;

        let id = Uuid::now_v7().to_string();
        let depth = parent_depth + 1;
        let max_depth = profile
            .max_subagent_depth
            .map_or(inherited_max_depth, |maximum| {
                inherited_max_depth.min(maximum)
            });
        let now = now_ms();
        state.agents.insert(
            id.clone(),
            AgentRecord {
                id: id.clone(),
                parent_session_id: parent_session_id.to_string(),
                parent_agent_id,
                root_session_id,
                child_session_id: None,
                depth,
                max_depth,
                profile,
                task: None,
                handle: None,
                current_turn: None,
                state: AgentState::Starting,
                last_result: None,
                usage: Usage::default(),
                warnings: Vec::new(),
                created_at: now,
                updated_at: now,
            },
        );
        drop(state);
        self.wake();
        Ok(LaunchTicket { id, depth })
    }

    pub(crate) fn cancel_launch(&self, id: &str) {
        let mut state = self.lock();
        let Some(agent) = state.agents.remove(id) else {
            return;
        };
        if let Some(child_session_id) = agent.child_session_id {
            state.assignments.remove(&child_session_id);
        }
        if let Some(budget) = state.roots.get_mut(&agent.root_session_id) {
            budget.spawns = budget.spawns.saturating_sub(1);
        }
        drop(state);
        self.wake();
    }

    pub(crate) fn attach_handle(
        &self,
        owner: &str,
        id: &str,
        handle: IsolatedSessionHandle,
    ) -> Result<IsolatedSessionTurnHandle, LaunchError> {
        let turn = handle.initial_turn();
        let mut state = self.lock();
        let agent = state
            .agents
            .get_mut(id)
            .filter(|agent| agent.parent_session_id == owner)
            .ok_or_else(|| LaunchError::NotOwned(id.to_string()))?;
        agent.handle = Some(handle);
        agent.current_turn = Some(turn.clone());
        agent.state = AgentState::Running;
        agent.updated_at = now_ms();
        drop(state);
        self.wake();
        Ok(turn)
    }

    pub(crate) fn child_session_id(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Option<String>, LaunchError> {
        let state = self.lock();
        let agent = state
            .agents
            .get(id)
            .filter(|agent| agent.parent_session_id == owner)
            .ok_or_else(|| LaunchError::NotOwned(id.to_string()))?;
        Ok(agent.child_session_id.clone())
    }

    pub(crate) fn bind_child(
        &self,
        id: &str,
        child_session_id: &str,
    ) -> Result<SubagentProfile, LaunchError> {
        let mut state = self.lock();
        let agent = state
            .agents
            .get_mut(id)
            .ok_or_else(|| LaunchError::UnknownAgent(id.to_string()))?;
        if agent
            .child_session_id
            .as_deref()
            .is_some_and(|bound| bound != child_session_id)
        {
            return Err(LaunchError::AlreadyBound(id.to_string()));
        }
        agent.child_session_id = Some(child_session_id.to_string());
        let profile = agent.profile.clone();
        state
            .assignments
            .insert(child_session_id.to_string(), id.to_string());
        drop(state);
        self.wake();
        Ok(profile)
    }

    pub(crate) fn assignment_for_session(
        &self,
        session_id: &str,
    ) -> Option<(String, SubagentProfile)> {
        let state = self.lock();
        let id = state.assignments.get(session_id)?;
        let profile = state.agents.get(id)?.profile.clone();
        Some((id.clone(), profile))
    }

    pub(crate) fn profile_for_run(&self, id: &str) -> Option<SubagentProfile> {
        self.lock()
            .agents
            .get(id)
            .map(|agent| agent.profile.clone())
    }

    pub(crate) fn record_warnings(&self, id: &str, warnings: Vec<String>) {
        if let Some(agent) = self.lock().agents.get_mut(id) {
            agent.warnings = warnings;
        }
        self.wake();
    }

    pub(crate) fn spawn_monitor(
        &self,
        owner: String,
        agent_id: String,
        turn: IsolatedSessionTurnHandle,
        timeout: Option<Duration>,
    ) {
        let weak = self.downgrade();
        let turn_id = turn.id().as_str().to_string();
        let monitor_id = format!("{agent_id}:{turn_id}");
        let monitor_agent_id = agent_id.clone();
        let task = tokio::spawn(async move {
            let result = AssertUnwindSafe(wait_for_turn(turn.clone(), timeout))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("agent monitor panicked".to_string()));
            if let Some(runtime) = weak.upgrade() {
                runtime.complete_turn(&monitor_agent_id, &turn_id, result);
            }
        });
        let abort = task.abort_handle();
        let mut monitors = self
            .inner
            .monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        monitors.retain(|_, (_, task)| !task.abort.is_finished());
        monitors.insert(
            monitor_id,
            (
                owner,
                Arc::new(MonitorTask {
                    task: tokio::sync::Mutex::new(Some(task)),
                    abort,
                }),
            ),
        );
    }

    pub(crate) async fn follow_up(
        &self,
        owner: &str,
        id: &str,
        task: String,
    ) -> Result<(bool, String), String> {
        let (handle, profile_timeout, reserved_state) = {
            let mut state = self.lock();
            let should_reserve = owned_agent(&state, owner, id)?.state.is_settled();
            if should_reserve {
                let active = state
                    .agents
                    .values()
                    .filter(|agent| agent.state.is_active())
                    .count();
                if active >= self.inner.limits.max_active_agents {
                    return Err(LaunchError::Concurrency {
                        active,
                        maximum: self.inner.limits.max_active_agents,
                    }
                    .to_string());
                }
            }
            let agent = owned_agent_mut(&mut state, owner, id)?;
            let handle = agent
                .handle
                .clone()
                .ok_or_else(|| LaunchError::NotLaunched(id.to_string()).to_string())?;
            let profile_timeout = agent.profile.timeout;
            let reserved_state = should_reserve.then_some(agent.state);
            if should_reserve {
                agent.state = AgentState::Starting;
                agent.updated_at = now_ms();
            }
            (handle, profile_timeout, reserved_state)
        };
        if reserved_state.is_some() {
            self.wake();
        }
        let follow_up = match handle.follow_up(CustomMessageContent::Text(task)).await {
            Ok(follow_up) => follow_up,
            Err(error) => {
                if let Some(previous) = reserved_state {
                    let mut state = self.lock();
                    if let Ok(agent) = owned_agent_mut(&mut state, owner, id)
                        && agent.state == AgentState::Starting
                        && agent.current_turn.is_none()
                    {
                        agent.state = previous;
                        agent.updated_at = now_ms();
                    }
                    drop(state);
                    self.wake();
                }
                return Err(error.to_string());
            }
        };
        let turn_id = follow_up.receipt.turn_id.as_str().to_string();
        if follow_up.receipt.started || reserved_state.is_some() {
            let mut state = self.lock();
            let agent = owned_agent_mut(&mut state, owner, id)?;
            agent.current_turn = Some(follow_up.turn.clone());
            agent.state = AgentState::Running;
            agent.updated_at = now_ms();
            drop(state);
            self.wake();
            self.spawn_monitor(
                owner.to_string(),
                id.to_string(),
                follow_up.turn,
                profile_timeout,
            );
        }
        Ok((follow_up.receipt.started, turn_id))
    }

    pub(crate) fn send_message(
        &self,
        sender_session_id: &str,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        if target == "parent" {
            return self.send_to_parent(sender_session_id, message);
        }
        let (handle, child_session_id, agent_id) = {
            let state = self.lock();
            let agent = owned_agent(&state, sender_session_id, target)?;
            (
                agent
                    .handle
                    .clone()
                    .ok_or_else(|| LaunchError::NotLaunched(target.to_string()).to_string())?,
                agent.child_session_id.clone(),
                agent.id.clone(),
            )
        };
        if let Some(child_session_id) = child_session_id {
            let mut state = self.lock();
            if state.waiters.get(&child_session_id).copied().unwrap_or(0) > 0 {
                state
                    .inboxes
                    .entry(child_session_id)
                    .or_default()
                    .push(AgentMessage {
                        id: Uuid::now_v7().to_string(),
                        from: "parent".to_string(),
                        message,
                        created_at: now_ms(),
                    });
                drop(state);
                self.wake();
                return Ok(json!({"target":agent_id,"acceptedAs":"wait_mailbox"}));
            }
        }
        let receipt = handle
            .send_message(CustomMessageContent::Text(message))
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "target": agent_id,
            "acceptedAs": match receipt.accepted_as {
                IsolatedMessageDelivery::Steer => "steer",
                IsolatedMessageDelivery::Mailbox => "mailbox",
            },
            "turnId": receipt.turn_id.map(|id| id.as_str().to_string()),
        }))
    }

    fn send_to_parent(&self, sender_session_id: &str, message: String) -> Result<Value, String> {
        let (agent_id, parent_session_id, parent_handle, parent_waiting) = {
            let state = self.lock();
            let agent_id = state.assignments.get(sender_session_id).ok_or_else(|| {
                "target \"parent\" is available only inside an assigned agent".to_string()
            })?;
            let agent = state
                .agents
                .get(agent_id)
                .ok_or_else(|| LaunchError::UnknownAgent(agent_id.clone()).to_string())?;
            (
                agent.id.clone(),
                agent.parent_session_id.clone(),
                state
                    .sessions
                    .get(&agent.parent_session_id)
                    .map(|registered| registered.session.handle_for_adapter()),
                state
                    .waiters
                    .get(&agent.parent_session_id)
                    .copied()
                    .unwrap_or(0)
                    > 0,
            )
        };
        if parent_waiting {
            self.lock()
                .inboxes
                .entry(parent_session_id)
                .or_default()
                .push(AgentMessage {
                    id: Uuid::now_v7().to_string(),
                    from: agent_id.clone(),
                    message,
                    created_at: now_ms(),
                });
            self.wake();
            return Ok(json!({"target":"parent","acceptedAs":"wait_mailbox"}));
        }
        let handle =
            parent_handle.ok_or_else(|| "parent session is no longer available".to_string())?;
        send_custom_message(
            &handle,
            CustomMessageInput {
                custom_type: "agent_message".into(),
                content: CustomMessageContent::Text(format!(
                    "Message from agent {agent_id}:\n{message}"
                )),
                display: true,
                details: Some(json!({"from":agent_id,"message":message})),
            },
            true,
        )?;
        Ok(json!({"target":"parent","acceptedAs":"steer"}))
    }

    pub(crate) fn interrupt(&self, owner: &str, id: &str) -> Result<AgentSnapshot, String> {
        let turn = {
            let mut state = self.lock();
            let agent = owned_agent_mut(&mut state, owner, id)?;
            let Some(turn) = agent
                .current_turn
                .clone()
                .filter(|_| agent.state.is_active())
            else {
                return Ok(snapshot(agent));
            };
            agent.state = AgentState::Interrupting;
            agent.updated_at = now_ms();
            turn
        };
        turn.abort().map_err(|error| error.to_string())?;
        self.wake();
        let state = self.lock();
        Ok(snapshot(owned_agent(&state, owner, id)?))
    }

    pub(crate) fn list(&self, owner: &str) -> Vec<AgentSnapshot> {
        let state = self.lock();
        let owner_agent = state.assignments.get(owner).cloned();
        let mut agents = state
            .agents
            .values()
            .filter(|agent| match &owner_agent {
                Some(owner_agent) => is_descendant(&state, agent, owner_agent),
                None => agent.root_session_id == owner,
            })
            .map(snapshot)
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        agents
    }

    pub(crate) fn validate_targets(&self, owner: &str, targets: &[String]) -> Result<(), String> {
        let state = self.lock();
        for id in targets {
            if id == "parent" {
                if !state.assignments.contains_key(owner) {
                    return Err(
                        "target \"parent\" is available only inside an assigned agent".into(),
                    );
                }
                continue;
            }
            owned_agent(&state, owner, id)?;
        }
        Ok(())
    }

    pub(crate) fn register_wait(
        &self,
        session_id: &str,
    ) -> (WaitRegistration, watch::Receiver<u64>) {
        *self
            .lock()
            .waiters
            .entry(session_id.to_string())
            .or_default() += 1;
        (
            WaitRegistration {
                runtime: self.downgrade(),
                session_id: session_id.to_string(),
            },
            self.subscribe(),
        )
    }

    pub(crate) fn evaluate_wait(
        &self,
        owner: &str,
        targets: &[String],
        mode: WaitMode,
    ) -> Result<WaitEvaluation, String> {
        let mut state = self.lock();
        let agents = targets
            .iter()
            .filter(|id| id.as_str() != "parent")
            .map(|id| owned_agent(&state, owner, id).map(snapshot))
            .collect::<Result<Vec<_>, _>>()?;
        let inbox = state.inboxes.entry(owner.to_string()).or_default();
        let (messages, retained): (Vec<_>, Vec<_>) = std::mem::take(inbox)
            .into_iter()
            .partition(|message| targets.iter().any(|id| id == &message.from));
        *inbox = retained;
        if !messages.is_empty() {
            return Ok(WaitEvaluation::Ready { agents, messages });
        }
        let settled = agents
            .iter()
            .filter(|agent| agent.state.is_settled())
            .count();
        let waits_for_parent = targets.iter().any(|id| id == "parent");
        let ready = match mode {
            WaitMode::Any => settled > 0,
            WaitMode::All => !waits_for_parent && settled == agents.len(),
        };
        if ready {
            Ok(WaitEvaluation::Ready { agents, messages })
        } else {
            Ok(WaitEvaluation::Pending)
        }
    }

    pub(crate) async fn drain_monitors(&self, owner: &str) {
        let tasks = self
            .inner
            .monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, (task_owner, _))| task_owner == owner)
            .map(|(id, (_, task))| (id.clone(), Arc::clone(task)))
            .collect::<Vec<_>>();
        for (id, task) in tasks {
            task.drain().await;
            self.inner
                .monitors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
        }
    }

    pub(crate) fn close_owner(&self, owner: &str) {
        let handles = {
            let state = self.lock();
            state
                .agents
                .values()
                .filter(|agent| {
                    agent.state.is_active()
                        && (agent.parent_session_id == owner || agent.root_session_id == owner)
                })
                .filter_map(|agent| agent.current_turn.clone())
                .collect::<Vec<_>>()
        };
        for handle in handles {
            let _ = handle.abort();
        }
    }

    pub(crate) fn forget_session(&self, session_id: &str) {
        let mut state = self.lock();
        state.sessions.remove(session_id);
        state.inboxes.remove(session_id);
        state.waiters.remove(session_id);
        let closing_root = !state.assignments.contains_key(session_id);
        let removed = state
            .agents
            .values()
            .filter(|agent| {
                if closing_root {
                    agent.root_session_id == session_id
                } else {
                    agent.parent_session_id == session_id
                        || agent.child_session_id.as_deref() == Some(session_id)
                }
            })
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(agent) = state.agents.remove(&id)
                && let Some(child_session_id) = agent.child_session_id
            {
                state.assignments.remove(&child_session_id);
            }
        }
        state.assignments.remove(session_id);
        if closing_root {
            state.roots.remove(session_id);
        }
        drop(state);
        self.inner
            .desktop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forget(session_id);
        self.wake();
    }

    fn downgrade(&self) -> WeakSubagentRuntime {
        WeakSubagentRuntime(Arc::downgrade(&self.inner))
    }

    fn complete_turn(&self, id: &str, turn_id: &str, result: Result<TurnCompletion, String>) {
        let (parent_handle, usage) = {
            let mut state = self.lock();
            let Some(agent) = state.agents.get_mut(id) else {
                return;
            };
            if agent.current_turn.as_ref().map(|turn| turn.id().as_str()) != Some(turn_id) {
                return;
            }
            let completion = result.unwrap_or_else(|error| TurnCompletion {
                outcome: None,
                state: AgentState::Failed,
                text: error,
            });
            let usage = completion
                .outcome
                .as_ref()
                .map(|outcome| outcome.usage.clone())
                .unwrap_or_default();
            add_usage(&mut agent.usage, &usage);
            agent.state = completion.state;
            agent.current_turn = None;
            agent.updated_at = now_ms();
            agent.last_result = Some(AgentTurnResult {
                turn_id: turn_id.to_string(),
                text: completion.text.clone(),
                is_error: completion.state != AgentState::Idle,
                aborted: completion
                    .outcome
                    .as_ref()
                    .is_some_and(|outcome| outcome.aborted),
                usage: usage.clone(),
            });
            let owner = agent.parent_session_id.clone();
            (
                state
                    .sessions
                    .get(&owner)
                    .map(|registered| registered.session.clone()),
                usage,
            )
        };
        self.wake();
        if usage != Usage::default()
            && let Some(session) = parent_handle.as_ref()
        {
            let _ = session.record_usage(
                usage,
                Some(json!({"source":"agent","agentId":id,"turnId":turn_id})),
            );
        }
    }
}

struct TurnCompletion {
    outcome: Option<IsolatedSessionOutcome>,
    state: AgentState,
    text: String,
}

async fn wait_for_turn(
    turn: IsolatedSessionTurnHandle,
    timeout: Option<Duration>,
) -> Result<TurnCompletion, String> {
    let outcome = if let Some(timeout) = timeout {
        match tokio::time::timeout(timeout, turn.wait()).await {
            Ok(result) => result.map_err(|error| error.to_string())?,
            Err(_) => {
                turn.abort().map_err(|error| error.to_string())?;
                let outcome = turn.wait().await.map_err(|error| error.to_string())?;
                return Ok(TurnCompletion {
                    outcome: Some(outcome),
                    state: AgentState::TimedOut,
                    text: format!("Agent turn timed out after {} ms.", timeout.as_millis()),
                });
            }
        }
    } else {
        turn.wait().await.map_err(|error| error.to_string())?
    };
    let failure = outcome
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            pi_core::Message::Assistant(message) => Some(message),
            _ => None,
        })
        .filter(|message| message.stop_reason == pi_core::StopReason::Error)
        .and_then(|message| message.error_message.clone());
    let text = failure
        .clone()
        .unwrap_or_else(|| final_text(&outcome.messages));
    let state = if outcome.aborted {
        AgentState::Interrupted
    } else if failure.is_some() {
        AgentState::Failed
    } else {
        AgentState::Idle
    };
    Ok(TurnCompletion {
        outcome: Some(outcome),
        state,
        text,
    })
}

fn owned_agent<'a>(
    state: &'a RuntimeState,
    owner: &str,
    id: &str,
) -> Result<&'a AgentRecord, String> {
    state
        .agents
        .get(id)
        .ok_or_else(|| LaunchError::UnknownAgent(id.to_string()).to_string())
        .and_then(|agent| {
            (agent.parent_session_id == owner)
                .then_some(agent)
                .ok_or_else(|| LaunchError::NotOwned(id.to_string()).to_string())
        })
}

fn owned_agent_mut<'a>(
    state: &'a mut RuntimeState,
    owner: &str,
    id: &str,
) -> Result<&'a mut AgentRecord, String> {
    state
        .agents
        .get_mut(id)
        .ok_or_else(|| LaunchError::UnknownAgent(id.to_string()).to_string())
        .and_then(|agent| {
            (agent.parent_session_id == owner)
                .then_some(agent)
                .ok_or_else(|| LaunchError::NotOwned(id.to_string()).to_string())
        })
}

fn is_descendant(state: &RuntimeState, candidate: &AgentRecord, ancestor_id: &str) -> bool {
    let mut parent = candidate.parent_agent_id.as_deref();
    while let Some(id) = parent {
        if id == ancestor_id {
            return true;
        }
        parent = state
            .agents
            .get(id)
            .and_then(|agent| agent.parent_agent_id.as_deref());
    }
    false
}

fn snapshot(agent: &AgentRecord) -> AgentSnapshot {
    AgentSnapshot {
        id: agent.id.clone(),
        agent: agent.profile.name.clone(),
        parent_agent_id: agent.parent_agent_id.clone(),
        child_session_id: agent.child_session_id.clone(),
        depth: agent.depth,
        state: agent.state,
        current_turn_id: agent
            .current_turn
            .as_ref()
            .map(|turn| turn.id().as_str().to_string()),
        last_result: agent
            .last_result
            .as_ref()
            .and_then(|result| serde_json::to_value(result).ok()),
        usage: agent.usage.clone(),
        warnings: agent.warnings.clone(),
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

fn send_custom_message(
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

fn add_usage(total: &mut Usage, usage: &Usage) {
    total.input = total.input.saturating_add(usage.input);
    total.output = total.output.saturating_add(usage.output);
    total.cache_read = total.cache_read.saturating_add(usage.cache_read);
    total.cache_write = total.cache_write.saturating_add(usage.cache_write);
    total.cache_write_1h = add_optional(total.cache_write_1h, usage.cache_write_1h);
    total.reasoning = add_optional(total.reasoning, usage.reasoning);
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
    total.cost.input += usage.cost.input;
    total.cost.output += usage.cost.output;
    total.cost.cache_read += usage.cost.cache_read;
    total.cost.cache_write += usage.cost.cache_write;
    total.cost.total += usage.cost.total;
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(
            left.unwrap_or_default()
                .saturating_add(right.unwrap_or_default()),
        ),
    }
}

fn final_text(messages: &[pi_core::Message]) -> String {
    messages
        .iter()
        .rev()
        .filter_map(|message| match message {
            pi_core::Message::Assistant(message) => Some(message),
            _ => None,
        })
        .find_map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|content| match content {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        })
        .unwrap_or_else(|| "Agent completed without a textual response.".to_string())
}

pub(crate) fn run_marker(text: &str) -> Option<&str> {
    let first_line = text.lines().next()?;
    first_line
        .strip_prefix(MARKER_PREFIX)?
        .strip_suffix(MARKER_SUFFIX)
        .filter(|id| !id.is_empty())
}

fn marker(id: &str) -> String {
    format!("{MARKER_PREFIX}{id}{MARKER_SUFFIX}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

pub(crate) fn result_with_details(text: impl Into<String>, details: Value) -> ToolResult {
    let mut result = ToolResult::text(text.into());
    result.details = Some(details);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::builtin_profile;

    fn runtime(max_depth: usize, max_spawns: usize, max_active: usize) -> SubagentRuntime {
        SubagentRuntime::with_limits(RuntimeLimits {
            max_depth,
            max_spawns_per_root: max_spawns,
            max_active_agents: max_active,
        })
    }

    #[test]
    fn admission_enforces_depth_spawn_and_active_limits() {
        let runtime = runtime(2, 2, 1);
        let first = runtime
            .begin_launch("root", builtin_profile("delegate"), 2)
            .unwrap();
        assert!(matches!(
            runtime.begin_launch("root", builtin_profile("delegate"), 2),
            Err(LaunchError::Concurrency { .. })
        ));
        runtime.cancel_launch(first.id());
        runtime
            .begin_launch("root", builtin_profile("delegate"), 2)
            .unwrap();
    }

    #[test]
    fn markers_are_exact_first_line_metadata() {
        let id = Uuid::now_v7().to_string();
        let prompt = format!("{}\nwork", marker(&id));
        assert_eq!(run_marker(&prompt), Some(id.as_str()));
        assert_eq!(run_marker("work"), None);
    }
}
