use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_agent::AgentLoopStop;
use pi_core::{
    AbortHandle, AbortSignal, CustomMessageContent, IsolatedSessionId, IsolatedSessionOutcome,
    Message, UserMessage,
};
use tokio::sync::watch;

use crate::{AgentSessionSnapshot, AgentSessionSubscription, PiSession, now_ms};

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

    pub fn subscribe(&self) -> AgentSessionSubscription {
        self.session.current().subscribe()
    }

    /// Observes an isolated child owned by this observed session.
    pub fn observe_isolated_session(&self, id: &IsolatedSessionId) -> Result<Self, String> {
        self.session
            .observe_isolated_session(id)
            .map_err(|error| error.to_string())
    }
}

pub(crate) struct IsolatedSessionRegistry {
    runs: Mutex<HashMap<IsolatedSessionId, Arc<IsolatedSessionRun>>>,
}

struct IsolatedSessionRun {
    owner_registration_id: String,
    session: PiSession,
    result: watch::Receiver<Option<IsolatedResult>>,
    abort: AbortHandle,
    task: tokio::sync::Mutex<Option<OwnedTask<()>>>,
}

/// Dropping a Tokio JoinHandle detaches it. Both levels of the isolated task
/// instead cancel on drop; normal shutdown joins them before releasing ownership.
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
                "isolated session task was cancelled before publishing its outcome".to_string(),
            )));
        }
    }
}

impl IsolatedSessionRun {
    fn abort(&self) {
        self.abort.abort();
        self.session.abort();
    }

    async fn drain(&self) {
        // Keep the handle in its owner while awaiting: cancelling a shutdown
        // future must not detach the task or lose the next caller's ability to join.
        let mut task = self.task.lock().await;
        if let Some(task) = task.as_mut() {
            let _ = (&mut task.0).await;
        }
        task.take();
    }
}

/// Until launch returns its control handle, the launch future owns cancellation.
/// In particular, a plugin shutting down while awaiting readiness must not
/// leave an independently running child that it never received a handle for.
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
            runs: Mutex::new(HashMap::new()),
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
        let session_id = session.id();
        let prompt_session = session.current();
        let running_session = Arc::clone(&prompt_session);
        let (result_sender, result) = watch::channel(None);
        let readiness = result.clone();
        let (launch_abort, launch_signal) = AbortHandle::new();
        let (abort, abort_signal) = AbortHandle::new();
        let mut launch_guard = LaunchGuard(Some(launch_abort));
        let prompt = OwnedTask(tokio::spawn(async move {
            run_prompt(
                prompt_session,
                session_id,
                input,
                launch_signal,
                abort_signal,
            )
            .await
        }));
        // The supervisor owns and observes the prompt even if launch is dropped.
        // Construct both guards before spawning so cancellation before the first
        // poll also aborts the prompt and publishes a terminal failure.
        let terminal = TerminalResult(result_sender);
        let task = OwnedTask(tokio::spawn(async move {
            let mut prompt = prompt;
            let result = match (&mut prompt.0).await {
                Ok(result) => result,
                Err(error) if error.is_panic() => {
                    Err(format!("isolated session prompt panicked: {error}"))
                }
                Err(error) => Err(format!("isolated session prompt was cancelled: {error}")),
            };
            terminal.0.send_replace(Some(result));
        }));
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id.clone(),
                Arc::new(IsolatedSessionRun {
                    owner_registration_id,
                    session,
                    result,
                    abort,
                    task: tokio::sync::Mutex::new(Some(task)),
                }),
            );
        while !running_session.runtime().agent().is_running() && readiness.borrow().is_none() {
            if readiness.has_changed().is_err() {
                break;
            }
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
        // Waiters own only the outcome channel, not the task or manager. Keeping
        // either alive here would prevent manager drop from cancelling the run.
        let result = self
            .owned_run(owner_registration_id, id)
            .map(|run| run.result.clone());
        let id = id.clone();
        async move {
            let mut result = result?;
            loop {
                if let Some(outcome) = result.borrow().clone() {
                    return outcome;
                }
                result.changed().await.map_err(|_| {
                    format!(
                        "isolated session {} ended without a terminal outcome",
                        id.as_str()
                    )
                })?;
            }
        }
    }

    pub(crate) fn abort(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<(), String> {
        self.owned_run(owner_registration_id, id)?.abort();
        Ok(())
    }

    pub(crate) fn observe(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<IsolatedSessionObservation, String> {
        let run = self.owned_run(owner_registration_id, id)?;
        Ok(IsolatedSessionObservation {
            id: id.clone(),
            session: run.session.clone(),
        })
    }

    pub(crate) fn owned_sessions(&self, owner_registration_id: &str) -> Vec<PiSession> {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|run| run.owner_registration_id == owner_registration_id)
            .map(|run| run.session.clone())
            .collect()
    }

    pub(crate) async fn drain_all(&self) {
        let runs = self
            .runs
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
                .runs
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
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&IsolatedSessionId::new(registration_id.to_owned()));
    }

    fn owned_run(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<Arc<IsolatedSessionRun>, String> {
        let run = self
            .runs
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

async fn drain_runs(runs: Vec<Arc<IsolatedSessionRun>>) {
    // Signal every child before waiting for any one child's cleanup.
    for run in &runs {
        run.abort();
    }
    for run in runs {
        run.drain().await;
    }
}

async fn run_prompt(
    session: Arc<crate::AgentSession>,
    session_id: String,
    input: CustomMessageContent,
    launch_signal: AbortSignal,
    abort_signal: AbortSignal,
) -> IsolatedResult {
    if launch_signal.is_aborted() {
        return Err("isolated session launch was cancelled".to_string());
    }
    let message = Message::User(UserMessage {
        content: input.to_blocks(),
        timestamp_ms: now_ms(),
    });
    let mut prompt = std::pin::pin!(session.prompt(vec![message]));
    let (result, launch_cancelled) = tokio::select! {
        biased;
        _ = launch_signal.wait() => {
            (finish_cancelled_prompt(&session, prompt.as_mut()).await, true)
        }
        _ = abort_signal.wait() => {
            (finish_cancelled_prompt(&session, prompt.as_mut()).await, false)
        }
        result = &mut prompt => (result, false),
    };
    if launch_cancelled {
        return Err("isolated session launch was cancelled".to_string());
    }
    result
        .map_err(|error| error.to_string())
        .map(|outcome| IsolatedSessionOutcome {
            session_id,
            messages: outcome.new_messages,
            aborted: outcome.stop == AgentLoopStop::Aborted,
        })
}

async fn finish_cancelled_prompt<F: Future>(
    session: &crate::AgentSession,
    mut prompt: std::pin::Pin<&mut F>,
) -> F::Output {
    // Cancellation can arrive before agent entry or between retries. Reapply it
    // around each poll to catch a newly installed abort handle, without dropping
    // the session prompt's persistence and agent_settled cleanup future.
    poll_fn(|context| {
        session.abort();
        let result = prompt.as_mut().poll(context);
        session.abort();
        result
    })
    .await
}
