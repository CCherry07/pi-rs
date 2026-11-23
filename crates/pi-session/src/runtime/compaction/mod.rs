use super::activity::{ActivityPhase, CancellationStage};
use super::context::ContextActivation;
use super::{AgentSession, CompactionActivation, CompletedCompaction};
use crate::{
    CompactionEntry, CompactionPreparation, FileOperations, SessionBeforeCompactEvent,
    SessionCompactEvent, SessionCompactFailedEvent, SessionEntry, SessionError,
    compact as generate_compaction,
};
use pi_agent::{AgentContext, AgentTurnContext};
use pi_core::AbortHandle;
use pi_runtime::{CompletionRetryPolicy, RuntimeCompletionRequest, RuntimeError};
mod algorithm;
mod automatic;
mod overflow;
mod runtime_adapter;
mod session_adapter;

pub(crate) use algorithm::summarization_failure;
pub use algorithm::{
    CompactionCompleter, CompactionCompletion, CompactionCompletionRequest, CompactionError,
    CompactionModelCapabilities, ContextUsageEstimate, GenericCompactionPolicy,
    SUMMARIZATION_SYSTEM_PROMPT, SessionCompactionPolicy, calculate_context_tokens,
    estimate_context_tokens, estimate_tokens, should_compact,
};
pub(crate) use session_adapter::estimate_projected_context_tokens;
pub use session_adapter::{
    compact, current_session_context_tokens, estimate_session_context_tokens, prepare_compaction,
    serialize_conversation,
};

#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;

pub(super) fn is_context_overflow_error(message: &pi_core::AssistantMessage) -> bool {
    message.stop_reason == pi_core::StopReason::Error
        && message.error_message.as_deref().is_some_and(|error| {
            // Agent's typed ProviderError adds a Rust diagnostic prefix. Classify the
            // original provider text so anchored status/exclusion rules retain their meaning.
            let error = error
                .strip_prefix("provider failed: ")
                .or_else(|| error.strip_prefix("provider protocol error: "))
                .unwrap_or(error);
            overflow::is_overflow_error(error, message.provider.as_str())
        })
}

struct SessionCompactionCompleter<'a> {
    session: &'a AgentSession,
    reason: crate::CompactionReason,
    model: pi_core::ModelSelection,
}

#[async_trait::async_trait]
impl CompactionCompleter for SessionCompactionCompleter<'_> {
    fn model_capabilities(&self) -> CompactionModelCapabilities {
        self.session
            .runtime
            .model(&self.model.provider, &self.model.model_id)
            .map_or_else(
                || self.session.runtime.model_capabilities(),
                |model| CompactionModelCapabilities {
                    reasoning: model.reasoning,
                    max_output_tokens: (model.max_tokens > 0).then_some(model.max_tokens),
                },
            )
    }

    async fn complete_compaction(
        &self,
        request: CompactionCompletionRequest,
        signal: pi_core::AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError> {
        let mut request = runtime_adapter::compaction_request(&self.session.runtime, request);
        request.model = Some(self.model.clone());
        runtime_adapter::compaction_completion(
            self.session
                .complete_summary(
                    request,
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
        let request = self.runtime.prepare_summarization_request(request);
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
    ) -> Result<Option<AgentContext>, SessionError> {
        if self.activity.is_aborted() {
            return Ok(None);
        }
        let (provider, model) = self.runtime.agent().model_selection();
        let settings = self.resolved_compaction_settings_for(&provider, &model)?;
        let Some(context_window) = self.active_context_window() else {
            return Ok(None);
        };
        let entries = self.store.branch_entries()?;
        let (context, projection) = crate::history::build_session_context_and_entries(
            &entries,
            self.context_updates.options(),
        );
        let tokens =
            estimate_projected_context_tokens(&entries, &context.messages, &projection).tokens;
        if !should_compact(tokens, context_window, settings) {
            return Ok(None);
        }
        let Ok(compacted) = self
            .run_compaction_locked(
                crate::CompactionReason::Threshold,
                false,
                None,
                CompactionActivation::AfterRun,
            )
            .await
        else {
            return Ok(None);
        };
        let mut next = turn.context.as_ref().clone();
        next.messages = compacted.context.runtime_messages();
        Ok(Some(next))
    }

    pub async fn append_compaction(
        &self,
        mut compaction: CompactionEntry,
    ) -> Result<String, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        let reason = crate::CompactionReason::Manual;
        let branch_entries = self.store.branch_entries()?;
        let (provider, model) = self.runtime.agent().model_selection();
        let compaction_settings = self.resolved_compaction_settings_for(&provider, &model)?;
        let preparation = prepare_compaction(
            &branch_entries,
            compaction_settings,
            self.context_updates.options(),
            self.compaction_policy.as_ref(),
        )
        .unwrap_or_else(|| CompactionPreparation {
            first_kept_entry_id: None,
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
                .publish_compaction_end(reason, None, true, false, None, None);
            return Err(SessionError::Cancelled("session compaction"));
        }
        let from_extension = before
            .and_then(|result| result.compaction)
            .map(|replacement| compaction = replacement)
            .is_some();

        compaction.from_hook = Some(from_extension);
        compaction.system_message =
            crate::history::current_system_snapshot(&self.context_updates.current()?.messages)?;
        crate::history::annotate_compaction(
            &mut compaction,
            &self.store.branch_entries()?,
            self.context_updates.options(),
        );
        let result = self.context_updates.append_from(
            SessionEntry::Compaction(compaction.clone()),
            ContextActivation::Restore,
            crate::EntryAppendOrigin::Compaction,
        );

        match result {
            Ok(committed) => {
                let record = committed.record;
                let SessionEntry::Compaction(compaction) = &record.entry else {
                    unreachable!()
                };
                let id = record.id.clone();
                let estimated_tokens_after =
                    committed.context.messages.iter().map(estimate_tokens).sum();
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
                self.events.publish_compaction_end(
                    reason,
                    Some(record),
                    false,
                    false,
                    None,
                    Some(estimated_tokens_after),
                );
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
                    None,
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
        let result = self.compact_with_result(custom_instructions).await?;
        let SessionEntry::Compaction(entry) = result.record.entry else {
            unreachable!()
        };
        Ok(entry)
    }

    pub async fn compact_with_result(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<super::SessionCompactionResult, SessionError> {
        self.abort_and_wait().await;
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        self.run_compaction_locked(
            crate::CompactionReason::Manual,
            false,
            custom_instructions,
            CompactionActivation::Immediate,
        )
        .await
        .map(|completed| super::SessionCompactionResult {
            record: completed.record,
            estimated_tokens_after: completed.estimated_tokens_after,
        })
    }

    /// Aborts an in-flight summary request without waiting for the session
    /// operation gate, matching Pi's `abortCompaction` behavior.
    pub fn abort_compaction(&self) {
        self.activity.abort_stage(CancellationStage::Compaction);
    }

    pub(super) async fn run_compaction_locked(
        &self,
        reason: crate::CompactionReason,
        will_retry: bool,
        custom_instructions: Option<String>,
        activation: CompactionActivation,
    ) -> Result<CompletedCompaction, SessionError> {
        let state = self.runtime.agent().state();
        let settings =
            self.resolved_compaction_settings_for(&state.provider_id, &state.model_id)?;
        let model = pi_core::ModelSelection::new(state.provider_id.clone(), state.model_id.clone());
        let thinking_level = state.thinking_level;
        let (abort_handle, signal) = AbortHandle::new();
        let _cancellation = self
            .activity
            .cancellation(CancellationStage::Compaction, abort_handle);
        let _phase = self.activity.phase(ActivityPhase::Compacting);
        if reason == crate::CompactionReason::Manual {
            self.events.publish_compaction_start(reason);
        }
        let branch_entries = self.store.branch_entries()?;
        let preparation_entries = branch_entries.clone();
        let Some(preparation) = prepare_compaction(
            &preparation_entries,
            settings,
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
                    None,
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

        if signal.is_aborted() {
            let error = SessionError::Cancelled("session compaction");
            self.publish_compaction_failure(reason, &error, false).await;
            return Err(error);
        }
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
                    model,
                },
                self.compaction_policy.as_ref(),
                custom_instructions.as_deref(),
                thinking_level,
                signal.clone(),
            )
            .await
            .map_err(|error| match error {
                CompactionError::Aborted(_) => SessionError::Cancelled("session compaction"),
                CompactionError::SummarizationFailed(message) => {
                    SessionError::CompactionFailed(message)
                }
            }),
        };
        if signal.is_aborted() {
            let error = SessionError::Cancelled("session compaction");
            self.publish_compaction_failure(reason, &error, from_extension)
                .await;
            return Err(error);
        }
        let mut compaction = match generated {
            Ok(compaction) => compaction,
            Err(error) => {
                self.publish_compaction_failure(reason, &error, from_extension)
                    .await;
                return Err(error);
            }
        };

        compaction.from_hook = Some(from_extension);
        compaction.system_message =
            crate::history::current_system_snapshot(&self.context_updates.current()?.messages)?;
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
        let persisted = self.context_updates.append_from(
            SessionEntry::Compaction(compaction.clone()),
            activation,
            crate::EntryAppendOrigin::Compaction,
        );
        match persisted {
            Ok(committed) => {
                let record = committed.record;
                let SessionEntry::Compaction(compaction) = &record.entry else {
                    unreachable!()
                };
                let compaction = compaction.clone();
                let context = committed.context;
                let estimated_tokens_after = context.messages.iter().map(estimate_tokens).sum();
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
                self.events.publish_compaction_end(
                    reason,
                    Some(record.clone()),
                    false,
                    will_retry,
                    None,
                    Some(estimated_tokens_after),
                );
                Ok(CompletedCompaction {
                    context,
                    record,
                    estimated_tokens_after,
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
        self.events.publish_compaction_end(
            reason,
            None,
            aborted,
            false,
            error_message.clone(),
            None,
        );
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
