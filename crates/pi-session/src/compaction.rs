use std::collections::HashSet;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, ContentBlock, Message, StopReason, ThinkingLevel, Usage, UsageCost, UserMessage,
};
use pi_runtime::{PiRuntime, RuntimeCompletionRequest, RuntimeError};
use serde_json::{Value, json};

use crate::{
    AgentMessage, CompactionEntry, CompactionPreparation, CompactionSettings, FileOperations,
    SessionContextBuildOptions, SessionEntry, SessionRecord, agent_message_to_provider_message,
    build_session_context,
};

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = r#"You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary."#;

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix."#;

const ESTIMATED_IMAGE_CHARS: usize = 4_800;
const TOOL_RESULT_MAX_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: Option<usize>,
    pub is_split_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionCompletionRequest {
    pub system_prompt: String,
    pub prompt: String,
    pub max_output_tokens: u64,
    pub thinking_level: ThinkingLevel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionCompletion {
    pub text: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionModelCapabilities {
    pub reasoning: bool,
    pub max_output_tokens: Option<u64>,
}

impl Default for CompactionModelCapabilities {
    fn default() -> Self {
        Self {
            reasoning: true,
            max_output_tokens: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("compaction aborted: {0}")]
    Aborted(String),
    #[error("summarization failed: {0}")]
    SummarizationFailed(String),
}

#[async_trait]
pub trait CompactionCompleter: Send + Sync {
    fn model_capabilities(&self) -> CompactionModelCapabilities {
        CompactionModelCapabilities::default()
    }

    async fn complete_compaction(
        &self,
        request: CompactionCompletionRequest,
        signal: AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError>;
}

#[async_trait]
impl CompactionCompleter for PiRuntime {
    fn model_capabilities(&self) -> CompactionModelCapabilities {
        let state = self.agent().state();
        self.model(&state.provider_id, &state.model_id).map_or(
            CompactionModelCapabilities {
                reasoning: false,
                max_output_tokens: None,
            },
            |model| CompactionModelCapabilities {
                reasoning: model.reasoning,
                max_output_tokens: (model.max_tokens > 0).then_some(model.max_tokens),
            },
        )
    }

    async fn complete_compaction(
        &self,
        request: CompactionCompletionRequest,
        signal: AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError> {
        let response = self
            .complete(
                RuntimeCompletionRequest {
                    system_prompt: request.system_prompt,
                    messages: vec![Message::User(UserMessage::text(
                        request.prompt,
                        crate::now_ms(),
                    ))],
                    thinking_level: request.thinking_level,
                    thinking_budgets: self.agent().thinking_budgets(),
                    max_output_tokens: Some(request.max_output_tokens),
                },
                signal,
            )
            .await
            .map_err(|error| match error {
                RuntimeError::Aborted => {
                    CompactionError::Aborted("summarization aborted".to_string())
                }
                other => CompactionError::SummarizationFailed(other.to_string()),
            })?;
        match response.stop_reason {
            StopReason::Aborted => Err(CompactionError::Aborted(
                response
                    .error_message
                    .unwrap_or_else(|| "summarization aborted".to_string()),
            )),
            StopReason::Error => Err(CompactionError::SummarizationFailed(
                response
                    .error_message
                    .unwrap_or_else(|| "unknown provider error".to_string()),
            )),
            _ => Ok(CompactionCompletion {
                text: response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        ContentBlock::Thinking(_)
                        | ContentBlock::Image(_)
                        | ContentBlock::ToolCall(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                usage: response.usage,
            }),
        }
    }
}

pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    }
}

pub fn get_last_assistant_usage(entries: &[SessionRecord]) -> Option<Usage> {
    entries.iter().rev().find_map(|entry| match &entry.entry {
        SessionEntry::Message(message) => assistant_usage(&message.message).cloned(),
        _ => None,
    })
}

pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut usage_info = None;
    for (index, message) in messages.iter().enumerate() {
        if message_timestamp(message).is_some_and(|timestamp| timestamp >= latest_prefix_timestamp)
            && let Some(usage) = assistant_usage(message)
        {
            usage_info = Some((index, usage));
        }
        if let Some(timestamp) = message_timestamp(message) {
            latest_prefix_timestamp = latest_prefix_timestamp.max(timestamp);
        }
    }
    let Some((index, usage)) = usage_info else {
        let estimated = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };
    let usage_tokens = calculate_context_tokens(usage);
    let trailing_tokens = messages[index + 1..].iter().map(estimate_tokens).sum();
    ContextUsageEstimate {
        tokens: usage_tokens.saturating_add(trailing_tokens),
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// Estimates the active session context while rejecting usage blocks retained
/// from before the latest compaction. Those blocks describe the old, larger
/// context and would otherwise immediately retrigger compaction.
pub fn estimate_session_context_tokens(
    path_entries: &[SessionRecord],
    messages: &[AgentMessage],
) -> ContextUsageEstimate {
    let Some(compaction_index) = path_entries
        .iter()
        .rposition(|entry| matches!(entry.entry, SessionEntry::Compaction(_)))
    else {
        return estimate_context_tokens(messages);
    };
    if get_last_assistant_usage(&path_entries[compaction_index + 1..]).is_some() {
        return estimate_context_tokens(messages);
    }
    let estimated = messages.iter().map(estimate_tokens).sum();
    ContextUsageEstimate {
        tokens: estimated,
        usage_tokens: 0,
        trailing_tokens: estimated,
        last_usage_index: None,
    }
}

/// Returns the current context usage exposed to product frontends.
///
/// Immediately after compaction, retained assistant messages still carry
/// usage for the larger pre-compaction prefix. Pi reports the current usage as
/// unknown until a later successful assistant response establishes a new
/// checkpoint.
pub fn current_session_context_tokens(
    path_entries: &[SessionRecord],
    messages: &[AgentMessage],
) -> Option<ContextUsageEstimate> {
    let Some(compaction_index) = path_entries
        .iter()
        .rposition(|entry| matches!(entry.entry, SessionEntry::Compaction(_)))
    else {
        return Some(estimate_context_tokens(messages));
    };
    get_last_assistant_usage(&path_entries[compaction_index + 1..])
        .map(|_| estimate_context_tokens(messages))
}

pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: CompactionSettings,
) -> bool {
    settings.enabled && context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    let chars = match message.as_standard() {
        Some(Message::User(message)) => content_chars(&message.content),
        Some(Message::Assistant(message)) => message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => text.text.len(),
                ContentBlock::Thinking(thinking) => thinking.thinking.len(),
                ContentBlock::Image(_) => 0,
                ContentBlock::ToolCall(call) => call.name.len() + call.arguments.to_string().len(),
            })
            .sum(),
        Some(Message::ToolResult(message)) => content_chars(&message.content),
        Some(Message::Custom(message)) => match &message.content {
            pi_core::CustomMessageContent::Text(text) => text.len(),
            pi_core::CustomMessageContent::Blocks(content) => content_chars(content),
        },
        None => custom_message_chars(message),
    };
    u64::try_from(chars.saturating_add(3) / 4).unwrap_or(u64::MAX)
}

pub fn find_turn_start_index(
    entries: &[SessionRecord],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    (start_index..=entry_index).rev().find(|index| {
        let entry = &entries[*index].entry;
        matches!(entry, SessionEntry::BranchSummary(_))
            || matches!(entry, SessionEntry::Message(message) if matches!(message.message.role(), "user" | "bashExecution"))
    })
}

pub fn find_cut_point(
    entries: &[SessionRecord],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let compactable = entries[start_index..end_index]
        .iter()
        .map(CompactableEntry::from_record)
        .collect::<Vec<_>>();
    let result = find_cut_point_compactable(&compactable, 0, compactable.len(), keep_recent_tokens);
    CutPointResult {
        first_kept_entry_index: result.first_kept_entry_index + start_index,
        turn_start_index: result.turn_start_index.map(|index| index + start_index),
        is_split_turn: result.is_split_turn,
    }
}

pub fn prepare_compaction(
    path_entries: &[SessionRecord],
    settings: CompactionSettings,
    context_options: &SessionContextBuildOptions,
) -> Option<CompactionPreparation> {
    if path_entries.is_empty()
        || path_entries
            .last()
            .is_some_and(|entry| matches!(entry.entry, SessionEntry::Compaction(_)))
    {
        return None;
    }

    let previous_index = path_entries
        .iter()
        .rposition(|entry| matches!(entry.entry, SessionEntry::Compaction(_)));
    let mut previous_summary = None;
    let compactable = if let Some(index) = previous_index {
        let SessionEntry::Compaction(previous) = &path_entries[index].entry else {
            unreachable!()
        };
        previous_summary = Some(previous.summary.clone());
        previous
            .retained_tail
            .iter()
            .cloned()
            .map(CompactableEntry::Message)
            .chain(
                path_entries[index + 1..]
                    .iter()
                    .map(CompactableEntry::from_record),
            )
            .collect::<Vec<_>>()
    } else {
        path_entries
            .iter()
            .map(CompactableEntry::from_record)
            .collect::<Vec<_>>()
    };

    let context = build_session_context(path_entries, context_options);
    let tokens_before = estimate_session_context_tokens(path_entries, &context.messages).tokens;
    let cut = find_cut_point_compactable(
        &compactable,
        0,
        compactable.len(),
        settings.keep_recent_tokens,
    );
    let history_end = if cut.is_split_turn {
        cut.turn_start_index.unwrap_or(cut.first_kept_entry_index)
    } else {
        cut.first_kept_entry_index
    };
    let messages_to_summarize = messages_in(&compactable[..history_end]);
    let turn_prefix_messages = cut
        .turn_start_index
        .filter(|_| cut.is_split_turn)
        .map_or_else(Vec::new, |start| {
            messages_in(&compactable[start..cut.first_kept_entry_index])
        });
    let retained_tail = messages_in(&compactable[cut.first_kept_entry_index..]);

    let mut file_ops = FileOperations::default();
    if let Some(index) = previous_index
        && let SessionEntry::Compaction(previous) = &path_entries[index].entry
        && let Some(details) = &previous.details
    {
        extend_string_set(&mut file_ops.read, details.get("readFiles"));
        extend_string_set(&mut file_ops.edited, details.get("modifiedFiles"));
    }
    for message in messages_to_summarize.iter().chain(&turn_prefix_messages) {
        extract_file_operations(message, &mut file_ops);
    }

    Some(CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn: cut.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings,
    })
}

pub async fn compact(
    preparation: &CompactionPreparation,
    completer: &dyn CompactionCompleter,
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
                &preparation.turn_prefix_messages,
                preparation.settings.reserve_tokens,
                thinking_level,
                signal.child(),
            )
            .await?;
            let history_text = history
                .as_ref()
                .map_or("No prior history.", |value| value.text.as_str());
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
                &preparation.messages_to_summarize,
                preparation,
                custom_instructions,
                thinking_level,
                signal.child(),
            )
            .await?;
            (generated.text, generated.usage)
        };

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    Ok(CompactionEntry {
        summary,
        retained_tail: preparation.retained_tail.clone(),
        tokens_before: preparation.tokens_before,
        details: Some(json!({
            "readFiles": read_files,
            "modifiedFiles": modified_files,
        })),
        usage: Some(usage),
    })
}

pub fn serialize_conversation(messages: &[AgentMessage]) -> String {
    let mut parts = Vec::new();
    for agent_message in messages {
        let Some(message) = agent_message_to_provider_message(agent_message) else {
            continue;
        };
        match message {
            Message::User(message) => {
                let content = content_text(&message.content);
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(message) => {
                let mut thinking = Vec::new();
                let mut text = Vec::new();
                let mut calls = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Thinking(value) => thinking.push(value.thinking.clone()),
                        ContentBlock::Text(value) => text.push(value.text.clone()),
                        ContentBlock::ToolCall(call) => {
                            let args = call.arguments.as_object().map_or_else(
                                || call.arguments.to_string(),
                                |arguments| {
                                    arguments
                                        .iter()
                                        .map(|(key, value)| format!("{key}={value}"))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                },
                            );
                            calls.push(format!("{}({args})", call.name));
                        }
                        ContentBlock::Image(_) => {}
                    }
                }
                if !thinking.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking.join("\n")));
                }
                if !text.is_empty() {
                    parts.push(format!("[Assistant]: {}", text.join("\n")));
                }
                if !calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResult(message) => {
                let content = content_text(&message.content);
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::Custom(message) => {
                let content = content_text(&message.content.to_blocks());
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
        }
    }
    parts.join("\n\n")
}

async fn generate_summary(
    completer: &dyn CompactionCompleter,
    messages: &[AgentMessage],
    preparation: &CompactionPreparation,
    custom_instructions: Option<&str>,
    thinking_level: ThinkingLevel,
    signal: AbortSignal,
) -> Result<CompactionCompletion, CompactionError> {
    let mut base_prompt = if preparation.previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_string()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(instructions) = custom_instructions.filter(|value| !value.is_empty()) {
        base_prompt.push_str("\n\nAdditional focus: ");
        base_prompt.push_str(instructions);
    }
    let mut prompt = format!(
        "<conversation>\n{}\n</conversation>\n\n",
        serialize_conversation(messages)
    );
    if let Some(previous) = &preparation.previous_summary {
        prompt.push_str("<previous-summary>\n");
        prompt.push_str(previous);
        prompt.push_str("\n</previous-summary>\n\n");
    }
    prompt.push_str(&base_prompt);
    let capabilities = completer.model_capabilities();
    let max_output_tokens = clamp_summary_tokens(
        preparation.settings.reserve_tokens.saturating_mul(4) / 5,
        capabilities,
    );
    let thinking_level = compaction_thinking_level(thinking_level, capabilities);
    completer
        .complete_compaction(
            CompactionCompletionRequest {
                system_prompt: SUMMARIZATION_SYSTEM_PROMPT.to_string(),
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
    messages: &[AgentMessage],
    reserve_tokens: u64,
    thinking_level: ThinkingLevel,
    signal: AbortSignal,
) -> Result<CompactionCompletion, CompactionError> {
    let prompt = format!(
        "<conversation>\n{}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}",
        serialize_conversation(messages)
    );
    let capabilities = completer.model_capabilities();
    let max_output_tokens = clamp_summary_tokens(reserve_tokens / 2, capabilities);
    let thinking_level = compaction_thinking_level(thinking_level, capabilities);
    completer
        .complete_compaction(
            CompactionCompletionRequest {
                system_prompt: SUMMARIZATION_SYSTEM_PROMPT.to_string(),
                prompt,
                max_output_tokens,
                thinking_level,
            },
            signal,
        )
        .await
}

fn clamp_summary_tokens(requested: u64, capabilities: CompactionModelCapabilities) -> u64 {
    capabilities
        .max_output_tokens
        .map_or(requested, |maximum| requested.min(maximum))
}

fn compaction_thinking_level(
    requested: ThinkingLevel,
    capabilities: CompactionModelCapabilities,
) -> ThinkingLevel {
    if capabilities.reasoning {
        requested
    } else {
        ThinkingLevel::Off
    }
}

#[derive(Debug, Clone)]
enum CompactableEntry {
    Message(AgentMessage),
    BranchSummary(AgentMessage),
    Compaction,
    Metadata,
}

impl CompactableEntry {
    fn from_record(record: &SessionRecord) -> Self {
        match &record.entry {
            SessionEntry::Message(message) => Self::Message(message.message.clone()),
            SessionEntry::CustomMessage(message) => {
                Self::Message(AgentMessage::from(message.to_message(record.timestamp_ms)))
            }
            SessionEntry::BranchSummary(summary) => Self::BranchSummary(
                AgentMessage::custom(json!({
                    "role": "branchSummary",
                    "summary": summary.summary,
                    "fromId": summary.from_id,
                    "timestamp": record.timestamp_ms,
                }))
                .expect("built-in branch summary is valid"),
            ),
            SessionEntry::Compaction(_) => Self::Compaction,
            SessionEntry::ModelChange(_)
            | SessionEntry::ThinkingLevelChange(_)
            | SessionEntry::ActiveToolsChange(_)
            | SessionEntry::Custom(_) => Self::Metadata,
        }
    }

    fn message(&self) -> Option<&AgentMessage> {
        match self {
            Self::Message(message) | Self::BranchSummary(message) => Some(message),
            Self::Compaction | Self::Metadata => None,
        }
    }

    fn is_valid_cut(&self) -> bool {
        match self {
            Self::BranchSummary(_) => true,
            Self::Message(message) => matches!(
                message.role(),
                "user"
                    | "assistant"
                    | "bashExecution"
                    | "custom"
                    | "branchSummary"
                    | "compactionSummary"
            ),
            Self::Compaction | Self::Metadata => false,
        }
    }

    fn starts_turn(&self) -> bool {
        matches!(self, Self::BranchSummary(_))
            || matches!(self, Self::Message(message) if matches!(message.role(), "user" | "bashExecution"))
    }
}

fn find_cut_point_compactable(
    entries: &[CompactableEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let cut_points = (start_index..end_index)
        .filter(|index| entries[*index].is_valid_cut())
        .collect::<Vec<_>>();
    let Some(&first_cut) = cut_points.first() else {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    };
    let mut accumulated = 0_u64;
    let mut cut_index = first_cut;
    for index in (start_index..end_index).rev() {
        let Some(message) = entries[index].message() else {
            continue;
        };
        accumulated = accumulated.saturating_add(estimate_tokens(message));
        if accumulated >= keep_recent_tokens {
            cut_index = cut_points
                .iter()
                .copied()
                .find(|point| *point >= index)
                .unwrap_or(first_cut);
            break;
        }
    }
    while cut_index > start_index {
        match entries[cut_index - 1] {
            CompactableEntry::Compaction | CompactableEntry::Message(_) => break,
            CompactableEntry::BranchSummary(_) | CompactableEntry::Metadata => cut_index -= 1,
        }
    }
    let is_user = matches!(&entries[cut_index], CompactableEntry::Message(message) if message.role() == "user");
    let turn_start_index = (!is_user)
        .then(|| {
            (start_index..=cut_index)
                .rev()
                .find(|index| entries[*index].starts_turn())
        })
        .flatten();
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !is_user && turn_start_index.is_some(),
    }
}

fn messages_in(entries: &[CompactableEntry]) -> Vec<AgentMessage> {
    entries
        .iter()
        .filter_map(|entry| entry.message().cloned())
        .collect()
}

fn assistant_usage(message: &AgentMessage) -> Option<&Usage> {
    let Some(Message::Assistant(message)) = message.as_standard() else {
        return None;
    };
    (!matches!(message.stop_reason, StopReason::Aborted | StopReason::Error)
        && calculate_context_tokens(&message.usage) > 0)
        .then_some(&message.usage)
}

fn message_timestamp(message: &AgentMessage) -> Option<i64> {
    match message.as_standard() {
        Some(Message::User(message)) => Some(message.timestamp_ms),
        Some(Message::Assistant(message)) => Some(message.timestamp_ms),
        Some(Message::ToolResult(message)) => Some(message.timestamp_ms),
        Some(Message::Custom(message)) => Some(message.timestamp_ms),
        None => message
            .as_custom()
            .and_then(|value| value.get("timestamp"))
            .and_then(|timestamp| {
                timestamp.as_i64().or_else(|| {
                    timestamp
                        .as_u64()
                        .and_then(|timestamp| i64::try_from(timestamp).ok())
                })
            }),
    }
}

fn content_chars(content: &[ContentBlock]) -> usize {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.len(),
            ContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => 0,
        })
        .sum()
}

fn custom_message_chars(message: &AgentMessage) -> usize {
    let Some(value) = message.as_custom() else {
        return 0;
    };
    match message.role() {
        "custom" => value.get("content").map_or(0, json_content_chars),
        "bashExecution" => {
            value
                .get("command")
                .and_then(Value::as_str)
                .map_or(0, str::len)
                + value
                    .get("output")
                    .and_then(Value::as_str)
                    .map_or(0, str::len)
        }
        "branchSummary" | "compactionSummary" => value
            .get("summary")
            .and_then(Value::as_str)
            .map_or(0, str::len),
        _ => 0,
    }
}

fn json_content_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map_or(0, str::len),
                Some("image") => ESTIMATED_IMAGE_CHARS,
                _ => 0,
            })
            .sum(),
        _ => 0,
    }
}

fn extract_file_operations(message: &AgentMessage, file_ops: &mut FileOperations) {
    let Some(Message::Assistant(message)) = message.as_standard() else {
        return;
    };
    for call in message.tool_calls() {
        let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
            continue;
        };
        match call.name.as_str() {
            "read" => {
                file_ops.read.insert(path.to_string());
            }
            "write" => {
                file_ops.written.insert(path.to_string());
            }
            "edit" => {
                file_ops.edited.insert(path.to_string());
            }
            _ => {}
        }
    }
}

fn extend_string_set(target: &mut HashSet<String>, value: Option<&Value>) {
    if let Some(Value::Array(values)) = value {
        target.extend(values.iter().filter_map(Value::as_str).map(str::to_string));
    }
}

fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified = file_ops
        .written
        .union(&file_ops.edited)
        .cloned()
        .collect::<HashSet<_>>();
    let mut read_files = file_ops
        .read
        .difference(&modified)
        .cloned()
        .collect::<Vec<_>>();
    let mut modified_files = modified.into_iter().collect::<Vec<_>>();
    read_files.sort();
    modified_files.sort();
    (read_files, modified_files)
}

fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            ContentBlock::Thinking(thinking) => Some(thinking.thinking.as_str()),
            ContentBlock::Image(_) | ContentBlock::ToolCall(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!(
        "{prefix}\n\n[... {} more characters truncated]",
        char_count - max_chars
    )
}

fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input.saturating_add(second.input),
        output: first.output.saturating_add(second.output),
        cache_read: first.cache_read.saturating_add(second.cache_read),
        cache_write: first.cache_write.saturating_add(second.cache_write),
        cache_write_1h: combine_optional(first.cache_write_1h, second.cache_write_1h),
        reasoning: combine_optional(first.reasoning, second.reasoning),
        total_tokens: first.total_tokens.saturating_add(second.total_tokens),
        cost: UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
        },
    }
}

fn combine_optional(first: Option<u64>, second: Option<u64>) -> Option<u64> {
    (first.is_some() || second.is_some())
        .then(|| first.unwrap_or(0).saturating_add(second.unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pi_core::{
        AssistantMessage, ModelId, ProviderId, TextContent, ToolCall, ToolCallId, ToolResultMessage,
    };

    use super::*;
    use crate::{MessageEntry, SessionEntry};

    fn usage(total_tokens: u64) -> Usage {
        Usage {
            input: total_tokens,
            total_tokens,
            ..Usage::default()
        }
    }

    fn user(text: &str) -> AgentMessage {
        Message::User(UserMessage::text(text, 0)).into()
    }

    fn assistant(text: &str, total_tokens: u64) -> AgentMessage {
        assistant_at(text, total_tokens, 0)
    }

    fn assistant_at(text: &str, total_tokens: u64, timestamp_ms: i64) -> AgentMessage {
        Message::assistant(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            api: "scripted".to_string(),
            provider: ProviderId::new("scripted"),
            model: ModelId::new("test"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(total_tokens),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms,
        })
        .into()
    }

    fn record(index: usize, entry: SessionEntry) -> SessionRecord {
        SessionRecord {
            id: format!("entry-{index}"),
            seq: index as u64,
            parent_id: index.checked_sub(1).map(|value| format!("entry-{value}")),
            timestamp_ms: index as i64,
            entry,
        }
    }

    fn message_record(index: usize, message: AgentMessage) -> SessionRecord {
        record(
            index,
            SessionEntry::Message(MessageEntry {
                message,
                terminate: false,
            }),
        )
    }

    #[test]
    fn estimate_uses_last_valid_usage_plus_trailing_messages() {
        let mut failed = assistant("failed", 9_999);
        if let Some(Message::Assistant(message)) = failed.as_standard() {
            let mut value = (**message).clone();
            value.stop_reason = StopReason::Error;
            failed = Message::assistant(value).into();
        }
        let messages = vec![assistant("done", 100), user("tail"), failed];
        let estimate = estimate_context_tokens(&messages);
        assert_eq!(estimate.usage_tokens, 100);
        assert_eq!(estimate.last_usage_index, Some(0));
        assert!(estimate.trailing_tokens > 0);
        assert_eq!(estimate.tokens, 100 + estimate.trailing_tokens);
    }

    #[test]
    fn estimate_ignores_usage_made_stale_by_a_newer_inserted_prefix() {
        let messages = vec![
            AgentMessage::custom(json!({
                "role": "compactionSummary",
                "summary": "short summary",
                "tokensBefore": 9_500,
                "timestamp": 200,
            }))
            .unwrap(),
            assistant_at("retained answer", 9_500, 100),
            Message::User(UserMessage::text("x".repeat(4_000), 300)).into(),
        ];

        let estimate = estimate_context_tokens(&messages);

        assert_eq!(estimate.last_usage_index, None);
        assert_eq!(estimate.usage_tokens, 0);
        assert_eq!(estimate.tokens, estimate.trailing_tokens);
        assert!(estimate.tokens < 2_000);
    }

    #[test]
    fn token_totals_and_thresholds_match_pi_boundaries() {
        assert_eq!(calculate_context_tokens(&usage(42)), 42);
        assert_eq!(
            calculate_context_tokens(&Usage {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
                total_tokens: 0,
                ..Usage::default()
            }),
            100
        );
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 200,
            keep_recent_tokens: 50,
        };
        assert!(!should_compact(800, 1_000, settings));
        assert!(should_compact(801, 1_000, settings));
        assert!(!should_compact(
            u64::MAX,
            1_000,
            CompactionSettings {
                enabled: false,
                ..settings
            }
        ));
        assert!(should_compact(
            1,
            100,
            CompactionSettings {
                reserve_tokens: 200,
                ..settings
            }
        ));
    }

    #[test]
    fn cut_point_never_starts_at_a_tool_result() {
        let entries = vec![
            message_record(0, user("request")),
            message_record(1, assistant("call", 10)),
            message_record(
                2,
                Message::tool_result(ToolResultMessage {
                    tool_call_id: ToolCallId::new("call-1"),
                    tool_name: "read".to_string(),
                    content: vec![ContentBlock::Text(TextContent::new("result"))],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: 0,
                })
                .into(),
            ),
            message_record(3, assistant("finish", 20)),
        ];
        let cut = find_cut_point(&entries, 0, entries.len(), 1);
        assert!(
            matches!(entries[cut.first_kept_entry_index].entry, SessionEntry::Message(ref value) if value.message.role() == "assistant")
        );
    }

    #[test]
    fn preparation_carries_previous_summary_and_retained_tail() {
        let previous = record(
            0,
            SessionEntry::Compaction(CompactionEntry {
                summary: "previous".to_string(),
                retained_tail: vec![user("retained user"), assistant("retained assistant", 50)],
                tokens_before: 1_000,
                details: None,
                usage: None,
            }),
        );
        let entries = vec![
            previous,
            message_record(1, user("new user")),
            message_record(2, assistant("new assistant", 100)),
        ];
        let preparation = prepare_compaction(
            &entries,
            CompactionSettings {
                keep_recent_tokens: 1,
                ..CompactionSettings::default()
            },
            &SessionContextBuildOptions::default(),
        )
        .unwrap();
        assert_eq!(preparation.previous_summary.as_deref(), Some("previous"));
        let all = preparation
            .messages_to_summarize
            .iter()
            .chain(&preparation.turn_prefix_messages)
            .chain(&preparation.retained_tail)
            .map(AgentMessage::role)
            .collect::<Vec<_>>();
        assert_eq!(all, vec!["user", "assistant", "user", "assistant"]);
    }

    #[test]
    fn preparation_requires_compactable_history_after_the_latest_compaction() {
        assert!(
            prepare_compaction(
                &[],
                CompactionSettings::default(),
                &SessionContextBuildOptions::default()
            )
            .is_none()
        );
        let latest = record(
            0,
            SessionEntry::Compaction(CompactionEntry {
                summary: "done".to_string(),
                retained_tail: Vec::new(),
                tokens_before: 10,
                details: None,
                usage: None,
            }),
        );
        assert!(
            prepare_compaction(
                &[latest],
                CompactionSettings::default(),
                &SessionContextBuildOptions::default()
            )
            .is_none()
        );
    }

    #[test]
    fn retained_pre_compaction_usage_is_not_treated_as_current_usage() {
        let retained = assistant("small retained answer", 100_000);
        let entries = vec![record(
            0,
            SessionEntry::Compaction(CompactionEntry {
                summary: "short summary".to_string(),
                retained_tail: vec![retained],
                tokens_before: 100_000,
                details: None,
                usage: None,
            }),
        )];
        let context = build_session_context(&entries, &SessionContextBuildOptions::default());
        let estimate = estimate_session_context_tokens(&entries, &context.messages);
        assert_eq!(estimate.usage_tokens, 0);
        assert!(estimate.tokens < 100_000);
        assert_eq!(
            current_session_context_tokens(&entries, &context.messages),
            None
        );
    }

    #[test]
    fn post_compaction_assistant_usage_restores_current_context_accounting() {
        let entries = vec![
            record(
                0,
                SessionEntry::Compaction(CompactionEntry {
                    summary: "short summary".to_string(),
                    retained_tail: vec![assistant_at("retained", 100_000, 1)],
                    tokens_before: 100_000,
                    details: None,
                    usage: None,
                }),
            ),
            message_record(1, Message::User(UserMessage::text("continue", 3)).into()),
            message_record(2, assistant_at("fresh", 250, 4)),
        ];
        let context = build_session_context(&entries, &SessionContextBuildOptions::default());

        let estimate = current_session_context_tokens(&entries, &context.messages).unwrap();

        assert_eq!(estimate.tokens, 250);
        assert_eq!(estimate.usage_tokens, 250);
    }

    #[test]
    fn serialize_truncates_tool_results() {
        let message: AgentMessage = Message::tool_result(ToolResultMessage {
            tool_call_id: ToolCallId::new("call-1"),
            tool_name: "read".to_string(),
            content: vec![ContentBlock::Text(TextContent::new("x".repeat(5_000)))],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 0,
        })
        .into();
        let serialized = serialize_conversation(&[message]);
        assert!(serialized.contains("[... 3000 more characters truncated]"));
    }

    struct FakeCompleter {
        prompts: Mutex<Vec<CompactionCompletionRequest>>,
        capabilities: CompactionModelCapabilities,
    }

    #[tokio::test]
    async fn summary_request_carries_previous_summary_custom_focus_and_reasoning() {
        let preparation = CompactionPreparation {
            messages_to_summarize: vec![user("new work")],
            turn_prefix_messages: Vec::new(),
            retained_tail: vec![assistant("tail", 10)],
            is_split_turn: false,
            tokens_before: 500,
            previous_summary: Some("old summary".to_string()),
            file_ops: FileOperations::default(),
            settings: CompactionSettings {
                reserve_tokens: 100,
                keep_recent_tokens: 20,
                enabled: true,
            },
        };
        let completer = FakeCompleter {
            prompts: Mutex::new(Vec::new()),
            capabilities: CompactionModelCapabilities::default(),
        };
        let (_, signal) = pi_core::AbortHandle::new();

        let result = compact(
            &preparation,
            &completer,
            Some("preserve exact errors"),
            ThinkingLevel::High,
            signal,
        )
        .await
        .unwrap();
        let requests = completer.prompts.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].thinking_level, ThinkingLevel::High);
        assert_eq!(requests[0].max_output_tokens, 80);
        assert_eq!(requests[0].system_prompt, SUMMARIZATION_SYSTEM_PROMPT);
        assert!(
            requests[0]
                .prompt
                .contains("<previous-summary>\nold summary")
        );
        assert!(
            requests[0]
                .prompt
                .contains("Additional focus: preserve exact errors")
        );
        assert_eq!(result.summary, "## Goal\nKeep working");
        assert_eq!(result.retained_tail, preparation.retained_tail);
    }

    struct FailingCompleter {
        aborted: bool,
    }

    #[async_trait]
    impl CompactionCompleter for FailingCompleter {
        async fn complete_compaction(
            &self,
            _request: CompactionCompletionRequest,
            _signal: AbortSignal,
        ) -> Result<CompactionCompletion, CompactionError> {
            if self.aborted {
                Err(CompactionError::Aborted("cancelled fixture".to_string()))
            } else {
                Err(CompactionError::SummarizationFailed(
                    "failed fixture".to_string(),
                ))
            }
        }
    }

    #[tokio::test]
    async fn summary_failures_and_aborts_are_returned_without_partial_entries() {
        let preparation = CompactionPreparation {
            messages_to_summarize: vec![user("work")],
            turn_prefix_messages: Vec::new(),
            retained_tail: Vec::new(),
            is_split_turn: false,
            tokens_before: 10,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionSettings::default(),
        };
        for (aborted, expected) in [(false, "failed fixture"), (true, "cancelled fixture")] {
            let (_, signal) = pi_core::AbortHandle::new();
            let error = compact(
                &preparation,
                &FailingCompleter { aborted },
                None,
                ThinkingLevel::Off,
                signal,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(expected));
            assert_eq!(matches!(error, CompactionError::Aborted(_)), aborted);
        }
    }

    #[async_trait]
    impl CompactionCompleter for FakeCompleter {
        fn model_capabilities(&self) -> CompactionModelCapabilities {
            self.capabilities
        }

        async fn complete_compaction(
            &self,
            request: CompactionCompletionRequest,
            _signal: AbortSignal,
        ) -> Result<CompactionCompletion, CompactionError> {
            self.prompts.lock().unwrap().push(request);
            Ok(CompactionCompletion {
                text: "## Goal\nKeep working".to_string(),
                usage: usage(25),
            })
        }
    }

    #[tokio::test]
    async fn summaries_respect_model_reasoning_and_output_capabilities() {
        let preparation = CompactionPreparation {
            messages_to_summarize: vec![user("history")],
            turn_prefix_messages: vec![user("large turn")],
            retained_tail: vec![assistant("tail", 10)],
            is_split_turn: true,
            tokens_before: 600_000,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionSettings {
                reserve_tokens: 500_000,
                keep_recent_tokens: 20_000,
                enabled: true,
            },
        };
        let completer = FakeCompleter {
            prompts: Mutex::new(Vec::new()),
            capabilities: CompactionModelCapabilities {
                reasoning: false,
                max_output_tokens: Some(128_000),
            },
        };
        let (_, signal) = pi_core::AbortHandle::new();

        compact(&preparation, &completer, None, ThinkingLevel::High, signal)
            .await
            .unwrap();

        let requests = completer.prompts.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.max_output_tokens == 128_000)
        );
        assert!(
            requests
                .iter()
                .all(|request| request.thinking_level == ThinkingLevel::Off)
        );
    }

    #[tokio::test]
    async fn split_turn_uses_two_summaries_and_preserves_file_details() {
        let mut calling = assistant("", 50);
        if let Some(Message::Assistant(message)) = calling.as_standard() {
            let mut value = (**message).clone();
            value.content = vec![ContentBlock::ToolCall(ToolCall::new(
                "call-1",
                "edit",
                json!({"path":"src/lib.rs"}),
            ))];
            calling = Message::assistant(value).into();
        }
        let preparation = CompactionPreparation {
            messages_to_summarize: vec![user("old"), calling],
            turn_prefix_messages: vec![user("large turn prefix")],
            retained_tail: vec![assistant("suffix", 50)],
            is_split_turn: true,
            tokens_before: 500,
            previous_summary: None,
            file_ops: {
                let mut operations = FileOperations::default();
                extract_file_operations(&preparation_message_for_edit(), &mut operations);
                operations
            },
            settings: CompactionSettings::default(),
        };
        let completer = Arc::new(FakeCompleter {
            prompts: Mutex::new(Vec::new()),
            capabilities: CompactionModelCapabilities::default(),
        });
        let (_, signal) = pi_core::AbortHandle::new();
        let result = compact(
            &preparation,
            completer.as_ref(),
            None,
            ThinkingLevel::Off,
            signal,
        )
        .await
        .unwrap();
        assert_eq!(completer.prompts.lock().unwrap().len(), 2);
        assert!(result.summary.contains("Turn Context (split turn)"));
        assert!(result.summary.contains("<modified-files>\nsrc/lib.rs"));
        assert_eq!(result.usage.unwrap().total_tokens, 50);
    }

    fn preparation_message_for_edit() -> AgentMessage {
        let mut message = assistant("", 1);
        if let Some(Message::Assistant(assistant)) = message.as_standard() {
            let mut value = (**assistant).clone();
            value.content = vec![ContentBlock::ToolCall(ToolCall::new(
                "call",
                "edit",
                json!({"path":"src/lib.rs"}),
            ))];
            message = Message::assistant(value).into();
        }
        message
    }
}
