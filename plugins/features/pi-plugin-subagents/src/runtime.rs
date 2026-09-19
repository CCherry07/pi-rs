use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures::FutureExt;
use pi_core::{ContentBlock, CustomMessageContent, CustomMessageInput, Message, ToolResult, Usage};
use pi_plugin::{
    IsolatedMessageDelivery, IsolatedSessionHandle, IsolatedSessionOutcome, IsolatedSessionRequest,
    IsolatedSessionTurnHandle, PluginContextHandle, SendMessageOptions, SessionContext,
    SessionSnapshot,
};
use pi_utils::time::unix_timestamp_ms_u64 as now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

use crate::collaboration::{
    COLLABORATION_ENTRY_TYPE, CollaborationMessage, consumed_event_ids_from_messages,
    consumed_event_ids_from_records, message_event_data, messages_for_recipient,
    projection_details, remove_consumed_projections,
};
use crate::profiles::SubagentProfile;

pub(crate) const DEFAULT_MAX_DEPTH: usize = 4;
const DEFAULT_MAX_SPAWNS_PER_ROOT: usize = 64;
const DEFAULT_MAX_ACTIVE_AGENTS: usize = 8;
const MAX_TASK_REPORT_BYTES: usize = 16 * 1024;
const RECOVERY_ENTRY_TYPE: &str = "pi.subagents.agent";
const RECOVERY_VERSION: u64 = 1;
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
    checkpointing: bool,
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
    observed_messages: HashMap<String, HashSet<String>>,
    active_waits: HashMap<String, usize>,
    recovering_roots: HashSet<String>,
    next_submission_seq: u64,
}

#[derive(Clone)]
struct OwnerSession {
    session: SessionContext,
    pi_session: Option<pi_session::WeakPiSession>,
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
    child_path: Option<PathBuf>,
    depth: usize,
    max_depth: usize,
    profile: SubagentProfile,
    task: Option<String>,
    pending_request: Option<IsolatedSessionRequest>,
    detached: bool,
    handle: Option<IsolatedSessionHandle>,
    current_turn: Option<IsolatedSessionTurnHandle>,
    state: AgentState,
    last_report: Option<AgentTaskReport>,
    usage: Usage,
    warnings: Vec<String>,
    submission_seq: u64,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentState {
    Queued,
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
        matches!(
            self,
            Self::Idle | Self::Failed | Self::Interrupted | Self::TimedOut
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskOutcome {
    Succeeded,
    Failed,
    Interrupted,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentTaskReport {
    pub(crate) turn_id: Option<String>,
    pub(crate) outcome: TaskOutcome,
    pub(crate) summary: String,
    pub(crate) truncated: bool,
    usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedAgent {
    id: String,
    parent_session_id: String,
    parent_agent_id: Option<String>,
    root_session_id: String,
    child_session_id: Option<String>,
    child_path: Option<PathBuf>,
    depth: usize,
    max_depth: usize,
    profile: SubagentProfile,
    task: Option<String>,
    pending_request: Option<IsolatedSessionRequest>,
    detached: bool,
    state: AgentState,
    current_turn_id: Option<String>,
    last_report: Option<AgentTaskReport>,
    usage: Usage,
    warnings: Vec<String>,
    submission_seq: u64,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentCheckpoint {
    version: u64,
    root_session_id: String,
    agent_id: String,
    agent: Option<PersistedAgent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentSnapshot {
    pub id: String,
    pub agent: String,
    pub parent_agent_id: Option<String>,
    pub child_session_id: Option<String>,
    pub depth: usize,
    pub detached: bool,
    pub state: AgentState,
    pub current_turn_id: Option<String>,
    pub last_report: Option<AgentTaskReport>,
    pub usage: Usage,
    pub warnings: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AgentState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Interrupting => "interrupting",
            Self::Idle => "idle",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::TimedOut => "timed_out",
        }
    }
}

impl TaskOutcome {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::TimedOut => "timed_out",
        }
    }
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
        messages: Vec<CollaborationMessage>,
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
    #[error("agent {0} was cancelled before launch completed")]
    LaunchCancelled(String),
    #[error("agent {0} is already bound to another child session")]
    AlreadyBound(String),
    #[error("subagent recovery checkpoint failed: {0}")]
    Persistence(String),
}

#[derive(Debug)]
pub(crate) struct LaunchTicket {
    id: String,
    depth: usize,
    state: AgentState,
}

impl LaunchTicket {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn state(&self) -> AgentState {
        self.state
    }

    pub(crate) fn is_queued(&self) -> bool {
        self.state == AgentState::Queued
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
        if let Some(count) = state.active_waits.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.active_waits.remove(&self.session_id);
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
        Self::with_limits_and_checkpointing(limits, true)
    }

    fn with_limits_and_checkpointing(limits: RuntimeLimits, checkpointing: bool) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                runtime_id: Uuid::now_v7().to_string(),
                limits,
                checkpointing,
                state: Mutex::new(RuntimeState::default()),
                changed: watch::channel(0).0,
                monitors: Mutex::new(HashMap::new()),
                desktop: Mutex::new(crate::desktop::DesktopState::default()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn without_persistence_for_testing() -> Self {
        Self::with_limits_and_checkpointing(
            RuntimeLimits {
                max_depth: DEFAULT_MAX_DEPTH,
                max_spawns_per_root: DEFAULT_MAX_SPAWNS_PER_ROOT,
                max_active_agents: DEFAULT_MAX_ACTIVE_AGENTS,
            },
            false,
        )
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

    fn checkpoint_agent(&self, id: &str) -> Result<(), String> {
        if !self.inner.checkpointing {
            return Ok(());
        }
        let (context, checkpoint) = {
            let state = self.lock();
            let agent = state
                .agents
                .get(id)
                .ok_or_else(|| LaunchError::UnknownAgent(id.to_string()).to_string())?;
            let context = state
                .sessions
                .get(&agent.root_session_id)
                .map(|registered| registered.session.clone())
                .ok_or_else(|| {
                    format!(
                        "root session {} is unavailable for subagent recovery checkpoint",
                        agent.root_session_id
                    )
                })?;
            (
                context,
                AgentCheckpoint {
                    version: RECOVERY_VERSION,
                    root_session_id: agent.root_session_id.clone(),
                    agent_id: agent.id.clone(),
                    agent: Some(PersistedAgent::from(agent)),
                },
            )
        };
        let data = serde_json::to_value(checkpoint).map_err(|error| error.to_string())?;
        context
            .append_entry(RECOVERY_ENTRY_TYPE, Some(data))
            .map_err(|error| error.to_string())
    }

    fn checkpoint_tombstone(
        &self,
        context: SessionContext,
        root_session_id: String,
        agent_id: String,
    ) -> Result<(), String> {
        if !self.inner.checkpointing {
            return Ok(());
        }
        let data = serde_json::to_value(AgentCheckpoint {
            version: RECOVERY_VERSION,
            root_session_id,
            agent_id,
            agent: None,
        })
        .map_err(|error| error.to_string())?;
        context
            .append_entry(RECOVERY_ENTRY_TYPE, Some(data))
            .map_err(|error| error.to_string())
    }

    fn install_recovery_state(&self, root_session_id: &str) -> Result<bool, String> {
        let snapshot = {
            let state = self.lock();
            if state.recovering_roots.contains(root_session_id)
                || state
                    .agents
                    .values()
                    .any(|agent| agent.root_session_id == root_session_id)
            {
                return Ok(false);
            }
            let Some(root) = state.sessions.get(root_session_id) else {
                return Ok(false);
            };
            root.session.snapshot().map_err(|error| error.to_string())?
        };
        let mut recovered = recovery_agents(&snapshot)?;
        if recovered.is_empty() {
            return Ok(false);
        }
        if recovered.len() > self.inner.limits.max_spawns_per_root {
            return Err(format!(
                "subagent recovery contains {} agents, exceeding the root limit {}",
                recovered.len(),
                self.inner.limits.max_spawns_per_root
            ));
        }
        reconcile_recovered_agents(&mut recovered);
        validate_recovered_agents(root_session_id, &recovered)?;

        let mut state = self.lock();
        if state.recovering_roots.contains(root_session_id)
            || state
                .agents
                .values()
                .any(|agent| agent.root_session_id == root_session_id)
        {
            return Ok(false);
        }
        let mut next_submission_seq = state.next_submission_seq;
        for persisted in recovered {
            next_submission_seq = next_submission_seq.max(persisted.submission_seq.wrapping_add(1));
            if persisted.child_path.is_some()
                && let Some(child_session_id) = &persisted.child_session_id
            {
                state
                    .assignments
                    .insert(child_session_id.clone(), persisted.id.clone());
            }
            state.agents.insert(
                persisted.id.clone(),
                AgentRecord {
                    id: persisted.id,
                    parent_session_id: persisted.parent_session_id,
                    parent_agent_id: persisted.parent_agent_id,
                    root_session_id: persisted.root_session_id,
                    child_session_id: persisted.child_session_id,
                    child_path: persisted.child_path,
                    depth: persisted.depth,
                    max_depth: persisted.max_depth,
                    profile: persisted.profile,
                    task: persisted.task,
                    pending_request: persisted.pending_request,
                    detached: persisted.detached,
                    handle: None,
                    current_turn: None,
                    state: persisted.state,
                    last_report: persisted.last_report,
                    usage: persisted.usage,
                    warnings: persisted.warnings,
                    submission_seq: persisted.submission_seq,
                    created_at: persisted.created_at,
                    updated_at: persisted.updated_at,
                },
            );
        }
        state.next_submission_seq = next_submission_seq;
        state
            .roots
            .entry(root_session_id.to_string())
            .or_default()
            .spawns = state
            .agents
            .values()
            .filter(|agent| agent.root_session_id == root_session_id)
            .count();
        state.recovering_roots.insert(root_session_id.to_string());
        let recovered_ids = state
            .agents
            .values()
            .filter(|agent| agent.root_session_id == root_session_id)
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        drop(state);
        for id in recovered_ids {
            let _ = self.checkpoint_agent(&id);
        }
        self.wake();
        Ok(true)
    }

    async fn restore_root(&self, root_session_id: &str) {
        let mut recoverable = {
            let state = self.lock();
            state
                .agents
                .values()
                .filter(|agent| {
                    agent.root_session_id == root_session_id
                        && agent.child_path.is_some()
                        && agent.child_session_id.is_some()
                })
                .map(|agent| {
                    (
                        agent.depth,
                        agent.submission_seq,
                        agent.id.clone(),
                        agent.parent_session_id.clone(),
                        agent.child_session_id.clone().expect("filtered child id"),
                        agent.child_path.clone().expect("filtered child path"),
                    )
                })
                .collect::<Vec<_>>()
        };
        recoverable.sort_by_key(|(depth, submission, ..)| (*depth, *submission));

        for (_, _, agent_id, parent_session_id, child_session_id, child_path) in recoverable {
            let parent = self
                .lock()
                .sessions
                .get(&parent_session_id)
                .and_then(|registered| registered.pi_session.as_ref())
                .and_then(pi_session::WeakPiSession::upgrade);
            let result = match parent {
                Some(parent) => match pi_session::SessionLog::open(&child_path) {
                    Ok((_, document)) if document.header.id == child_session_id => {
                        parent.restore_isolated_session(&child_path).await
                    }
                    Ok((_, document)) => Err(
                        pi_session::MultiSessionManagerError::InvalidIsolatedRequest(format!(
                            "restored child id {:?} does not match checkpoint {:?}",
                            document.header.id, child_session_id
                        )),
                    ),
                    Err(error) => Err(error.into()),
                },
                None => Err(
                    pi_session::MultiSessionManagerError::InvalidIsolatedRequest(format!(
                        "parent session {parent_session_id:?} was not restored"
                    )),
                ),
            };
            match result {
                Ok(isolated_id) => {
                    let context = self
                        .lock()
                        .sessions
                        .get(&parent_session_id)
                        .map(|registered| registered.session.clone());
                    if let Some(agent) = self.lock().agents.get_mut(&agent_id) {
                        if let Some(context) = context {
                            agent.handle = Some(context.isolated_session_handle(isolated_id));
                        }
                        agent.updated_at = now_ms();
                    }
                }
                Err(error) => {
                    if let Some(agent) = self.lock().agents.get_mut(&agent_id) {
                        agent.warnings.push(format!(
                            "agent session recovery failed; follow-up is unavailable: {error}"
                        ));
                        agent.updated_at = now_ms();
                    }
                }
            }
            let _ = self.checkpoint_agent(&agent_id);
            self.wake();
        }

        let mut queued = {
            let state = self.lock();
            state
                .agents
                .values()
                .filter(|agent| {
                    agent.root_session_id == root_session_id && agent.state == AgentState::Queued
                })
                .map(|agent| {
                    (
                        agent.submission_seq,
                        agent.id.clone(),
                        agent.parent_session_id.clone(),
                        agent.pending_request.clone(),
                        agent.profile.timeout,
                    )
                })
                .collect::<Vec<_>>()
        };
        queued.sort_by_key(|(submission, ..)| *submission);
        for (_, agent_id, owner, request, timeout) in queued {
            let session = self
                .lock()
                .sessions
                .get(&owner)
                .map(|registered| registered.session.clone());
            match (session, request) {
                (Some(session), Some(request)) => {
                    if let Err(error) =
                        self.spawn_queued_launch(owner, agent_id.clone(), session, request, timeout)
                    {
                        if let Some(agent) = self.lock().agents.get_mut(&agent_id) {
                            reconcile_as_interrupted(
                                agent,
                                &format!("Queued agent recovery failed before launch: {error}"),
                            );
                        }
                        let _ = self.checkpoint_agent(&agent_id);
                    }
                }
                _ => {
                    if let Some(agent) = self.lock().agents.get_mut(&agent_id) {
                        reconcile_as_interrupted(
                            agent,
                            "Queued agent could not be restored because its owner or request was unavailable.",
                        );
                    }
                    let _ = self.checkpoint_agent(&agent_id);
                }
            }
        }
        self.lock().recovering_roots.remove(root_session_id);
        self.wake();
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
        let mut state = self.lock();
        let pi_session = state
            .sessions
            .get(&id)
            .and_then(|registered| registered.pi_session.clone());
        state.sessions.insert(
            id,
            OwnerSession {
                session,
                pi_session,
            },
        );
        drop(state);
        self.wake();
    }

    /// Completes outer-session registration and starts any durable recovery
    /// owned by this session. Calling this again after a session replacement
    /// rebinds the same frontend handle to the newly published generation.
    pub fn session_registered(&self, session: pi_session::PiSession) {
        let session_id = session.id();
        let context = session.current().runtime().context_parts().session;
        self.bind_session(session_id.clone(), context);
        if let Some(registered) = self.lock().sessions.get_mut(&session_id) {
            registered.pi_session = Some(session.downgrade());
        }
        match self.install_recovery_state(&session_id) {
            Ok(true) => {
                let runtime = self.clone();
                tokio::spawn(async move {
                    runtime.restore_root(&session_id).await;
                });
            }
            Ok(false) => {}
            Err(error) => session.current().notify_plugin(
                format!("Subagent recovery state was ignored: {error}"),
                pi_session::NoticeLevel::Warning,
            ),
        }
    }

    pub(crate) fn suspend_desktop(&self, id: &str) {
        // Serializes with publication so shutdown cannot race a captured old context.
        self.inner
            .desktop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .suspend(id);
    }

    pub(crate) fn set_task(&self, id: &str, task: &str) -> Result<(), LaunchError> {
        if let Some(agent) = self.lock().agents.get_mut(id) {
            agent.task = Some(task.to_string());
        } else {
            return Err(LaunchError::UnknownAgent(id.to_string()));
        }
        self.checkpoint_agent(id)
            .map_err(LaunchError::Persistence)?;
        self.wake();
        Ok(())
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
        detached: bool,
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
        let active = active_agents_for_root(&state, &root_session_id);
        let budget = state.roots.entry(root_session_id.clone()).or_default();
        if budget.spawns >= self.inner.limits.max_spawns_per_root {
            return Err(LaunchError::SpawnBudget {
                used: budget.spawns,
                maximum: self.inner.limits.max_spawns_per_root,
            });
        }
        budget.spawns += 1;

        let id = Uuid::now_v7().to_string();
        let submission_seq = state.next_submission_seq;
        state.next_submission_seq = state.next_submission_seq.wrapping_add(1);
        let depth = parent_depth + 1;
        let max_depth = profile
            .max_subagent_depth
            .map_or(inherited_max_depth, |maximum| {
                inherited_max_depth.min(maximum)
            });
        let now = now_ms();
        let initial_state = if active < self.inner.limits.max_active_agents {
            AgentState::Starting
        } else {
            AgentState::Queued
        };
        state.agents.insert(
            id.clone(),
            AgentRecord {
                id: id.clone(),
                parent_session_id: parent_session_id.to_string(),
                parent_agent_id,
                root_session_id,
                child_session_id: None,
                child_path: None,
                depth,
                max_depth,
                profile,
                task: None,
                pending_request: None,
                detached,
                handle: None,
                current_turn: None,
                state: initial_state,
                last_report: None,
                usage: Usage::default(),
                warnings: Vec::new(),
                submission_seq,
                created_at: now,
                updated_at: now,
            },
        );
        drop(state);
        if let Err(error) = self.checkpoint_agent(&id) {
            self.cancel_launch(&id);
            return Err(LaunchError::Persistence(error));
        }
        self.wake();
        Ok(LaunchTicket {
            id,
            depth,
            state: initial_state,
        })
    }

    pub(crate) fn cancel_launch(&self, id: &str) {
        let mut state = self.lock();
        let Some(agent) = state.agents.remove(id) else {
            return;
        };
        let checkpoint = state
            .sessions
            .get(&agent.root_session_id)
            .map(|registered| {
                (
                    registered.session.clone(),
                    agent.root_session_id.clone(),
                    agent.id.clone(),
                )
            });
        if let Some(child_session_id) = agent.child_session_id {
            state.assignments.remove(&child_session_id);
        }
        if let Some(budget) = state.roots.get_mut(&agent.root_session_id) {
            budget.spawns = budget.spawns.saturating_sub(1);
        }
        drop(state);
        if let Some((context, root_session_id, agent_id)) = checkpoint {
            let _ = self.checkpoint_tombstone(context, root_session_id, agent_id);
        }
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
        if agent.state != AgentState::Starting {
            return Err(LaunchError::LaunchCancelled(id.to_string()));
        }
        agent.handle = Some(handle);
        agent.current_turn = Some(turn.clone());
        agent.pending_request = None;
        agent.state = AgentState::Running;
        agent.updated_at = now_ms();
        drop(state);
        self.checkpoint_agent(id)
            .map_err(LaunchError::Persistence)?;
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
        let child_path = state.sessions.get(child_session_id).and_then(|registered| {
            registered
                .pi_session
                .as_ref()
                .and_then(pi_session::WeakPiSession::upgrade)
                .map(|session| session.path())
                .or_else(|| {
                    registered
                        .session
                        .snapshot()
                        .ok()
                        .and_then(|snapshot| snapshot.file().map(Path::to_path_buf))
                })
        });
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
        agent.child_path = child_path;
        let profile = agent.profile.clone();
        state
            .assignments
            .insert(child_session_id.to_string(), id.to_string());
        drop(state);
        self.checkpoint_agent(id)
            .map_err(LaunchError::Persistence)?;
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
        let _ = self.checkpoint_agent(id);
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
        self.register_monitor(monitor_id, owner, task);
    }

    pub(crate) fn spawn_queued_launch(
        &self,
        owner: String,
        agent_id: String,
        session: SessionContext,
        request: IsolatedSessionRequest,
        timeout: Option<Duration>,
    ) -> Result<(), LaunchError> {
        let mut state = self.lock();
        let agent = state
            .agents
            .get_mut(&agent_id)
            .ok_or_else(|| LaunchError::UnknownAgent(agent_id.clone()))?;
        if agent.state != AgentState::Queued {
            return Err(LaunchError::LaunchCancelled(agent_id));
        }
        agent.pending_request = Some(request.clone());
        agent.updated_at = now_ms();
        drop(state);
        self.checkpoint_agent(&agent_id)
            .map_err(LaunchError::Persistence)?;
        self.wake();

        let weak = self.downgrade();
        let monitor_id = format!("launch:{agent_id}");
        let launch_agent_id = agent_id.clone();
        let launch_owner = owner.clone();
        let task = tokio::spawn(async move {
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            if !runtime.wait_for_admission(&launch_agent_id).await {
                return;
            }
            let handle = match session.launch_isolated_session(request).await {
                Ok(handle) => handle,
                Err(error) => {
                    runtime.fail_launch(&launch_agent_id, error.to_string());
                    return;
                }
            };
            let turn = match runtime.attach_handle(&launch_owner, &launch_agent_id, handle.clone())
            {
                Ok(turn) => turn,
                Err(error) => {
                    let _ = handle.initial_turn().abort();
                    runtime.fail_launch(&launch_agent_id, error.to_string());
                    return;
                }
            };
            let turn_id = turn.id().as_str().to_string();
            let result = AssertUnwindSafe(wait_for_turn(turn, timeout))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("agent monitor panicked".to_string()));
            runtime.complete_turn(&launch_agent_id, &turn_id, result);
        });
        self.register_monitor(monitor_id, owner, task);
        Ok(())
    }

    fn register_monitor(
        &self,
        monitor_id: String,
        owner: String,
        task: tokio::task::JoinHandle<()>,
    ) {
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

    async fn wait_for_admission(&self, id: &str) -> bool {
        let mut changed = self.subscribe();
        loop {
            let admitted = {
                let mut state = self.lock();
                let Some(agent) = state.agents.get(id) else {
                    return false;
                };
                if agent.state != AgentState::Queued {
                    return false;
                }
                let root = agent.root_session_id.clone();
                let first_queued = state
                    .agents
                    .values()
                    .filter(|candidate| {
                        candidate.root_session_id == root && candidate.state == AgentState::Queued
                    })
                    .min_by(|left, right| {
                        left.submission_seq
                            .cmp(&right.submission_seq)
                            .then_with(|| left.id.cmp(&right.id))
                    })
                    .map(|candidate| candidate.id.as_str());
                let has_capacity =
                    active_agents_for_root(&state, &root) < self.inner.limits.max_active_agents;
                if first_queued == Some(id) && has_capacity {
                    let agent = state.agents.get_mut(id).expect("queued agent still exists");
                    agent.state = AgentState::Starting;
                    agent.updated_at = now_ms();
                    true
                } else {
                    false
                }
            };
            if admitted {
                if let Err(error) = self.checkpoint_agent(id) {
                    self.fail_launch(
                        id,
                        format!("could not persist queued-agent admission: {error}"),
                    );
                    return false;
                }
                self.wake();
                return true;
            }
            if changed.changed().await.is_err() {
                return false;
            }
        }
    }

    fn fail_launch(&self, id: &str, error: String) {
        let mut state = self.lock();
        let Some(agent) = state.agents.get_mut(id) else {
            return;
        };
        if agent.state == AgentState::Interrupted {
            return;
        }
        let (summary, truncated) = bounded_task_summary(&error);
        agent.state = AgentState::Failed;
        agent.updated_at = now_ms();
        agent.last_report = Some(AgentTaskReport {
            turn_id: None,
            outcome: TaskOutcome::Failed,
            summary,
            truncated,
            usage: Usage::default(),
        });
        drop(state);
        let _ = self.checkpoint_agent(id);
        self.wake();
        self.notify_parent_if_joined(id);
    }

    pub(crate) async fn follow_up(
        &self,
        owner: &str,
        id: &str,
        task: String,
    ) -> Result<(bool, String), String> {
        self.wait_for_recovered_handle(owner, id).await?;
        let (handle, profile_timeout, reserved_state) = {
            let mut state = self.lock();
            let (should_reserve, root_session_id) = {
                let agent = owned_agent(&state, owner, id)?;
                (agent.state.is_settled(), agent.root_session_id.clone())
            };
            if should_reserve {
                let active = active_agents_for_root(&state, &root_session_id);
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
            if let Err(error) = self.checkpoint_agent(id) {
                let mut state = self.lock();
                if let Some(previous) = reserved_state
                    && let Ok(agent) = owned_agent_mut(&mut state, owner, id)
                {
                    agent.state = previous;
                    agent.updated_at = now_ms();
                }
                return Err(format!("could not persist follow-up admission: {error}"));
            }
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
                    let _ = self.checkpoint_agent(id);
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
            if let Err(error) = self.checkpoint_agent(id) {
                let _ = follow_up.turn.abort();
                return Err(format!("could not persist active follow-up: {error}"));
            }
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

    async fn wait_for_recovered_handle(&self, owner: &str, id: &str) -> Result<(), String> {
        let mut changed = self.subscribe();
        loop {
            let pending = {
                let state = self.lock();
                let agent = owned_agent(&state, owner, id)?;
                agent.handle.is_none()
                    && agent.child_path.is_some()
                    && state.recovering_roots.contains(&agent.root_session_id)
            };
            if !pending {
                return Ok(());
            }
            changed
                .changed()
                .await
                .map_err(|_| "subagent recovery runtime closed".to_string())?;
        }
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
        let (handle, child_session_id, agent_id, root_session_id, root, owner) = {
            let state = self.lock();
            let agent = owned_agent(&state, sender_session_id, target)?;
            (
                agent
                    .handle
                    .clone()
                    .ok_or_else(|| LaunchError::NotLaunched(target.to_string()).to_string())?,
                agent.child_session_id.clone(),
                agent.id.clone(),
                agent.root_session_id.clone(),
                state
                    .sessions
                    .get(&agent.root_session_id)
                    .and_then(|session| session.pi_session.as_ref())
                    .and_then(pi_session::WeakPiSession::upgrade)
                    .ok_or_else(|| "root session is no longer available".to_string())?,
                state
                    .sessions
                    .get(sender_session_id)
                    .and_then(|session| session.pi_session.as_ref())
                    .and_then(pi_session::WeakPiSession::upgrade)
                    .ok_or_else(|| "owner session is no longer available".to_string())?,
            )
        };
        let child_session_id = child_session_id
            .ok_or_else(|| LaunchError::NotLaunched(target.to_string()).to_string())?;
        let event = persist_message_event(
            &root,
            &root_session_id,
            &child_session_id,
            "parent",
            &message,
        )?;
        let receipt = owner
            .send_custom_to_isolated_session(
                handle.id(),
                CustomMessageInput {
                    custom_type: "agent_message".into(),
                    content: CustomMessageContent::Text(message),
                    display: true,
                    details: Some(projection_details(&event, &child_session_id)),
                },
            )
            .map_err(|error| format!("could not project collaboration message: {error}"))?;
        Ok(json!({
            "target": agent_id,
            "eventRecordId": event.id,
            "eventRecordSeq": event.sequence,
            "acceptedAs": match receipt.accepted_as {
                IsolatedMessageDelivery::Steer => "steer",
                IsolatedMessageDelivery::Mailbox => "mailbox",
            },
            "turnId": receipt.turn_id.map(|id| id.as_str().to_string()),
        }))
    }

    fn send_to_parent(&self, sender_session_id: &str, message: String) -> Result<Value, String> {
        let (agent_id, parent_session_id, root_session_id, root, parent) = {
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
                agent.root_session_id.clone(),
                state
                    .sessions
                    .get(&agent.root_session_id)
                    .and_then(|session| session.pi_session.as_ref())
                    .and_then(pi_session::WeakPiSession::upgrade)
                    .ok_or_else(|| "root session is no longer available".to_string())?,
                state
                    .sessions
                    .get(&agent.parent_session_id)
                    .and_then(|session| session.pi_session.as_ref())
                    .and_then(pi_session::WeakPiSession::upgrade),
            )
        };
        let event = persist_message_event(
            &root,
            &root_session_id,
            &parent_session_id,
            &agent_id,
            &message,
        )?;
        let parent = parent.ok_or_else(|| "parent session is no longer available".to_string())?;
        parent
            .send_custom_message(CustomMessageInput {
                custom_type: "agent_message".into(),
                content: CustomMessageContent::Text(format!(
                    "Message from agent {agent_id}:\n{message}"
                )),
                display: true,
                details: Some(projection_details(&event, &parent_session_id)),
            })
            .map_err(|error| format!("could not project collaboration message: {error}"))?;
        Ok(json!({
            "target":"parent",
            "acceptedAs":"steer",
            "eventRecordId":event.id,
            "eventRecordSeq":event.sequence,
        }))
    }

    pub(crate) fn interrupt(&self, owner: &str, id: &str) -> Result<AgentSnapshot, String> {
        let (turn, abort_launch, immediate) = {
            let mut state = self.lock();
            let agent = owned_agent_mut(&mut state, owner, id)?;
            if matches!(agent.state, AgentState::Queued | AgentState::Starting)
                && agent.current_turn.is_none()
            {
                agent.state = AgentState::Interrupted;
                agent.updated_at = now_ms();
                agent.last_report = Some(AgentTaskReport {
                    turn_id: None,
                    outcome: TaskOutcome::Interrupted,
                    summary: "Agent launch interrupted before its first turn.".to_string(),
                    truncated: false,
                    usage: Usage::default(),
                });
                (None, true, Some(snapshot(agent)))
            } else {
                let Some(turn) = agent
                    .current_turn
                    .clone()
                    .filter(|_| agent.state.is_active())
                else {
                    return Ok(snapshot(agent));
                };
                agent.state = AgentState::Interrupting;
                agent.updated_at = now_ms();
                (Some(turn), false, None)
            }
        };
        if abort_launch
            && let Some((_, task)) = self
                .inner
                .monitors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&format!("launch:{id}"))
        {
            task.abort.abort();
        }
        if let Some(turn) = turn {
            turn.abort().map_err(|error| error.to_string())?;
        }
        self.checkpoint_agent(id)?;
        self.wake();
        if immediate.is_some() {
            self.notify_parent_if_joined(id);
        }
        if let Some(snapshot) = immediate {
            return Ok(snapshot);
        }
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
    ) -> Result<(WaitRegistration, pi_session::PiSession), String> {
        let root = self.root_session(session_id)?;
        *self
            .lock()
            .active_waits
            .entry(session_id.to_string())
            .or_default() += 1;
        Ok((
            WaitRegistration {
                runtime: self.downgrade(),
                session_id: session_id.to_string(),
            },
            root,
        ))
    }

    pub(crate) fn evaluate_wait(
        &self,
        owner: &str,
        targets: &[String],
        mode: WaitMode,
    ) -> Result<WaitEvaluation, String> {
        let (root_session_id, root, recipient) = {
            let state = self.lock();
            let root_session_id = state
                .assignments
                .get(owner)
                .and_then(|id| state.agents.get(id))
                .map_or_else(|| owner.to_string(), |agent| agent.root_session_id.clone());
            let root = state
                .sessions
                .get(&root_session_id)
                .map(|session| session.session.clone())
                .ok_or_else(|| format!("root session {root_session_id} is unavailable"))?;
            let recipient = state
                .sessions
                .get(owner)
                .map(|session| session.session.clone())
                .ok_or_else(|| format!("session {owner} is unavailable"))?;
            (root_session_id, root, recipient)
        };
        let root_records = snapshot_records(&root.snapshot().map_err(|error| error.to_string())?)?;
        let recipient_records =
            snapshot_records(&recipient.snapshot().map_err(|error| error.to_string())?)?;
        let consumed = consumed_event_ids_from_records(recipient_records.iter());
        let pending_messages = messages_for_recipient(root_records.iter(), &root_session_id, owner);

        let mut state = self.lock();
        let agents = targets
            .iter()
            .filter(|id| id.as_str() != "parent")
            .map(|id| owned_agent(&state, owner, id).map(snapshot))
            .collect::<Result<Vec<_>, _>>()?;
        let messages = pending_messages
            .into_iter()
            .filter(|message| targets.iter().any(|id| id == &message.from))
            .filter(|message| !consumed.contains(&message.id))
            .filter(|message| {
                state
                    .observed_messages
                    .entry(owner.to_string())
                    .or_default()
                    .insert(message.id.clone())
            })
            .collect::<Vec<_>>();
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

    pub(crate) fn project_collaboration_context(
        &self,
        session_id: &str,
        messages: Vec<Message>,
    ) -> Vec<Message> {
        let consumed = consumed_event_ids_from_messages(&messages);
        // Claims only serialize concurrent wait calls until a real tool result
        // is available. Never use a claim itself as proof of consumption: if
        // tool-result persistence fails, the semantic projection must remain
        // visible on the next provider request.
        self.lock().observed_messages.remove(session_id);
        remove_consumed_projections(messages, &consumed)
    }

    fn root_session(&self, owner: &str) -> Result<pi_session::PiSession, String> {
        let state = self.lock();
        let root_session_id = state
            .assignments
            .get(owner)
            .and_then(|id| state.agents.get(id))
            .map_or(owner, |agent| agent.root_session_id.as_str());
        state
            .sessions
            .get(root_session_id)
            .and_then(|session| session.pi_session.as_ref())
            .and_then(pi_session::WeakPiSession::upgrade)
            .ok_or_else(|| format!("root session {root_session_id} is no longer available"))
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
        let monitors = self
            .inner
            .monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|(task_owner, _)| task_owner == owner)
            .map(|(_, task)| Arc::clone(task))
            .collect::<Vec<_>>();
        for task in monitors {
            task.abort.abort();
        }
    }

    pub(crate) fn forget_session(&self, session_id: &str) {
        let mut state = self.lock();
        state.sessions.remove(session_id);
        state.observed_messages.remove(session_id);
        state.active_waits.remove(session_id);
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
            let (summary, truncated) = bounded_task_summary(&completion.text);
            agent.last_report = Some(AgentTaskReport {
                turn_id: Some(turn_id.to_string()),
                outcome: task_outcome(completion.state),
                summary,
                truncated,
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
        let _ = self.checkpoint_agent(id);
        self.wake();
        if usage != Usage::default()
            && let Some(session) = parent_handle.as_ref()
        {
            let _ = session.record_usage(
                usage,
                Some(json!({"source":"agent","agentId":id,"turnId":turn_id})),
            );
        }
        self.notify_parent_if_joined(id);
    }

    fn notify_parent_if_joined(&self, id: &str) {
        let notification = {
            let state = self.lock();
            let Some(agent) = state.agents.get(id) else {
                return;
            };
            if agent.detached
                || state
                    .active_waits
                    .get(&agent.parent_session_id)
                    .copied()
                    .unwrap_or_default()
                    > 0
            {
                return;
            }
            let Some(report) = agent.last_report.clone() else {
                return;
            };
            let Some(parent) = state.sessions.get(&agent.parent_session_id) else {
                return;
            };
            (
                parent.session.handle_for_adapter(),
                agent.profile.name.clone(),
                report,
            )
        };
        let (handle, agent_name, report) = notification;
        if let Err(error) = send_custom_message(
            &handle,
            CustomMessageInput {
                custom_type: "agent_settled".into(),
                content: CustomMessageContent::Text(format!(
                    "Agent {id} ({agent_name}) finished with outcome {}.\n\n{}",
                    task_outcome_name(report.outcome),
                    report.summary
                )),
                display: true,
                details: Some(json!({"agentId":id,"agent":agent_name,"report":report})),
            },
            true,
        ) {
            if let Some(agent) = self.lock().agents.get_mut(id) {
                agent
                    .warnings
                    .push(format!("automatic task report delivery failed: {error}"));
            }
            let _ = self.checkpoint_agent(id);
            self.wake();
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
        detached: agent.detached,
        state: agent.state,
        current_turn_id: agent
            .current_turn
            .as_ref()
            .map(|turn| turn.id().as_str().to_string()),
        last_report: agent.last_report.clone(),
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

fn persist_message_event(
    root: &pi_session::PiSession,
    root_session_id: &str,
    recipient_session_id: &str,
    from: &str,
    message: &str,
) -> Result<CollaborationMessage, String> {
    let record = root
        .current()
        .append_custom_record(
            COLLABORATION_ENTRY_TYPE,
            Some(message_event_data(
                root_session_id,
                recipient_session_id,
                from,
                message,
            )),
        )
        .map_err(|error| format!("could not persist collaboration event: {error}"))?;
    Ok(CollaborationMessage {
        id: record.id,
        sequence: record.seq,
        from: from.to_string(),
        message: message.to_string(),
        created_at: u64::try_from(record.timestamp_ms).unwrap_or_default(),
    })
}

fn snapshot_records(snapshot: &SessionSnapshot) -> Result<Vec<pi_session::SessionRecord>, String> {
    snapshot
        .branch()
        .iter()
        .map(|entry| serde_json::from_value(entry.raw().clone()).map_err(|error| error.to_string()))
        .collect()
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

fn task_outcome(state: AgentState) -> TaskOutcome {
    match state {
        AgentState::Idle => TaskOutcome::Succeeded,
        AgentState::Interrupted => TaskOutcome::Interrupted,
        AgentState::TimedOut => TaskOutcome::TimedOut,
        AgentState::Failed => TaskOutcome::Failed,
        AgentState::Queued
        | AgentState::Starting
        | AgentState::Running
        | AgentState::Interrupting => {
            debug_assert!(false, "active agent state cannot produce a terminal report");
            TaskOutcome::Failed
        }
    }
}

fn task_outcome_name(outcome: TaskOutcome) -> &'static str {
    match outcome {
        TaskOutcome::Succeeded => "succeeded",
        TaskOutcome::Failed => "failed",
        TaskOutcome::Interrupted => "interrupted",
        TaskOutcome::TimedOut => "timed_out",
    }
}

fn active_agents_for_root(state: &RuntimeState, root: &str) -> usize {
    state
        .agents
        .values()
        .filter(|agent| agent.root_session_id == root && agent.state.is_active())
        .count()
}

fn bounded_task_summary(value: &str) -> (String, bool) {
    if value.len() <= MAX_TASK_REPORT_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_TASK_REPORT_BYTES.saturating_sub('…'.len_utf8());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &value[..end]), true)
}

impl From<&AgentRecord> for PersistedAgent {
    fn from(agent: &AgentRecord) -> Self {
        Self {
            id: agent.id.clone(),
            parent_session_id: agent.parent_session_id.clone(),
            parent_agent_id: agent.parent_agent_id.clone(),
            root_session_id: agent.root_session_id.clone(),
            child_session_id: agent.child_session_id.clone(),
            child_path: agent.child_path.clone(),
            depth: agent.depth,
            max_depth: agent.max_depth,
            profile: agent.profile.clone(),
            task: agent.task.clone(),
            pending_request: agent.pending_request.clone(),
            detached: agent.detached,
            state: agent.state,
            current_turn_id: agent
                .current_turn
                .as_ref()
                .map(|turn| turn.id().as_str().to_string()),
            last_report: agent.last_report.clone(),
            usage: agent.usage.clone(),
            warnings: agent.warnings.clone(),
            submission_seq: agent.submission_seq,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
        }
    }
}

fn recovery_agents(snapshot: &SessionSnapshot) -> Result<Vec<PersistedAgent>, String> {
    let mut latest = HashMap::<String, Option<PersistedAgent>>::new();
    for entry in snapshot.branch() {
        let raw = entry.raw();
        if raw.get("customType").and_then(Value::as_str) != Some(RECOVERY_ENTRY_TYPE) {
            continue;
        }
        let data = raw
            .get("data")
            .cloned()
            .ok_or_else(|| "subagent recovery entry is missing data".to_string())?;
        let checkpoint: AgentCheckpoint =
            serde_json::from_value(data).map_err(|error| error.to_string())?;
        if checkpoint.version != RECOVERY_VERSION {
            return Err(format!(
                "unsupported subagent recovery version {}",
                checkpoint.version
            ));
        }
        if checkpoint.root_session_id != snapshot.id() {
            return Err(format!(
                "subagent recovery root {:?} does not match session {:?}",
                checkpoint.root_session_id,
                snapshot.id()
            ));
        }
        if checkpoint
            .agent
            .as_ref()
            .is_some_and(|agent| agent.id != checkpoint.agent_id)
        {
            return Err("subagent recovery agent id does not match its checkpoint key".into());
        }
        latest.insert(checkpoint.agent_id, checkpoint.agent);
    }
    Ok(latest.into_values().flatten().collect())
}

fn reconcile_recovered_agents(agents: &mut [PersistedAgent]) {
    for agent in agents {
        match agent.state {
            AgentState::Queued if valid_queued_request(agent) => {
                agent.current_turn_id = None;
            }
            AgentState::Queued => reconcile_persisted_as_interrupted(
                agent,
                "Queued agent could not be restored because its durable launch request was missing or invalid.",
            ),
            AgentState::Starting | AgentState::Running | AgentState::Interrupting => {
                reconcile_persisted_as_interrupted(
                    agent,
                    "Agent turn was interrupted by process restart. No tool call was replayed; use followup_task to continue in the same agent session.",
                );
            }
            AgentState::Idle
            | AgentState::Failed
            | AgentState::Interrupted
            | AgentState::TimedOut => {
                agent.current_turn_id = None;
                agent.pending_request = None;
            }
        }
    }
}

fn valid_queued_request(agent: &PersistedAgent) -> bool {
    let Some(request) = &agent.pending_request else {
        return false;
    };
    let CustomMessageContent::Text(text) = &request.input else {
        return false;
    };
    run_marker(text) == Some(agent.id.as_str())
        && agent.child_session_id.is_none()
        && agent.child_path.is_none()
}

fn reconcile_persisted_as_interrupted(agent: &mut PersistedAgent, summary: &str) {
    agent.state = AgentState::Interrupted;
    agent.last_report = Some(AgentTaskReport {
        turn_id: agent.current_turn_id.take(),
        outcome: TaskOutcome::Interrupted,
        summary: summary.to_string(),
        truncated: false,
        usage: Usage::default(),
    });
    agent.pending_request = None;
    agent.updated_at = now_ms();
}

fn reconcile_as_interrupted(agent: &mut AgentRecord, summary: &str) {
    let turn_id = agent
        .current_turn
        .take()
        .map(|turn| turn.id().as_str().to_string());
    agent.state = AgentState::Interrupted;
    agent.last_report = Some(AgentTaskReport {
        turn_id,
        outcome: TaskOutcome::Interrupted,
        summary: summary.to_string(),
        truncated: false,
        usage: Usage::default(),
    });
    agent.pending_request = None;
    agent.updated_at = now_ms();
}

fn validate_recovered_agents(root: &str, agents: &[PersistedAgent]) -> Result<(), String> {
    let by_id = agents
        .iter()
        .map(|agent| (agent.id.as_str(), agent))
        .collect::<HashMap<_, _>>();
    if by_id.len() != agents.len() {
        return Err("duplicate agent id in recovery state".into());
    }
    for agent in agents {
        if agent.id.is_empty() || agent.root_session_id != root {
            return Err("invalid agent identity in recovery state".into());
        }
        if agent.depth == 0 || agent.depth > agent.max_depth {
            return Err(format!("invalid recovered depth for agent {}", agent.id));
        }
        match agent.parent_agent_id.as_deref() {
            None if agent.depth == 1 && agent.parent_session_id == root => {}
            Some(parent_id) => {
                let parent = by_id
                    .get(parent_id)
                    .ok_or_else(|| format!("missing recovered parent agent {parent_id}"))?;
                if parent.depth + 1 != agent.depth
                    || parent.child_session_id.as_deref() != Some(agent.parent_session_id.as_str())
                {
                    return Err(format!("invalid recovered lineage for agent {}", agent.id));
                }
            }
            _ => {
                return Err(format!(
                    "invalid recovered root ownership for agent {}",
                    agent.id
                ));
            }
        }
    }
    Ok(())
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

pub(crate) fn result_with_details(text: impl Into<String>, details: Value) -> ToolResult {
    let mut result = ToolResult::text(text.into());
    result.details = Some(details);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::builtin_profile;
    use pi_core::CustomMessage;

    fn runtime(max_depth: usize, max_spawns: usize, max_active: usize) -> SubagentRuntime {
        SubagentRuntime::with_limits_and_checkpointing(
            RuntimeLimits {
                max_depth,
                max_spawns_per_root: max_spawns,
                max_active_agents: max_active,
            },
            false,
        )
    }

    #[test]
    fn admission_enforces_depth_spawn_and_active_limits() {
        let runtime = runtime(2, 2, 1);
        let first = runtime
            .begin_launch("root", builtin_profile("delegate"), 2, false)
            .unwrap();
        let queued = runtime
            .begin_launch("root", builtin_profile("delegate"), 2, false)
            .unwrap();
        assert_eq!(first.state(), AgentState::Starting);
        assert_eq!(queued.state(), AgentState::Queued);
        assert!(matches!(
            runtime.begin_launch("root", builtin_profile("delegate"), 2, false),
            Err(LaunchError::SpawnBudget { .. })
        ));
        runtime.cancel_launch(first.id());
        runtime.cancel_launch(queued.id());
    }

    #[test]
    fn active_capacity_is_isolated_per_root() {
        let runtime = runtime(2, 4, 1);
        let root_a = runtime
            .begin_launch("root-a", builtin_profile("delegate"), 2, false)
            .unwrap();
        let root_b = runtime
            .begin_launch("root-b", builtin_profile("delegate"), 2, false)
            .unwrap();
        let queued_a = runtime
            .begin_launch("root-a", builtin_profile("delegate"), 2, false)
            .unwrap();
        assert_eq!(root_a.state(), AgentState::Starting);
        assert_eq!(root_b.state(), AgentState::Starting);
        assert_eq!(queued_a.state(), AgentState::Queued);
    }

    #[test]
    fn markers_are_exact_first_line_metadata() {
        let id = Uuid::now_v7().to_string();
        let prompt = format!("{}\nwork", marker(&id));
        assert_eq!(run_marker(&prompt), Some(id.as_str()));
        assert_eq!(run_marker("work"), None);
    }

    #[test]
    fn an_uncommitted_wait_claim_never_hides_the_semantic_message() {
        let runtime = runtime(2, 2, 1);
        runtime
            .lock()
            .observed_messages
            .insert("root".into(), HashSet::from(["event-1".into()]));
        let projection = Message::custom(CustomMessage {
            custom_type: "agent_message".into(),
            content: CustomMessageContent::Text("ready".into()),
            display: true,
            details: Some(json!({"sourceRecordId":"event-1"})),
            timestamp_ms: 1,
        });

        let projected = runtime.project_collaboration_context("root", vec![projection.clone()]);

        assert_eq!(projected, vec![projection]);
        assert!(!runtime.lock().observed_messages.contains_key("root"));
    }
}
