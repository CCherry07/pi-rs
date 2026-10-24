use super::activity::{ActivityPhase, CancellationStage};
use super::options::AutoRetrySettings;
use super::{
    AgentSession, CompactionActivation, PreparedSessionPrompt, PromptOutcome, SessionInput,
    SessionSubmission,
};
use crate::{MAIN_LANE, QueueKind, QueueSnapshot, SessionError};
use pi_agent::{AgentLoopOutcome, AgentLoopStop};
use pi_core::{AbortHandle, Message, StopReason};
use pi_plugin::{CommandOutcome, InputStreamingBehavior};
use pi_runtime::PreparedTextSubmission;
use pi_telemetry::{
    OperationStartAttributes, RunEnd, RunOperationKind, RunOutcome as TelemetryRunOutcome, RunSpan,
    RunStart, SpanStatus,
};

impl AgentSession {
    /// Invokes an exact registered command from a product control.
    ///
    /// Unknown names are errors, never model input. The existing submission
    /// pipeline executes handled commands immediately even during an active
    /// run; transformed input passes through input hooks and the normal queue
    /// exactly once. This does not manufacture a tool call or result.
    pub async fn invoke_command(
        &self,
        name: &str,
        arguments: &str,
    ) -> Result<PromptOutcome, SessionError> {
        self.ensure_open()?;
        if name.is_empty()
            || name
                .chars()
                .any(|character| character.is_whitespace() || character == '/')
            || !self
                .runtime
                .command_specs()
                .iter()
                .any(|command| command.name == name)
        {
            return Err(SessionError::Runtime(format!(
                "registered command is unavailable: {name}"
            )));
        }
        self.prompt(if arguments.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {arguments}")
        })
        .await
    }

    /// Processes user text and images through commands and input hooks.
    ///
    /// Handled input returns without starting a run. During an active durable
    /// run, unhandled input is queued as steering; otherwise this waits through
    /// the complete run and its settled callbacks.
    pub async fn prompt(
        &self,
        input: impl Into<SessionInput>,
    ) -> Result<PromptOutcome, SessionError> {
        let input = input.into();
        if self.accepts_live_messages() {
            return self.queue_input(input, QueueKind::Steer).await;
        }
        let Some(submission) = self.prepare_text_command(input).await? else {
            return Ok(PromptOutcome::Handled);
        };
        Ok(match self.run_submission(submission).await? {
            Some(outcome) => PromptOutcome::Agent(outcome),
            None => PromptOutcome::Handled,
        })
    }

    /// Commands may switch the current session, so they run before activity
    /// admission. Input hooks remain inside the admitted generation lease.
    pub(super) async fn prepare_text_command(
        &self,
        mut input: SessionInput,
    ) -> Result<Option<SessionSubmission>, SessionError> {
        self.ensure_open()?;
        let display_text = input.text.clone();
        if let Some(outcome) = self.runtime.execute_command(&input.text).await? {
            match outcome {
                CommandOutcome::Handled => return Ok(None),
                CommandOutcome::TransformInput(transformed) => input.text = transformed,
            }
        }
        Ok(Some(SessionSubmission::Text {
            display_text,
            input,
        }))
    }

    pub(super) async fn run_submission(
        &self,
        submission: SessionSubmission,
    ) -> Result<Option<AgentLoopOutcome>, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Preparing).await;
        self.ensure_open()?;
        self.maybe_threshold_compact_locked().await;
        let (prepared, run_id) = match submission {
            SessionSubmission::Text {
                display_text,
                input,
            } => {
                let prepared = match self
                    .runtime
                    .prepare_submission_after_command(display_text, input.text, input.images)
                    .await?
                {
                    PreparedTextSubmission::Handled => return Ok(None),
                    PreparedTextSubmission::Agent(prepared) => prepared,
                };
                if self.activity.is_aborted() {
                    return Err(SessionError::Cancelled("session input"));
                }
                let run_id = self.activity.begin_text(
                    prepared.generation(),
                    prepared.display_text(),
                    prepared.text(),
                    prepared.images(),
                )?;
                (PreparedSessionPrompt::Text(prepared), run_id)
            }
            SessionSubmission::Messages(messages) => {
                let prepared = self.runtime.prepare_message_submission(messages).await;
                if self.activity.is_aborted() {
                    return Err(SessionError::Cancelled("session input"));
                }
                let run_id = self
                    .activity
                    .begin_messages(prepared.generation(), prepared.messages())?;
                (PreparedSessionPrompt::Messages(prepared), run_id)
            }
        };
        let telemetry = self.runtime.agent().telemetry_context();
        let run_span = telemetry.start_span::<RunSpan>(RunStart {
            operation: OperationStartAttributes {
                session_id: self
                    .runtime
                    .agent()
                    .session_id()
                    .unwrap_or_else(|| "unsaved".to_string()),
                lane_name: MAIN_LANE.to_string(),
                operation_id: run_id.clone(),
                recovery: false,
            },
            kind: RunOperationKind::Run,
        });
        let recorded = match prepared {
            PreparedSessionPrompt::Text(prepared) => prepared.run().await,
            PreparedSessionPrompt::Messages(prepared) => prepared.run().await,
        };
        let mut result = match recorded {
            Ok(outcome) => self.finish_prompt_locked(outcome).await,
            Err(error) => Err(SessionError::from(error)),
        };
        if let Err(error) = self.context_updates.reconcile()
            && result.is_ok()
        {
            result = Err(error);
        }
        let finish_result = self
            .activity
            .finish_run(self.runtime.agent(), &run_id, &result);
        self.emit_agent_settled().await;
        let (telemetry_outcome, telemetry_error) = match (&result, &finish_result) {
            (Ok(outcome), Ok(())) if outcome.stop == AgentLoopStop::Aborted => {
                (TelemetryRunOutcome::Aborted, None)
            }
            (Ok(_), Ok(())) => (TelemetryRunOutcome::Completed, None),
            (Err(_), _) | (Ok(_), Err(_)) => (
                TelemetryRunOutcome::Failed,
                Some(("session_run_failed".to_string(), "session".to_string())),
            ),
        };
        run_span.set_end_attributes(RunEnd {
            outcome: Some(telemetry_outcome),
            error_code: telemetry_error.as_ref().map(|(code, _)| code.clone()),
            error_type: telemetry_error.map(|(_, message)| message),
        });
        if telemetry_outcome == TelemetryRunOutcome::Failed {
            run_span.set_status(SpanStatus::Error);
        }
        run_span.finish();
        match (result, finish_result) {
            (Ok(outcome), Ok(())) => Ok(Some(outcome)),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Queues a steering message for the active run after command/input-hook
    /// preprocessing. The durable queue record is committed before the Agent
    /// can observe the message.
    pub async fn steer(
        &self,
        input: impl Into<SessionInput>,
    ) -> Result<PromptOutcome, SessionError> {
        self.queue_input(input.into(), QueueKind::Steer).await
    }

    /// Queues a follow-up message for the active run.
    pub async fn follow_up(
        &self,
        input: impl Into<SessionInput>,
    ) -> Result<PromptOutcome, SessionError> {
        self.queue_input(input.into(), QueueKind::FollowUp).await
    }

    /// Enqueues an extension-created user or custom message without running
    /// text preprocessing. The durable queue record is committed before the
    /// live agent can observe steer/follow-up delivery.
    pub fn enqueue_message(
        &self,
        message: Message,
        kind: QueueKind,
    ) -> Result<PromptOutcome, SessionError> {
        self.ensure_open()?;
        self.activity
            .enqueue_message(self.runtime.agent(), message, kind)
    }

    /// Clears durable and live queues together and returns their editor-ready text.
    pub fn clear_queue(&self) -> Result<QueueSnapshot, SessionError> {
        self.ensure_open()?;
        self.activity.clear_queue(self.runtime.agent())
    }

    /// Requests cancellation without waiting for the operation gate. Any
    /// undelivered queue items are retained for [`Self::clear_queue`].
    pub fn abort(&self) {
        self.activity.abort();
        self.runtime.abort();
    }

    pub fn abort_retry(&self) {
        self.activity.abort_stage(CancellationStage::Retry);
    }

    pub(super) async fn queue_input(
        &self,
        input: SessionInput,
        kind: QueueKind,
    ) -> Result<PromptOutcome, SessionError> {
        self.ensure_open()?;
        let expected = self.activity.delivery_target()?;
        let behavior = match kind {
            QueueKind::Steer => InputStreamingBehavior::Steer,
            QueueKind::FollowUp => InputStreamingBehavior::FollowUp,
            QueueKind::NextRun => {
                return Err(SessionError::InvalidEntry(
                    "next-run queue cannot target an active run".to_string(),
                ));
            }
        };
        let prepared = self
            .runtime
            .process_queued_input(input.text, input.images, behavior)
            .await?;
        self.activity
            .enqueue_text(self.runtime.agent(), expected, kind, prepared)
    }

    /// Runs a protocol-neutral, already-structured message batch.
    ///
    /// External adapters use this when their prompt vocabulary contains roles
    /// or content blocks beyond [`SessionInput`]'s text-and-image interface.
    /// It skips text commands/input hooks and waits for activity admission and
    /// full settlement, rather than queuing input into an existing run.
    pub async fn run_messages(
        &self,
        messages: Vec<Message>,
    ) -> Result<AgentLoopOutcome, SessionError> {
        Ok(self
            .run_submission(SessionSubmission::Messages(messages))
            .await?
            .expect("structured input cannot be handled by text preprocessing"))
    }

    pub(super) async fn emit_agent_settled(&self) {
        self.runtime.dispatch_agent_settled().await;
        self.events
            .publish_agent_settled(self.runtime.agent().state());
    }

    pub(super) async fn finish_prompt_locked(
        &self,
        outcome: AgentLoopOutcome,
    ) -> Result<AgentLoopOutcome, SessionError> {
        self.activity.set_retry_attempt(0);
        let mut outcome = outcome;
        let mut retry_attempt = 0_u32;
        let mut overflow_recovery_attempted = false;
        let mut continued_messages = Vec::new();
        loop {
            self.context_updates.reconcile()?;
            outcome.final_context.messages = self.runtime.agent().state().messages.as_ref().clone();
            if !self.activity.is_aborted() && self.is_overflow_outcome(&outcome) {
                if retry_attempt > 0 {
                    self.events.publish_auto_retry_end(
                        false,
                        retry_attempt,
                        assistant_error_message(&outcome).map(ToOwned::to_owned),
                    );
                    retry_attempt = 0;
                    self.activity.set_retry_attempt(0);
                }
                if self.compaction_settings().enabled && !overflow_recovery_attempted {
                    overflow_recovery_attempted = true;
                    if self
                        .run_compaction_locked(
                            crate::CompactionReason::Overflow,
                            true,
                            None,
                            true,
                            CompactionActivation::Immediate,
                        )
                        .await
                        .is_ok()
                        && !self.activity.is_aborted()
                        && let Ok(retried) = self.runtime.continue_activity().await
                    {
                        outcome = retried;
                        continue;
                    }
                }
            }

            let retry_settings = self.retry_settings();
            if !self.activity.is_aborted()
                && retry_settings.enabled
                && retry_attempt < retry_settings.max_retries
                && is_retryable_assistant_outcome(&outcome)
            {
                retry_attempt = retry_attempt.saturating_add(1);
                self.activity.set_retry_attempt(retry_attempt);
                let delay_ms = retry_delay_ms(retry_settings.base_delay_ms, retry_attempt);
                self.events.publish_auto_retry_start(
                    retry_attempt,
                    retry_settings.max_retries,
                    delay_ms,
                    assistant_error_message(&outcome)
                        .unwrap_or("Unknown error")
                        .to_string(),
                );
                if let Some(assistant) = last_assistant(&outcome) {
                    self.context_updates.omit_failed_assistant(assistant)?;
                }
                if !self.wait_for_retry_delay(delay_ms).await {
                    self.events.publish_auto_retry_end(
                        false,
                        retry_attempt,
                        Some("Retry cancelled".to_string()),
                    );
                    continued_messages.append(&mut outcome.new_messages);
                    outcome.new_messages = continued_messages;
                    // Cancellation stops the scheduled retry; the last provider attempt keeps
                    // its error outcome because no aborted response was started.
                    return Ok(outcome);
                }
                match self.runtime.continue_activity().await {
                    Ok(retried) => {
                        outcome = retried;
                        continue;
                    }
                    Err(error) => {
                        self.events.publish_auto_retry_end(
                            false,
                            retry_attempt,
                            Some(error.to_string()),
                        );
                        return Err(error.into());
                    }
                }
            }

            if retry_attempt > 0 {
                self.events.publish_auto_retry_end(
                    assistant_outcome_succeeded(&outcome),
                    retry_attempt,
                    assistant_error_message(&outcome).map(ToOwned::to_owned),
                );
                retry_attempt = 0;
                self.activity.set_retry_attempt(0);
            }
            self.maybe_threshold_compact_locked().await;
            if !self.activity.is_aborted() && outcome.stop != AgentLoopStop::Aborted {
                if !self.activity.is_stopped() && self.runtime.agent().has_queued_messages() {
                    continued_messages.append(&mut outcome.new_messages);
                    outcome = self.runtime.continue_activity().await?;
                    continue;
                }
                let (abort, signal) = AbortHandle::new();
                self.activity
                    .register_cancellation(CancellationStage::Boundary, abort);
                let activity = match outcome.stop {
                    AgentLoopStop::ProviderError | AgentLoopStop::MaxToolIterations => {
                        pi_plugin::AgentActivityOutcome::Error
                    }
                    _ => pi_plugin::AgentActivityOutcome::Completed,
                };
                let run_id = pi_core::RunId::new(
                    self.activity
                        .run_id()
                        .unwrap_or_else(|| self.store.id().to_owned()),
                );
                let continuation = {
                    let _phase = self.activity.phase(ActivityPhase::BeforeSettle);
                    self.run_boundary(&run_id, None, activity, &signal).await
                };
                self.activity
                    .clear_cancellation(CancellationStage::Boundary);
                if continuation? && !self.activity.is_aborted() && !self.activity.is_stopped() {
                    self.context_updates.reconcile()?;
                    continued_messages.append(&mut outcome.new_messages);
                    outcome = self.runtime.continue_activity().await?;
                    continue;
                }
            }
            self.context_updates.reconcile()?;
            outcome.final_context.messages = self.runtime.agent().state().messages.as_ref().clone();
            if self.activity.is_aborted() {
                outcome.stop = AgentLoopStop::Aborted;
            }
            continued_messages.append(&mut outcome.new_messages);
            outcome.new_messages = continued_messages;
            return Ok(outcome);
        }
    }

    pub(super) async fn wait_for_retry_delay(&self, delay_ms: u64) -> bool {
        let _phase = self.activity.phase(ActivityPhase::RetryWait);
        let (abort, signal) = AbortHandle::new();
        self.activity
            .register_cancellation(CancellationStage::Retry, abort);
        let cancelled = tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => false,
            () = signal.wait() => true,
        };
        self.activity.clear_cancellation(CancellationStage::Retry);
        !cancelled
    }
}

pub(super) fn last_assistant(outcome: &AgentLoopOutcome) -> Option<&pi_core::AssistantMessage> {
    outcome
        .final_context
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message) => Some(message.as_ref()),
            _ => None,
        })
}

pub(super) fn assistant_error_message(outcome: &AgentLoopOutcome) -> Option<&str> {
    last_assistant(outcome)?.error_message.as_deref()
}

pub(super) fn assistant_outcome_succeeded(outcome: &AgentLoopOutcome) -> bool {
    last_assistant(outcome).is_some_and(|message| message.stop_reason != StopReason::Error)
}

pub(super) fn will_retry_after_agent_end(
    messages: &[Message],
    settings: AutoRetrySettings,
    attempt: u32,
) -> bool {
    if !settings.enabled || attempt >= settings.max_retries {
        return false;
    }
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(message) => Some(
            message.stop_reason == StopReason::Error
                && message
                    .error_message
                    .as_deref()
                    .is_some_and(pi_plugin::is_retryable_provider_error_message),
        ),
        _ => None,
    }) == Some(true)
}

pub(super) fn retry_delay_ms(base_delay_ms: u64, attempt: u32) -> u64 {
    let multiplier = 1_u64
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u64::MAX);
    base_delay_ms.saturating_mul(multiplier)
}

pub(super) fn is_retryable_assistant_outcome(outcome: &AgentLoopOutcome) -> bool {
    let Some(message) = last_assistant(outcome) else {
        return false;
    };
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error) = message.error_message.as_deref() else {
        return false;
    };
    pi_plugin::is_retryable_provider_error_message(error)
}
