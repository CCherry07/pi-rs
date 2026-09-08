use std::sync::Mutex;
use std::time::Duration;

use pi_core::{
    ContentBlock, CustomMessageContent, IsolatedSessionOutcome, IsolatedSessionRequest, Message,
    NoticeLevel, PluginContextError, PluginId, SessionExecutionOrigin, StopReason,
};
use pi_session::{
    SessionPlugin, SessionPluginContext, SessionPluginError, SessionShutdownEvent,
    SessionStartEvent,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result, ScheduleOptions,
    interface::Scope,
    now_ms,
    store::{Claim, Run, Status, Store},
};

/// Generation-owned resource. Factories only construct this value; session
/// start activates it, shutdown cancels and joins it before context retirement.
pub struct ScheduleSessionPlugin {
    options: ScheduleOptions,
    worker: Mutex<Option<Worker>>,
}

struct Worker {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl ScheduleSessionPlugin {
    pub fn new(options: ScheduleOptions) -> Self {
        Self {
            options,
            worker: Mutex::new(None),
        }
    }
}

impl Drop for ScheduleSessionPlugin {
    fn drop(&mut self) {
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            worker.stop.cancel();
        }
    }
}

#[pi_session::session_plugin]
impl SessionPlugin for ScheduleSessionPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("schedule")
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _: &SessionStartEvent,
    ) -> std::result::Result<(), SessionPluginError> {
        if context.session.execution_origin()? != SessionExecutionOrigin::User {
            return Ok(());
        }
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker.is_some() {
            return Ok(());
        }
        let stop = CancellationToken::new();
        let task = tokio::spawn(run_scheduler(
            self.options.clone(),
            context.clone(),
            stop.clone(),
        ));
        *worker = Some(Worker { stop, task });
        Ok(())
    }

    async fn session_shutdown(
        &self,
        _: &SessionPluginContext,
        _: &SessionShutdownEvent,
    ) -> std::result::Result<(), SessionPluginError> {
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            worker.stop.cancel();
            worker.task.await.map_err(|error| {
                SessionPluginError::Failure(format!("schedule worker failed: {error}"))
            })?;
        }
        Ok(())
    }
}

async fn run_scheduler(
    options: ScheduleOptions,
    context: SessionPluginContext,
    stop: CancellationToken,
) {
    let mut last_error = None;
    loop {
        // First tick is delayed until the host has published/bound the session.
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        let result = tick(&options, &context, &stop).await;
        if let Err(error) = result {
            let message = error.to_string();
            if last_error.as_ref() != Some(&message) {
                let _ = context
                    .ui
                    .notify(NoticeLevel::Error, format!("Schedule: {message}"));
            }
            last_error = Some(message);
        } else {
            last_error = None;
        }
    }
}

async fn tick(
    options: &ScheduleOptions,
    context: &SessionPluginContext,
    stop: &CancellationToken,
) -> Result<()> {
    for scope in [Scope::Global, Scope::Project] {
        if stop.is_cancelled() {
            break;
        }
        if !context.session.is_idle()? || context.session.has_pending_messages()? {
            break;
        }
        if matches!(scope, Scope::Project) && options.project.is_none() {
            continue;
        }
        let store = options.store(scope)?;
        let work_store = store.clone();
        let cwd = options.cwd.clone();
        let claim = tokio::task::spawn_blocking(move || work_store.claim(&cwd, now_ms()))
            .await
            .map_err(|error| Error::Invalid(format!("schedule claim failed: {error}")))??;
        if let Some(claim) = claim {
            execute(store, claim, context, stop).await?;
            // At most one job per store per tick; project schedules cannot
            // starve behind a continuously due global schedule.
        }
    }
    Ok(())
}

async fn execute(
    store: Store,
    claim: Claim,
    context: &SessionPluginContext,
    stop: &CancellationToken,
) -> Result<()> {
    let mut run = claim.run.clone();
    let mut options = claim.job.options.clone();
    if let Some(tools) = &mut options.active_tools {
        tools.retain(|name| name != "schedule");
    }
    let request = IsolatedSessionRequest::new(CustomMessageContent::Text(claim.job.prompt.clone()))
        .options(options);
    let deadline = tokio::time::sleep(Duration::from_secs(claim.job.timeout_seconds));
    tokio::pin!(deadline);
    // Launch may be waiting for the manager's replacement/shutdown gate. It
    // must be cancellable or awaiting this worker in shutdown would deadlock.
    let launched = tokio::select! {
        biased;
        _ = stop.cancelled() => { run.status = Status::Aborted; None }
        _ = &mut deadline => { run.status = Status::TimedOut; None }
        result = context.session.launch_isolated_session(request) => {
            match result {
                Ok(handle) => Some(handle),
                Err(error) => { run.status = Status::Failed; run.error = Some(error.to_string()); None }
            }
        }
    };
    if let Some(handle) = launched {
        let outcome = tokio::select! {
            biased;
            _ = stop.cancelled() => { run.status = Status::Aborted; None }
            _ = &mut deadline => { run.status = Status::TimedOut; None }
            result = handle.wait() => Some(result),
        };
        match outcome {
            Some(Ok(outcome)) => apply_outcome(&mut run, outcome),
            Some(Err(error)) => {
                run.status = if matches!(
                    error,
                    PluginContextError::Unbound
                        | PluginContextError::Retired
                        | PluginContextError::Unavailable(_)
                ) {
                    let _ = handle.abort();
                    Status::Unknown
                } else {
                    Status::Failed
                };
                run.error = Some(error.to_string());
            }
            None => {
                let _ = handle.abort();
                // Keep the per-job lock until cancellation settles, so another
                // process cannot overlap an attempt that is still unwinding.
                match tokio::time::timeout(Duration::from_secs(5), handle.wait()).await {
                    Ok(Ok(outcome)) => {
                        let status = run.status;
                        apply_outcome(&mut run, outcome);
                        run.status = status;
                    }
                    _ => {
                        run.status = Status::Unknown;
                        run.error = Some(
                            "cancelled execution did not settle; schedule paused for inspection"
                                .into(),
                        );
                    }
                }
            }
        }
    }
    run.finished_at = Some(now_ms());
    let notice = format!(
        "Scheduled task '{}' ({}) {:?}\n{}{}",
        claim.job.name,
        claim.job.id,
        run.status,
        run.output.chars().take(2000).collect::<String>(),
        run.error.as_deref().unwrap_or_default()
    );
    let level = if run.status == Status::Completed {
        NoticeLevel::Info
    } else {
        NoticeLevel::Error
    };
    let work_store = store.clone();
    tokio::task::spawn_blocking(move || work_store.finish(run))
        .await
        .map_err(|error| Error::Invalid(format!("schedule finish failed: {error}")))??;
    if claim.job.notify && !stop.is_cancelled() {
        context.ui.notify(level, notice)?;
    }
    // claim and its run lock drop only after the terminal record is committed.
    Ok(())
}

fn apply_outcome(run: &mut Run, outcome: IsolatedSessionOutcome) {
    run.session_id = Some(outcome.session_id);
    run.status = if outcome.aborted {
        Status::Aborted
    } else {
        Status::Completed
    };
    if let Some(assistant) = outcome
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message) => Some(message),
            _ => None,
        })
    {
        run.output = assistant
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if assistant.stop_reason == StopReason::Error || assistant.error_message.is_some() {
            run.status = Status::Failed;
            run.error = assistant
                .error_message
                .clone()
                .or_else(|| Some("provider failed".into()));
        }
    }
}
