use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use pi_agent::AgentLoopStop;
use pi_core::{
    AbortHandle, AbortSignal, CustomMessageContent, IsolatedFollowUpReceipt,
    IsolatedMessageDelivery, IsolatedMessageReceipt, IsolatedSessionId, IsolatedSessionOutcome,
    IsolatedSessionTurnId, Message, Usage, UsageCost, UserMessage,
};
use tokio::sync::watch;

use crate::{
    AgentSessionSnapshot, AgentSessionSubscription, PiSession, QueueKind, SessionError,
    aggregate_document_usage, current_session_context_tokens, now_ms,
};

type IsolatedResult = Result<IsolatedSessionOutcome, String>;

/// A read-only view of one live isolated session.
///
/// Product frontends can use this to project a child agent's semantic event
/// stream without promoting the child to a frontend-owned [`PiSession`].
#[derive(Clone)]
pub struct IsolatedSessionObservation {
    id: IsolatedSessionId,
    session: PiSession,
}

/// Product-facing usage snapshot for one managed isolated session.
#[derive(Debug, Clone, PartialEq)]
pub struct IsolatedSessionUsageSnapshot {
    pub usage: Usage,
    pub context_tokens: Option<u64>,
    pub model_context_window: Option<u64>,
}

impl IsolatedSessionObservation {
    pub fn isolated_id(&self) -> &IsolatedSessionId {
        &self.id
    }

    pub fn session_id(&self) -> String {
        self.session.id()
    }

    pub fn cwd(&self) -> PathBuf {
        self.session.cwd()
    }

    pub fn snapshot(&self) -> AgentSessionSnapshot {
        self.session.current().snapshot()
    }

    /// Reads durable provenance and transcript data without granting child control.
    /// Unsaved children are read from their in-memory log.
    pub fn document(&self) -> Result<crate::SessionDocument, SessionError> {
        self.session.current().log().load()
    }

    pub fn subscribe(&self) -> AgentSessionSubscription {
        self.session.current().subscribe()
    }

    /// Returns billed and current-context usage without exposing control over
    /// the managed child session.
    pub fn usage_snapshot(&self) -> Option<IsolatedSessionUsageSnapshot> {
        let session = self.session.current();
        let document = session.log().load().ok()?;
        let context_tokens = document.context().ok().and_then(|context| {
            let branch = document
                .branch()
                .ok()?
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            current_session_context_tokens(&branch, &context.messages).map(|usage| usage.tokens)
        });
        Some(IsolatedSessionUsageSnapshot {
            usage: aggregate_document_usage(&document),
            context_tokens,
            model_context_window: session.active_context_window(),
        })
    }

    /// Observes an isolated child owned by this observed session.
    pub fn observe_isolated_session(&self, id: &IsolatedSessionId) -> Result<Self, String> {
        self.session
            .observe_isolated_session(id)
            .map_err(|error| error.to_string())
    }
}

pub(crate) struct IsolatedSessionRegistry {
    sessions: Mutex<HashMap<IsolatedSessionId, Arc<IsolatedSessionRun>>>,
}

struct IsolatedSessionRun {
    owner_registration_id: String,
    session: PiSession,
    turns: Mutex<IsolatedTurnState>,
}

#[derive(Default)]
struct IsolatedTurnState {
    active: Option<IsolatedSessionTurnId>,
    turns: HashMap<IsolatedSessionTurnId, Arc<IsolatedTurnRun>>,
    mailbox: Vec<CustomMessageContent>,
}

struct IsolatedTurnRun {
    result: watch::Receiver<Option<IsolatedResult>>,
    abort: AbortHandle,
    task: tokio::sync::Mutex<Option<OwnedTask<()>>>,
}

/// Dropping a Tokio JoinHandle detaches it. Managed turn tasks instead cancel
/// on drop; normal shutdown joins them before releasing ownership.
struct OwnedTask<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct TerminalResult(watch::Sender<Option<IsolatedResult>>);

impl Drop for TerminalResult {
    fn drop(&mut self) {
        if self.0.borrow().is_none() {
            self.0.send_replace(Some(Err(
                "isolated session turn was cancelled before publishing its outcome".to_string(),
            )));
        }
    }
}

impl IsolatedSessionRun {
    fn abort_active(&self) {
        let turn = {
            let state = self
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .active
                .as_ref()
                .and_then(|id| state.turns.get(id))
                .cloned()
        };
        if let Some(turn) = turn {
            turn.abort.abort();
            self.session.abort();
        }
    }

    fn turn(&self, id: &IsolatedSessionTurnId) -> Result<Arc<IsolatedTurnRun>, String> {
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .turns
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown isolated session turn: {}", id.as_str()))
    }

    async fn drain(&self) {
        let turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .turns
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for turn in &turns {
            turn.abort.abort();
        }
        self.session.abort();
        for turn in turns {
            let mut task = turn.task.lock().await;
            if let Some(task) = task.as_mut() {
                let _ = (&mut task.0).await;
            }
            task.take();
        }
    }
}

/// Until launch returns its control handle, the launch future owns
/// cancellation. Dropping launch cannot leave an undiscoverable running child.
struct LaunchGuard(Option<AbortHandle>);

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

impl Default for IsolatedSessionRegistry {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl IsolatedSessionRegistry {
    pub(crate) async fn launch(
        &self,
        owner_registration_id: String,
        session: PiSession,
        input: CustomMessageContent,
    ) -> IsolatedSessionId {
        let id = IsolatedSessionId::new(session.registration_id().to_owned());
        let initial_turn_id = IsolatedSessionTurnId::new(id.as_str());
        let run = Arc::new(IsolatedSessionRun {
            owner_registration_id,
            session,
            turns: Mutex::new(IsolatedTurnState::default()),
        });
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), Arc::clone(&run));

        let (launch_abort, launch_signal) = AbortHandle::new();
        let mut launch_guard = LaunchGuard(Some(launch_abort));
        let readiness = {
            let mut state = run
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            start_turn_locked(
                &run,
                &mut state,
                initial_turn_id,
                vec![input],
                Some(launch_signal),
            )
        };
        let session = run.session.current();
        while !session.accepts_live_messages() && readiness.borrow().is_none() {
            tokio::task::yield_now().await;
        }
        launch_guard.0.take();
        id
    }

    pub(crate) fn wait(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> impl Future<Output = Result<IsolatedSessionOutcome, String>> + Send + 'static + use<> {
        self.wait_turn(
            owner_registration_id,
            id,
            &IsolatedSessionTurnId::new(id.as_str()),
        )
    }

    pub(crate) fn wait_turn(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
        turn_id: &IsolatedSessionTurnId,
    ) -> impl Future<Output = Result<IsolatedSessionOutcome, String>> + Send + 'static + use<> {
        let result = self
            .owned_session(owner_registration_id, id)
            .and_then(|run| run.turn(turn_id))
            .map(|turn| turn.result.clone());
        let turn_id = turn_id.clone();
        async move {
            let mut result = result?;
            loop {
                if let Some(outcome) = result.borrow().clone() {
                    return outcome;
                }
                result.changed().await.map_err(|_| {
                    format!(
                        "isolated session turn {} ended without a terminal outcome",
                        turn_id.as_str()
                    )
                })?;
            }
        }
    }

    pub(crate) fn send_message(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
        content: CustomMessageContent,
    ) -> Result<IsolatedMessageReceipt, String> {
        let run = self.owned_session(owner_registration_id, id)?;
        let mut state = run
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(turn_id) = state.active.clone() {
            match run
                .session
                .current()
                .enqueue_message(user_message(content.clone()), QueueKind::Steer)
            {
                Ok(_) => {
                    return Ok(IsolatedMessageReceipt {
                        accepted_as: IsolatedMessageDelivery::Steer,
                        turn_id: Some(turn_id),
                    });
                }
                Err(SessionError::Busy) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        state.mailbox.push(content);
        Ok(IsolatedMessageReceipt {
            accepted_as: IsolatedMessageDelivery::Mailbox,
            turn_id: None,
        })
    }

    pub(crate) async fn follow_up(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
        content: CustomMessageContent,
    ) -> Result<IsolatedFollowUpReceipt, String> {
        let run = self.owned_session(owner_registration_id, id)?;
        let (turn_id, readiness) = loop {
            let active = run
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                .clone();
            if let Some(turn_id) = active {
                match run
                    .session
                    .current()
                    .enqueue_message(user_message(content.clone()), QueueKind::FollowUp)
                {
                    Ok(_) => {
                        return Ok(IsolatedFollowUpReceipt {
                            turn_id,
                            started: false,
                        });
                    }
                    Err(SessionError::Busy) => continue,
                    Err(error) => return Err(error.to_string()),
                }
            }

            let mut state = run
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.active.is_some() {
                continue;
            }
            let turn_id = IsolatedSessionTurnId::new(uuid::Uuid::now_v7().to_string());
            let mut messages = std::mem::take(&mut state.mailbox);
            messages.push(content);
            let readiness = start_turn_locked(&run, &mut state, turn_id.clone(), messages, None);
            break (turn_id, readiness);
        };
        let session = run.session.current();
        while !session.accepts_live_messages() && readiness.borrow().is_none() {
            tokio::task::yield_now().await;
        }
        Ok(IsolatedFollowUpReceipt {
            turn_id,
            started: true,
        })
    }

    pub(crate) fn abort_turn(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
        turn_id: &IsolatedSessionTurnId,
    ) -> Result<(), String> {
        let run = self.owned_session(owner_registration_id, id)?;
        let turn = run.turn(turn_id)?;
        turn.abort.abort();
        let is_active = run
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            == Some(turn_id);
        if is_active {
            run.session.abort();
        }
        Ok(())
    }

    pub(crate) fn abort(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<(), String> {
        self.owned_session(owner_registration_id, id)?
            .abort_active();
        Ok(())
    }

    pub(crate) fn observe(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<IsolatedSessionObservation, String> {
        let run = self.owned_session(owner_registration_id, id)?;
        Ok(IsolatedSessionObservation {
            id: id.clone(),
            session: run.session.clone(),
        })
    }

    pub(crate) fn owned_sessions(&self, owner_registration_id: &str) -> Vec<PiSession> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|run| run.owner_registration_id == owner_registration_id)
            .map(|run| run.session.clone())
            .collect()
    }

    pub(crate) async fn drain_all(&self) {
        let runs = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        drain_runs(runs).await;
    }

    pub(crate) async fn drain_sessions(&self, sessions: &[PiSession]) {
        let runs = {
            let runs = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions
                .iter()
                .filter_map(|session| {
                    runs.get(&IsolatedSessionId::new(
                        session.registration_id().to_owned(),
                    ))
                    .cloned()
                })
                .collect()
        };
        drain_runs(runs).await;
    }

    /// The caller drains the complete closing tree before removing any run.
    pub(crate) fn remove_session(&self, registration_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&IsolatedSessionId::new(registration_id.to_owned()));
    }

    fn owned_session(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<Arc<IsolatedSessionRun>, String> {
        let run = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown isolated session: {}", id.as_str()))?;
        if run.owner_registration_id != owner_registration_id {
            return Err(format!(
                "isolated session {} is not owned by the current session",
                id.as_str()
            ));
        }
        Ok(run)
    }
}

fn start_turn_locked(
    run: &Arc<IsolatedSessionRun>,
    state: &mut IsolatedTurnState,
    turn_id: IsolatedSessionTurnId,
    input: Vec<CustomMessageContent>,
    launch_signal: Option<AbortSignal>,
) -> watch::Receiver<Option<IsolatedResult>> {
    debug_assert!(state.active.is_none());
    let (result_sender, result) = watch::channel(None);
    let (abort, abort_signal) = AbortHandle::new();
    let session = run.session.current();
    let session_id = run.session.id();
    let prompt = OwnedTask(tokio::spawn(async move {
        run_prompt(session, session_id, input, launch_signal, abort_signal).await
    }));
    let weak = Arc::downgrade(run);
    let completed_turn = turn_id.clone();
    let terminal = TerminalResult(result_sender);
    let task = OwnedTask(tokio::spawn(async move {
        let mut prompt = prompt;
        let outcome = match (&mut prompt.0).await {
            Ok(outcome) => outcome,
            Err(error) if error.is_panic() => {
                Err(format!("isolated session turn panicked: {error}"))
            }
            Err(error) => Err(format!("isolated session turn was cancelled: {error}")),
        };
        clear_active_turn(&weak, &completed_turn);
        terminal.0.send_replace(Some(outcome));
    }));
    state.active = Some(turn_id.clone());
    state.turns.insert(
        turn_id,
        Arc::new(IsolatedTurnRun {
            result: result.clone(),
            abort,
            task: tokio::sync::Mutex::new(Some(task)),
        }),
    );
    result
}

fn clear_active_turn(run: &Weak<IsolatedSessionRun>, turn_id: &IsolatedSessionTurnId) {
    let Some(run) = run.upgrade() else {
        return;
    };
    let mut state = run
        .turns
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.active.as_ref() == Some(turn_id) {
        state.active = None;
    }
}

async fn drain_runs(runs: Vec<Arc<IsolatedSessionRun>>) {
    for run in &runs {
        run.abort_active();
    }
    for run in runs {
        run.drain().await;
    }
}

async fn run_prompt(
    session: Arc<crate::AgentSession>,
    session_id: String,
    input: Vec<CustomMessageContent>,
    launch_signal: Option<AbortSignal>,
    abort_signal: AbortSignal,
) -> IsolatedResult {
    if launch_signal.as_ref().is_some_and(AbortSignal::is_aborted) {
        return Err("isolated session launch was cancelled".to_string());
    }
    let before = session
        .log()
        .load()
        .map(|document| aggregate_document_usage(&document))
        .map_err(|error| error.to_string())?;
    let messages = input.into_iter().map(user_message).collect::<Vec<_>>();
    let mut prompt = std::pin::pin!(session.prompt(messages));
    let (result, launch_cancelled) = if let Some(launch_signal) = launch_signal {
        tokio::select! {
            biased;
            () = launch_signal.wait() => {
                (finish_cancelled_prompt(&session, prompt.as_mut()).await, true)
            }
            () = abort_signal.wait() => {
                (finish_cancelled_prompt(&session, prompt.as_mut()).await, false)
            }
            result = &mut prompt => (result, false),
        }
    } else {
        tokio::select! {
            biased;
            () = abort_signal.wait() => {
                (finish_cancelled_prompt(&session, prompt.as_mut()).await, false)
            }
            result = &mut prompt => (result, false),
        }
    };
    if launch_cancelled {
        return Err("isolated session launch was cancelled".to_string());
    }
    let outcome = result.map_err(|error| error.to_string())?;
    let after = session
        .log()
        .load()
        .map(|document| aggregate_document_usage(&document))
        .map_err(|error| error.to_string())?;
    Ok(IsolatedSessionOutcome {
        session_id,
        messages: outcome.new_messages,
        aborted: outcome.stop == AgentLoopStop::Aborted,
        usage: usage_delta(&after, &before),
    })
}

fn user_message(content: CustomMessageContent) -> Message {
    Message::User(UserMessage {
        content: content.to_blocks(),
        timestamp_ms: now_ms(),
    })
}

fn usage_delta(after: &Usage, before: &Usage) -> Usage {
    let optional_delta = |after: Option<u64>, before: Option<u64>| match (after, before) {
        (Some(after), Some(before)) => Some(after.saturating_sub(before)),
        (Some(after), None) => Some(after),
        (None, _) => None,
    };
    Usage {
        input: after.input.saturating_sub(before.input),
        output: after.output.saturating_sub(before.output),
        cache_read: after.cache_read.saturating_sub(before.cache_read),
        cache_write: after.cache_write.saturating_sub(before.cache_write),
        cache_write_1h: optional_delta(after.cache_write_1h, before.cache_write_1h),
        reasoning: optional_delta(after.reasoning, before.reasoning),
        total_tokens: after.total_tokens.saturating_sub(before.total_tokens),
        cost: UsageCost {
            input: (after.cost.input - before.cost.input).max(0.0),
            output: (after.cost.output - before.cost.output).max(0.0),
            cache_read: (after.cost.cache_read - before.cost.cache_read).max(0.0),
            cache_write: (after.cost.cache_write - before.cost.cache_write).max(0.0),
            total: (after.cost.total - before.cost.total).max(0.0),
        },
    }
}

async fn finish_cancelled_prompt<F: Future>(
    session: &crate::AgentSession,
    mut prompt: std::pin::Pin<&mut F>,
) -> F::Output {
    poll_fn(|context| {
        session.abort();
        let result = prompt.as_mut().poll(context);
        session.abort();
        result
    })
    .await
}
