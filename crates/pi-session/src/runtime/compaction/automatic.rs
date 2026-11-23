//! Automatic triggering is Session policy; provider signals and preparation are pure.
use pi_core::{AssistantMessage, Message, StopReason};

use super::super::{AgentSession, CompactionActivation};
use super::overflow::{is_context_overflow, is_recoverable_length};
use super::{
    calculate_context_tokens, estimate_context_tokens, estimate_projected_context_tokens,
    should_compact,
};
use crate::{
    CompactionReason, SessionCompactFailedEvent, SessionEntry, SessionError, SessionRecord,
};

#[derive(Clone, Copy)]
enum OverflowKind {
    Context,
    Truncated,
}

impl OverflowKind {
    fn failure_message(self) -> &'static str {
        match self {
            Self::Context => {
                "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model."
            }
            Self::Truncated => {
                "Truncated response recovery failed after one compact-and-retry attempt."
            }
        }
    }
}

fn predates_compaction(message: &AssistantMessage, entries: &[SessionRecord]) -> bool {
    entries
        .iter()
        .rev()
        .find(|record| matches!(record.entry, SessionEntry::Compaction(_)))
        .is_some_and(|record| message.timestamp_ms <= record.timestamp_ms)
}

impl AgentSession {
    /// None means ordinary retry/threshold checks may run. Some(false) means an
    /// overflow was handled (or recovery failed); do not attempt another summary.
    pub(in crate::runtime) async fn maybe_overflow_compact_locked(
        &self,
        message: &AssistantMessage,
    ) -> Result<Option<bool>, SessionError> {
        let (provider, model) = self.runtime.agent().model_selection();
        let settings = self.resolved_compaction_settings_for(&provider, &model)?;
        if !settings.enabled || self.activity.is_aborted() {
            return Ok(None);
        }
        if message.provider != provider || message.model != model {
            return Ok(None);
        }
        let branch = self.store.branch_entries()?;
        if predates_compaction(message, &branch) {
            return Ok(None);
        }
        let projection = crate::project_session_entries(&branch, self.context_updates.options());
        let projected_source = projection
            .iter()
            .rev()
            .find(|entry| {
                entry.messages.iter().any(|candidate| {
                    matches!(candidate.as_standard(),
                Some(Message::Assistant(value)) if value.as_ref() == message)
                })
            })
            .map(|entry| &entry.source_entry.id);
        let source_index = projected_source
            .and_then(|id| branch.iter().position(|entry| entry.id == *id))
            .or_else(|| {
                branch.iter().rposition(|record| {
                    matches!(&record.entry,
                SessionEntry::Message(entry) if matches!(entry.message.as_standard(),
                    Some(Message::Assistant(value)) if value.as_ref() == message))
                })
            });
        let (projected, usage_matches, retained_for_explicit_recovery) = match source_index {
            None => (true, true, true),
            Some(index) => {
                let id = &branch[index].id;
                let after = &branch[index + 1..];
                let projected = projection.iter().any(|entry| {
                    entry.source_entry.id == *id
                        && entry.messages.iter().any(|message| {
                            matches!(message.as_standard(), Some(Message::Assistant(_)))
                        })
                });
                let latest_edit = after.iter().rev().find_map(|record| match &record.entry {
                    SessionEntry::ContextEdit(edit) if edit.target_id == *id => Some(edit),
                    _ => None,
                });
                (
                    projected,
                    projected
                        && !after
                            .iter()
                            .any(|record| matches!(record.entry, SessionEntry::ContextEdit(_))),
                    !after
                        .iter()
                        .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
                        && !latest_edit.is_some_and(|edit| edit.replacement.is_none()),
                )
            }
        };
        let explicit = super::is_context_overflow_error(message);
        let context_overflow = (explicit && retained_for_explicit_recovery)
            || (usage_matches
                && message.stop_reason != StopReason::Error
                && is_context_overflow(message, self.active_context_window()));
        let max_output = self
            .runtime
            .model(&provider, &model)
            .map_or(0, |model| model.max_tokens);
        let recoverable_length = projected && is_recoverable_length(message, max_output);
        if !context_overflow && !recoverable_length {
            return Ok(None);
        }
        let will_retry = message.stop_reason != StopReason::Stop;
        if will_retry {
            if !self.activity.start_overflow_recovery() {
                let kind = if context_overflow {
                    OverflowKind::Context
                } else {
                    OverflowKind::Truncated
                };
                let error_message = Some(kind.failure_message().to_owned());
                self.events.publish_compaction_end(
                    CompactionReason::Overflow,
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
                            reason: CompactionReason::Overflow,
                            error_message,
                            aborted: false,
                            will_retry: false,
                            from_extension: false,
                        },
                    )
                    .await;
                return Ok(Some(false));
            }
            self.context_updates.omit_failed_assistant(message)?;
        }
        let succeeded = self
            .run_compaction_locked(
                CompactionReason::Overflow,
                will_retry,
                None,
                CompactionActivation::Immediate,
            )
            .await
            .is_ok();
        Ok(Some(succeeded && will_retry && !self.activity.is_aborted()))
    }

    pub(in crate::runtime) async fn maybe_threshold_compact_locked(
        &self,
        include_aborted: bool,
    ) -> Result<(), SessionError> {
        if self.activity.is_aborted() {
            return Ok(());
        }
        let state = self.runtime.agent().state();
        let Some(message) = state
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(message) => Some(message.as_ref()),
                _ => None,
            })
        else {
            return Ok(());
        };
        let settings =
            self.resolved_compaction_settings_for(&state.provider_id, &state.model_id)?;
        if !settings.enabled || (!include_aborted && message.stop_reason == StopReason::Aborted) {
            return Ok(());
        }
        let Some(window) = self.active_context_window() else {
            return Ok(());
        };
        let entries = self.store.branch_entries()?;
        if predates_compaction(message, &entries) {
            return Ok(());
        }
        let (context, projection) = crate::history::build_session_context_and_entries(
            &entries,
            self.context_updates.options(),
        );
        let direct = calculate_context_tokens(&message.usage);
        let tokens = if projection
            .iter()
            .any(|entry| matches!(entry.source_entry.entry, SessionEntry::ContextEdit(_)))
        {
            estimate_projected_context_tokens(&entries, &context.messages, &projection).tokens
        } else if message.stop_reason == StopReason::Error || direct == 0 {
            let estimate = estimate_context_tokens(&context.messages);
            if estimate.last_usage_index.and_then(|index| context.messages.get(index))
                .and_then(|message| message.as_standard()).is_some_and(|message| {
                    matches!(message, Message::Assistant(message) if predates_compaction(message, &entries))
                }) { return Ok(()); }
            estimate.tokens
        } else {
            // Already measured by the provider. Unsent custom/boundary input is
            // not another completed request and must not override this checkpoint.
            direct
        };
        if should_compact(tokens, window, settings) {
            // A summary failure is best effort; configuration/history failures above are not.
            let _ = self
                .run_compaction_locked(
                    CompactionReason::Threshold,
                    false,
                    None,
                    CompactionActivation::Immediate,
                )
                .await;
        }
        Ok(())
    }

    pub(in crate::runtime) async fn compact_before_prompt(&self) -> Result<(), SessionError> {
        let state = self.runtime.agent().state();
        let last = state
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(message) => Some(message.as_ref()),
                _ => None,
            });
        if let Some(last) = last
            && self.maybe_overflow_compact_locked(last).await?.is_some()
        {
            // The new prompt will be sent by the caller, never continue the old turn here.
            return Ok(());
        }
        self.maybe_threshold_compact_locked(true).await
    }
}
