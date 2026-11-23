//! Session-history adaptation and summary execution through an injected completer.
//!
//! Projection and normalization happen here; the algorithm receives plain values.
//! Returned checkpoint values are persisted by the owning AgentSession operation.

use pi_core::session::{
    AgentMessage, CompactionEntry, CompactionPreparation, CompactionSettings,
    ProjectedSessionEntry, SessionEntry, SessionRecord,
};
use pi_core::{AbortSignal, Message, ThinkingLevel};

use super::algorithm::{
    CompactionCompleter, CompactionCompletion, CompactionCompletionRequest, CompactionError,
    ContextUsageEstimate, SessionCompactionPolicy, combine_usage, compaction_thinking_level,
    estimate_context_tokens, estimate_tokens, format_conversation, prepare_projected_compaction,
    summary_prompt, turn_prefix_prompt,
};
use crate::{SessionContextBuildOptions, agent_message_to_provider_message};

pub fn prepare_compaction(
    path_entries: &[SessionRecord],
    settings: CompactionSettings,
    context_options: &SessionContextBuildOptions,
    policy: &dyn SessionCompactionPolicy,
) -> Option<CompactionPreparation> {
    if path_entries.is_empty()
        || path_entries
            .last()
            .is_some_and(|entry| matches!(entry.entry, SessionEntry::Compaction(_)))
    {
        return None;
    }

    let projected = crate::project_session_entries(path_entries, context_options);
    let messages = projected
        .iter()
        .flat_map(|entry| entry.messages.iter().cloned())
        .collect::<Vec<_>>();
    let tokens_before =
        estimate_projected_context_tokens(path_entries, &messages, &projected).tokens;
    let mut preparation = prepare_projected_compaction(projected, settings, tokens_before)?;
    policy.prepare(path_entries, &mut preparation);
    Some(preparation)
}

pub async fn compact(
    preparation: &CompactionPreparation,
    completer: &dyn CompactionCompleter,
    policy: &dyn SessionCompactionPolicy,
    custom_instructions: Option<&str>,
    thinking_level: ThinkingLevel,
    signal: AbortSignal,
) -> Result<CompactionEntry, CompactionError> {
    let (mut summary, usage) =
        if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
            let history = if preparation.messages_to_summarize.is_empty() {
                None
            } else {
                Some(
                    generate_summary(
                        completer,
                        policy,
                        &preparation.messages_to_summarize,
                        preparation,
                        custom_instructions,
                        thinking_level,
                        signal.child(),
                    )
                    .await?,
                )
            };
            let prefix = generate_turn_prefix_summary(
                completer,
                policy,
                &preparation.turn_prefix_messages,
                custom_instructions,
                thinking_level,
                signal.child(),
            )
            .await?;
            let history_text = history.as_ref().map_or_else(
                || {
                    preparation
                        .previous_summary
                        .as_deref()
                        .unwrap_or("No prior history.")
                },
                |value| value.text.as_str(),
            );
            let usage = history.as_ref().map_or(prefix.usage.clone(), |value| {
                combine_usage(&value.usage, &prefix.usage)
            });
            (
                format!(
                    "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
                    prefix.text
                ),
                usage,
            )
        } else {
            let generated = generate_summary(
                completer,
                policy,
                &preparation.messages_to_summarize,
                preparation,
                custom_instructions,
                thinking_level,
                signal.child(),
            )
            .await?;
            (generated.text, generated.usage)
        };

    let details = policy.finalize(preparation, &mut summary);
    Ok(CompactionEntry {
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        from_hook: None,
        system_message: None,
        retained_entry_ids: Vec::new(),
        summary,
        retained_tail: preparation.retained_tail.clone(),
        tokens_before: preparation.tokens_before,
        details,
        usage: Some(usage),
    })
}

/// Estimates projected context without trusting usage from before a later edit,
/// compaction, or isolated seed. Source identity, not message timestamps, defines
/// whether an assistant checkpoint described the current transcript.
pub fn estimate_session_context_tokens(
    path_entries: &[SessionRecord],
    messages: &[AgentMessage],
) -> ContextUsageEstimate {
    let projected = crate::project_session_entries(path_entries, &Default::default());
    estimate_projected_context_tokens(path_entries, messages, &projected)
}

pub(crate) fn estimate_projected_context_tokens(
    path_entries: &[SessionRecord],
    messages: &[AgentMessage],
    projected: &[ProjectedSessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(messages);
    if usage_is_current(path_entries, messages, projected, &estimate) {
        return estimate;
    }
    // Pi counts the effective system prompt once, not each historical delta.
    let system_tokens = crate::history::current_system_snapshot(messages)
        .ok()
        .flatten()
        .map_or(0, |snapshot| estimate_tokens(snapshot.as_message()));
    let tokens = system_tokens.saturating_add(
        messages
            .iter()
            .filter(|message| message.role() != "system")
            .map(estimate_tokens)
            .sum(),
    );
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

fn latest_context_invalidation(path_entries: &[SessionRecord]) -> Option<usize> {
    path_entries.iter().rposition(|entry| {
        matches!(
            entry.entry,
            SessionEntry::Compaction(_) | SessionEntry::ContextEdit(_)
        ) || crate::history::is_inherited_context_entry(&entry.entry)
    })
}

fn usage_is_current(
    path_entries: &[SessionRecord],
    messages: &[AgentMessage],
    projected: &[ProjectedSessionEntry],
    estimate: &ContextUsageEstimate,
) -> bool {
    let Some(message) = estimate
        .last_usage_index
        .and_then(|index| messages.get(index))
    else {
        return false;
    };
    let Some(invalidation) = latest_context_invalidation(path_entries) else {
        return true;
    };
    projected
        .iter()
        .rev()
        .find(|entry| entry.messages.iter().any(|candidate| candidate == message))
        .and_then(|entry| {
            path_entries
                .iter()
                .position(|source| source.id == entry.source_entry.id)
        })
        .is_some_and(|index| index > invalidation)
}

/// Product usage remains unknown after compaction/edit until a visible assistant
/// establishes a checkpoint against the new context.
pub fn current_session_context_tokens(
    path_entries: &[SessionRecord],
    messages: &[AgentMessage],
) -> Option<ContextUsageEstimate> {
    let projected = crate::project_session_entries(path_entries, &Default::default());
    let estimate = estimate_context_tokens(messages);
    if latest_context_invalidation(path_entries).is_some()
        && !usage_is_current(path_entries, messages, &projected, &estimate)
    {
        return None;
    }
    Some(estimate_projected_context_tokens(
        path_entries,
        messages,
        &projected,
    ))
}

pub fn serialize_conversation(messages: &[AgentMessage]) -> String {
    format_conversation(normalized_messages(messages))
}

fn normalized_messages(messages: &[AgentMessage]) -> impl Iterator<Item = Message> + '_ {
    messages
        .iter()
        .filter_map(agent_message_to_provider_message)
}

async fn generate_summary(
    completer: &dyn CompactionCompleter,
    policy: &dyn SessionCompactionPolicy,
    messages: &[AgentMessage],
    preparation: &CompactionPreparation,
    custom_instructions: Option<&str>,
    thinking_level: ThinkingLevel,
    signal: AbortSignal,
) -> Result<CompactionCompletion, CompactionError> {
    let prompt = summary_prompt(
        normalized_messages(messages),
        preparation.previous_summary.as_deref(),
        policy,
        custom_instructions,
    );
    let capabilities = completer.model_capabilities();
    let max_output_tokens = None;
    let thinking_level = compaction_thinking_level(thinking_level, capabilities);
    completer
        .complete_compaction(
            CompactionCompletionRequest {
                system_prompt: policy.system_prompt().to_string(),
                prompt,
                max_output_tokens,
                thinking_level,
            },
            signal,
        )
        .await
}

async fn generate_turn_prefix_summary(
    completer: &dyn CompactionCompleter,
    policy: &dyn SessionCompactionPolicy,
    messages: &[AgentMessage],
    custom_instructions: Option<&str>,
    thinking_level: ThinkingLevel,
    signal: AbortSignal,
) -> Result<CompactionCompletion, CompactionError> {
    let prompt = turn_prefix_prompt(normalized_messages(messages), policy, custom_instructions);
    let capabilities = completer.model_capabilities();
    let max_output_tokens = None;
    let thinking_level = compaction_thinking_level(thinking_level, capabilities);
    completer
        .complete_compaction(
            CompactionCompletionRequest {
                system_prompt: policy.system_prompt().to_string(),
                prompt,
                max_output_tokens,
                thinking_level,
            },
            signal,
        )
        .await
}
