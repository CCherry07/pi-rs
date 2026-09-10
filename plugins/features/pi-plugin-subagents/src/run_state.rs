//! Feature-owned subagent state. JSON is only an output projection.

use std::time::Duration;

use pi_core::{AbortHandle, ContentBlock, IsolatedContextMode, ToolResult, Usage};
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

pub(crate) type RunResult = Result<ToolResult, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunState {
    Starting,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalState {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl From<TerminalState> for RunState {
    fn from(state: TerminalState) -> Self {
        match state {
            TerminalState::Completed => Self::Completed,
            TerminalState::Failed => Self::Failed,
            TerminalState::Cancelled => Self::Cancelled,
            TerminalState::TimedOut => Self::TimedOut,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunActivity {
    Normal,
    NeedsAttention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitMode {
    Foreground,
    Detached,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<crate::workflow::WorkflowSnapshot>,
    pub agent: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<IsolatedContextMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolated_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl RunMetadata {
    pub fn new(agent: String, depth: usize, context: IsolatedContextMode) -> Self {
        Self {
            workflow_id: None,
            workflow: None,
            agent,
            depth,
            context: Some(context),
            isolated_session_id: None,
            session_id: None,
            usage: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ManagedRun {
    pub owner: String,
    pub abort: AbortHandle,
    pub result: Option<RunResult>,
    metadata: RunMetadata,
    state: RunState,
    wait_mode: WaitMode,
    started: Instant,
    started_at: u64,
    finished_at: Option<u64>,
    elapsed_ms: Option<u64>,
    deadline_at: Option<u64>,
    timeout: Option<Duration>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunSnapshot {
    pub run_id: String,
    pub owner_session_id: String,
    #[serde(flatten)]
    pub metadata: RunMetadata,
    pub state: RunState,
    pub activity_state: RunActivity,
    pub wait_mode: WaitMode,
    /// Compatibility projection; `wait_mode` remains the stored truth.
    pub detached: bool,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub elapsed_ms: u64,
    pub deadline_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub non_blocking_wait: Option<crate::waiting::WaitDeadline>,
    pub pending_request_ids: Vec<String>,
    pub result_summary: Option<String>,
    pub is_error: bool,
}

impl ManagedRun {
    pub fn in_workflow(mut self, id: Option<String>) -> Self {
        self.metadata.workflow_id = id;
        self
    }

    pub fn workflow_id(&self) -> Option<&str> {
        self.metadata.workflow_id.as_deref()
    }

    pub fn set_workflow(&mut self, snapshot: crate::workflow::WorkflowSnapshot, usage: Usage) {
        self.metadata.context = None;
        self.metadata.workflow = Some(snapshot);
        self.metadata.usage = Some(usage);
        self.mark_running();
    }

    pub fn new(
        owner: String,
        metadata: RunMetadata,
        abort: AbortHandle,
        timeout: Option<Duration>,
    ) -> Self {
        let started_at = crate::coordination::now_ms();
        Self {
            owner,
            abort,
            result: None,
            metadata,
            state: RunState::Starting,
            wait_mode: WaitMode::Foreground,
            started: Instant::now(),
            started_at,
            finished_at: None,
            elapsed_ms: None,
            deadline_at: None,
            timeout,
        }
    }

    pub fn start_deadline(&mut self) -> Option<Instant> {
        let now = Instant::now();
        self.deadline_at = self
            .timeout
            .map(|timeout| crate::coordination::now_ms().saturating_add(millis(timeout)));
        self.timeout.and_then(|timeout| now.checked_add(timeout))
    }

    pub fn mark_running(&mut self) {
        if self.state == RunState::Starting {
            self.state = RunState::Running;
        }
    }

    pub fn mark_cancelling(&mut self) {
        if self.result.is_none() {
            self.state = RunState::Cancelling;
        }
    }

    pub fn detach(&mut self) {
        self.wait_mode = WaitMode::Detached;
    }

    pub fn is_detached(&self) -> bool {
        self.wait_mode == WaitMode::Detached
    }

    pub fn set_isolated_session_id(&mut self, id: &str) {
        self.metadata.isolated_session_id = Some(id.to_string());
    }

    pub fn set_session_id(&mut self, id: &str) {
        self.metadata.session_id = Some(id.to_string());
    }

    pub fn set_usage(&mut self, usage: Usage) {
        self.metadata.usage = Some(usage);
    }

    pub fn finish(&mut self, state: TerminalState, result: RunResult) -> bool {
        if self.result.is_some() {
            return false;
        }
        self.state = state.into();
        self.elapsed_ms = Some(millis(self.started.elapsed()));
        self.finished_at = Some(crate::coordination::now_ms());
        self.result = Some(result);
        true
    }

    pub fn snapshot(&self, id: &str, pending_request_ids: Vec<String>) -> RunSnapshot {
        let activity_state = if pending_request_ids.is_empty() {
            RunActivity::Normal
        } else {
            RunActivity::NeedsAttention
        };
        let (result_summary, is_error) = result_summary(self.result.as_ref());
        RunSnapshot {
            run_id: id.into(),
            owner_session_id: self.owner.clone(),
            metadata: self.metadata.clone(),
            state: self.state,
            activity_state,
            wait_mode: self.wait_mode,
            detached: self.is_detached(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            elapsed_ms: self
                .elapsed_ms
                .unwrap_or_else(|| millis(self.started.elapsed())),
            deadline_at: self.deadline_at,
            non_blocking_wait: None,
            pending_request_ids,
            result_summary,
            is_error,
        }
    }

    pub fn details(&self, id: &str, pending_request_ids: Vec<String>) -> Value {
        serde_json::to_value(self.snapshot(id, pending_request_ids))
            .expect("run snapshot is serializable")
    }
}

fn result_summary(result: Option<&RunResult>) -> (Option<String>, bool) {
    match result {
        None => (None, false),
        Some(Err(error)) => (Some(error.chars().take(1000).collect()), true),
        Some(Ok(result)) => {
            let summary = result
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .flat_map(|text| text.chars().chain(std::iter::once('\n')))
                .take(1000)
                .collect::<String>();
            (Some(summary.trim_end().to_string()), result.is_error)
        }
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
