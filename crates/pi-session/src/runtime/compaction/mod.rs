use super::activity::{ActivityPhase, CancellationStage};
use super::context::ContextActivation;
use super::{AgentSession, CompactionActivation, CompletedCompaction};
use crate::{
    CompactionEntry, CompactionPreparation, FileOperations, SessionBeforeCompactEvent,
    SessionCompactEvent, SessionCompactFailedEvent, SessionEntry, SessionError,
    compact as generate_compaction,
};
use pi_agent::{AgentContext, AgentLoopOutcome, AgentTurnContext};
use pi_core::{AbortHandle, Message, StopReason};
use pi_runtime::{CompletionRetryPolicy, RuntimeCompletionRequest, RuntimeError};
mod algorithm;
pub use algorithm::*;

struct SessionCompactionCompleter<'a> {
    session: &'a AgentSession,
    reason: crate::CompactionReason,
}

#[async_trait::async_trait]
impl CompactionCompleter for SessionCompactionCompleter<'_> {
    fn model_capabilities(&self) -> CompactionModelCapabilities {
        self.session.runtime.model_capabilities()
    }

    async fn complete_compaction(
        &self,
        request: CompactionCompletionRequest,
        signal: pi_core::AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError> {
        algorithm::compaction_completion(
            self.session
                .complete_summary(
                    algorithm::compaction_request(&self.session.runtime, request),
                    signal,
                    crate::SummarizationSource::Compaction {
                        reason: self.reason,
                    },
                )
                .await,
        )
    }
}

impl AgentSession {
    pub(super) async fn complete_summary(
        &self,
        request: RuntimeCompletionRequest,
        signal: pi_core::AbortSignal,
        source: crate::SummarizationSource,
    ) -> Result<pi_core::AssistantMessage, RuntimeError> {
        let settings = self.retry_settings();
        self.runtime
            .complete_with_retry(
                request,
                signal,
                Some(CompletionRetryPolicy {
                    enabled: settings.enabled,
                    max_retries: settings.max_retries,
                    base_delay_ms: settings.base_delay_ms,
                }),
                |event| self.events.publish_summarization_retry(source, event),
            )
            .await
    }
    pub(super) async fn compact_before_next_turn(
        &self,
        turn: &AgentTurnContext,
    ) -> Option<AgentContext> {
        let context_window = self.active_context_window()?;
        let entries = self.store.branch_entries().ok()?;
        let session_context =
            crate::build_session_context(&entries, self.context_updates.options());
        let tokens = estimate_session_context_tokens(&entries, &session_context.messages).tokens;
        if !should_compact(tokens, context_window, self.compaction_settings()) {
            return None;
        }

        let compacted = self
            .run_compaction_locked(
                crate::CompactionReason::Threshold,
                false,
                None,
                false,
                CompactionActivation::AfterRun,
            )
            .await
            .ok()?;
        let mut next = turn.context.as_ref().clone();
        next.messages = compacted.context.runtime_messages();
        Some(next)
    }

    pub async fn append_compaction(
        &self,
        mut compaction: CompactionEntry,
    ) -> Result<String, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        let reason = crate::CompactionReason::Manual;
        let branch_entries = self.store.branch_entries()?;
        let compaction_settings = self.compaction_settings();
        let preparation = prepare_compaction(
            &branch_entries,
            compaction_settings,
            self.context_updates.options(),
            self.compaction_policy.as_ref(),
        )
        .unwrap_or_else(|| CompactionPreparation {
            messages_to_summarize: Vec::new(),
            turn_prefix_messages: Vec::new(),
            retained_tail: compaction.retained_tail.clone(),
            is_split_turn: false,
            tokens_before: compaction.tokens_before,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: compaction_settings,
        });
        self.events.publish_compaction_start(reason);
        let (_, signal) = AbortHandle::new();
        let before = self
            .plugin_driver()
            .session_before_compact(
                self.session_dispatch_context(),
                &SessionBeforeCompactEvent {
                    preparation,
                    branch_entries,
                    custom_instructions: None,
                    reason,
                    will_retry: false,
                    signal,
                },
            )
            .await;
        if before.as_ref().is_some_and(|result| result.cancel) {
            self.plugin_driver()
                .session_compact_failed(
                    self.session_dispatch_context(),
                    &SessionCompactFailedEvent {
                        reason,
                        error_message: None,
                        aborted: true,
                        will_retry: false,
                        from_extension: false,
                    },
                )
                .await;
            self.events
                .publish_compaction_end(reason, None, true, false, None);
            return Err(SessionError::Cancelled("session compaction"));
        }
        let from_extension = before
            .and_then(|result| result.compaction)
            .map(|replacement| compaction = replacement)
            .is_some();

        crate::history::annotate_compaction(
            &mut compaction,
            &self.store.branch_entries()?,
            self.context_updates.options(),
        );
        let result = self.context_updates.append(
            SessionEntry::Compaction(compaction.clone()),
            ContextActivation::Restore,
        );

        match result {
            Ok(committed) => {
                let record = committed.record;
                let id = record.id.clone();
                self.plugin_driver()
                    .session_compact(
                        self.session_dispatch_context(),
                        &SessionCompactEvent {
                            compaction_entry: compaction.clone(),
                            from_extension,
                            reason,
                            will_retry: false,
                        },
                    )
                    .await;
                self.events
                    .publish_compaction_end(reason, Some(record), false, false, None);
                Ok(id)
            }
            Err(error) => {
                self.plugin_driver()
                    .session_compact_failed(
                        self.session_dispatch_context(),
                        &SessionCompactFailedEvent {
                            reason,
                            error_message: Some(error.to_string()),
                            aborted: false,
                            will_retry: false,
                            from_extension,
                        },
                    )
                    .await;
                self.events.publish_compaction_end(
                    reason,
                    None,
                    false,
                    false,
                    Some(error.to_string()),
                );
                Err(error)
            }
        }
    }

    /// Generates, persists, and activates a structured compaction checkpoint.
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<CompactionEntry, SessionError> {
        self.abort_and_wait().await;
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        self.run_compaction_locked(
            crate::CompactionReason::Manual,
            false,
            custom_instructions,
            false,
            CompactionActivation::Immediate,
        )
        .await
        .map(|completed| completed.entry)
    }

    /// Aborts an in-flight summary request without waiting for the session
    /// operation gate, matching Pi's `abortCompaction` behavior.
    pub fn abort_compaction(&self) {
        self.activity.abort_stage(CancellationStage::Compaction);
    }

    pub(super) async fn maybe_threshold_compact_locked(&self) {
        let Some(context_window) = self.active_context_window() else {
            return;
        };
        let Ok(entries) = self.store.branch_entries() else {
            return;
        };
        let context = crate::build_session_context(&entries, self.context_updates.options());
        let tokens = estimate_session_context_tokens(&entries, &context.messages).tokens;
        if should_compact(tokens, context_window, self.compaction_settings()) {
            let _ = self
                .run_compaction_locked(
                    crate::CompactionReason::Threshold,
                    false,
                    None,
                    false,
                    CompactionActivation::Immediate,
                )
                .await;
        }
    }

    pub(super) fn is_overflow_outcome(&self, outcome: &AgentLoopOutcome) -> bool {
        let Some(Message::Assistant(message)) = outcome.final_context.messages.last() else {
            return false;
        };
        let explicit_context_error = message.stop_reason == StopReason::Error
            && message.error_message.as_deref().is_some_and(|error| {
                let error = error.to_ascii_lowercase();
                error.contains("context")
                    && ["length", "window", "token", "overflow", "too long"]
                        .iter()
                        .any(|needle| error.contains(needle))
            });
        if explicit_context_error {
            return true;
        }
        let Some(context_window) = self.active_context_window() else {
            return false;
        };
        message.stop_reason == StopReason::Length
            && should_compact(
                estimate_session_context_tokens(
                    &self.store.branch_entries().unwrap_or_default(),
                    &outcome
                        .final_context
                        .messages
                        .iter()
                        .cloned()
                        .map(crate::AgentMessage::from)
                        .collect::<Vec<_>>(),
                )
                .tokens,
                context_window,
                self.compaction_settings(),
            )
    }

    pub(super) async fn run_compaction_locked(
        &self,
        reason: crate::CompactionReason,
        will_retry: bool,
        custom_instructions: Option<String>,
        remove_failed_assistant: bool,
        activation: CompactionActivation,
    ) -> Result<CompletedCompaction, SessionError> {
        let _phase = self.activity.phase(ActivityPhase::Compacting);
        if reason == crate::CompactionReason::Manual {
            self.events.publish_compaction_start(reason);
        }
        let branch_entries = self.store.branch_entries()?;
        let mut preparation_entries = branch_entries.clone();
        if remove_failed_assistant
            && let Some(index) = preparation_entries.iter().rposition(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Message(message)
                        if matches!(
                            message.message.as_standard(),
                            Some(Message::Assistant(assistant))
                                if matches!(assistant.stop_reason, StopReason::Error | StopReason::Length)
                        )
                )
            })
        {
            preparation_entries.remove(index);
        }
        let Some(preparation) = prepare_compaction(
            &preparation_entries,
            self.compaction_settings(),
            self.context_updates.options(),
            self.compaction_policy.as_ref(),
        ) else {
            let error = if preparation_entries
                .last()
                .is_some_and(|record| matches!(record.entry, SessionEntry::Compaction(_)))
            {
                SessionError::AlreadyCompacted
            } else {
                SessionError::NothingToCompact
            };
            if reason == crate::CompactionReason::Manual {
                let error_message = Some(format!("Compaction failed: {error}"));
                self.events.publish_compaction_end(
                    reason,
                    None,
                    false,
                    false,
                    error_message.clone(),
                );
                self.plugin_driver()
                    .session_compact_failed(
                        self.session_dispatch_context(),
                        &SessionCompactFailedEvent {
                            reason,
                            error_message,
                            aborted: false,
                            will_retry: false,
                            from_extension: false,
                        },
                    )
                    .await;
            }
            return Err(error);
        };
        if reason != crate::CompactionReason::Manual {
            self.events.publish_compaction_start(reason);
        }

        let (abort_handle, signal) = AbortHandle::new();
        self.activity
            .register_cancellation(CancellationStage::Compaction, abort_handle);
        let before = self
            .plugin_driver()
            .session_before_compact(
                self.session_dispatch_context(),
                &SessionBeforeCompactEvent {
                    preparation: preparation.clone(),
                    branch_entries,
                    custom_instructions: custom_instructions.clone(),
                    reason,
                    will_retry,
                    signal: signal.clone(),
                },
            )
            .await;
        if signal.is_aborted() || before.as_ref().is_some_and(|result| result.cancel) {
            self.activity
                .clear_cancellation(CancellationStage::Compaction);
            let error = SessionError::Cancelled("session compaction");
            self.publish_compaction_failure(reason, &error, false).await;
            return Err(error);
        }

        let replacement = before.and_then(|result| result.compaction);
        let from_extension = replacement.is_some();
        let generated = match replacement {
            Some(compaction) => Ok(compaction),
            None => generate_compaction(
                &preparation,
                &SessionCompactionCompleter {
                    session: self,
                    reason,
                },
                self.compaction_policy.as_ref(),
                custom_instructions.as_deref(),
                self.runtime.agent().state().thinking_level,
                signal,
            )
            .await
            .map_err(|error| match error {
                CompactionError::Aborted(_) => SessionError::Cancelled("session compaction"),
                CompactionError::SummarizationFailed(message) => {
                    SessionError::CompactionFailed(message)
                }
            }),
        };
        self.activity
            .clear_cancellation(CancellationStage::Compaction);
        let mut compaction = match generated {
            Ok(compaction) => compaction,
            Err(error) => {
                self.publish_compaction_failure(reason, &error, from_extension)
                    .await;
                return Err(error);
            }
        };

        if reason != crate::CompactionReason::Manual {
            for message in crate::runtime::compaction::unsent_context_inputs(
                &preparation_entries,
                self.context_updates.options(),
            ) {
                if !compaction.retained_tail.contains(&message) {
                    compaction.retained_tail.push(message);
                }
            }
        }
        crate::history::annotate_compaction(
            &mut compaction,
            &preparation_entries,
            self.context_updates.options(),
        );
        let activation = match activation {
            CompactionActivation::AfterRun => ContextActivation::AfterRun,
            CompactionActivation::Immediate if self.activity.accepts_live_messages() => {
                ContextActivation::KeepConfiguration
            }
            CompactionActivation::Immediate => ContextActivation::Restore,
        };
        let persisted = self
            .context_updates
            .append(SessionEntry::Compaction(compaction.clone()), activation);
        match persisted {
            Ok(committed) => {
                let record = committed.record;
                let context = committed.context;
                self.plugin_driver()
                    .session_compact(
                        self.session_dispatch_context(),
                        &SessionCompactEvent {
                            compaction_entry: compaction.clone(),
                            from_extension,
                            reason,
                            will_retry,
                        },
                    )
                    .await;
                self.events
                    .publish_compaction_end(reason, Some(record), false, will_retry, None);
                Ok(CompletedCompaction {
                    entry: compaction,
                    context,
                })
            }
            Err(error) => {
                self.publish_compaction_failure(reason, &error, from_extension)
                    .await;
                Err(error)
            }
        }
    }

    async fn publish_compaction_failure(
        &self,
        reason: crate::CompactionReason,
        error: &SessionError,
        from_extension: bool,
    ) {
        let aborted = matches!(error, SessionError::Cancelled(_));
        let error_message = (!aborted).then(|| {
            let prefix = match reason {
                crate::CompactionReason::Manual => "Compaction failed",
                crate::CompactionReason::Threshold => "Auto-compaction failed",
                crate::CompactionReason::Overflow => "Context overflow recovery failed",
            };
            format!("{prefix}: {error}")
        });
        self.events
            .publish_compaction_end(reason, None, aborted, false, error_message.clone());
        self.plugin_driver()
            .session_compact_failed(
                self.session_dispatch_context(),
                &SessionCompactFailedEvent {
                    reason,
                    error_message,
                    aborted,
                    will_retry: false,
                    from_extension,
                },
            )
            .await;
    }
}
