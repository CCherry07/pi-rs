//! Deterministic compaction planning, summary formatting and execution contracts.
//!
//! Callers supply projected entries and normalized messages. History projection,
//! provider execution and checkpoint persistence belong to the adapters/caller.

use async_trait::async_trait;
use pi_core::session::{
    AgentMessage, CompactionPreparation, CompactionSettings, FileOperations, ProjectedSessionEntry,
    SessionEntry, SessionRecord,
};
use pi_core::{AbortSignal, ContentBlock, Message, StopReason, ThinkingLevel, Usage, UsageCost};
use serde_json::Value;

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

Keep each section concise. Preserve exact identifiers, important references, and error messages."#;

const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact identifiers, important references, and error messages
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

Keep each section concise. Preserve exact identifiers, important references, and error messages."#;

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

const BRANCH_SUMMARIZATION_PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.\n\nUse this EXACT format:\n\n## Goal\n[What was the user trying to accomplish in this branch?]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Work that was started but not finished]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [What should happen next to continue this work]\n\nKeep each section concise. Preserve exact identifiers, important references, and error messages.";

/// Immutable summary policy selected while building a session generation.
///
/// The session owns cut points, model calls, cancellation and checkpoint writes.
/// A domain can supply prompts and enrich preparation/result metadata without
/// adding another plugin lifecycle or changing the persisted session schema.
pub trait SessionCompactionPolicy: Send + Sync {
    fn system_prompt(&self) -> &str {
        SUMMARIZATION_SYSTEM_PROMPT
    }

    fn summary_prompt(&self, updating: bool) -> &str {
        if updating {
            UPDATE_SUMMARIZATION_PROMPT
        } else {
            SUMMARIZATION_PROMPT
        }
    }

    fn turn_prefix_prompt(&self) -> &str {
        TURN_PREFIX_SUMMARIZATION_PROMPT
    }

    fn branch_prompt(&self) -> &str {
        BRANCH_SUMMARIZATION_PROMPT
    }

    /// Enriches the prepared input before session lifecycle hooks see it.
    fn prepare(&self, _path_entries: &[SessionRecord], _preparation: &mut CompactionPreparation) {}

    /// Enriches an abandoned branch before summarization. Domains can carry
    /// branch-specific metadata without changing the generic session rules.
    fn prepare_branch(
        &self,
        path_entries: &[SessionRecord],
        preparation: &mut CompactionPreparation,
    ) {
        self.prepare(path_entries, preparation);
    }

    /// Appends domain summary context and returns optional checkpoint details.
    fn finalize(
        &self,
        _preparation: &CompactionPreparation,
        _summary: &mut String,
    ) -> Option<Value> {
        None
    }
}

/// General-purpose summaries with no assumptions about tools or domain data.
#[derive(Debug, Default)]
pub struct GenericCompactionPolicy;

impl SessionCompactionPolicy for GenericCompactionPolicy {}

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
struct CutPointResult {
    first_kept_entry_index: usize,
    turn_start_index: Option<usize>,
    is_split_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionCompletionRequest {
    pub system_prompt: String,
    pub prompt: String,
    /// Leave unset to use the provider/model output limit, independently of
    /// the context space reserved to trigger compaction.
    pub max_output_tokens: Option<u64>,
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

pub(crate) fn summarization_failure(
    response: &pi_core::AssistantMessage,
    label: &str,
) -> Option<String> {
    match response.stop_reason {
        StopReason::Error => Some(format!(
            "{label} failed: {}",
            response
                .error_message
                .as_deref()
                .filter(|message| !message.is_empty())
                .unwrap_or("Unknown error")
        )),
        StopReason::Length => Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        )),
        _ if response
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall(_))) =>
        {
            Some(format!("{label} attempted to call a tool"))
        }
        _ => None,
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

pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let usage_info = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| assistant_usage(message).map(|usage| (index, usage)));
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

pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: CompactionSettings,
) -> bool {
    settings.enabled
        && (settings.reserve_tokens > context_window
            || context_tokens > context_window - settings.reserve_tokens)
}

pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    let chars = match message.as_standard() {
        Some(Message::System(system)) => {
            let mut chars = content_chars(&system.content)
                + system
                    .sections
                    .iter()
                    .filter_map(|(_, value)| value.as_deref())
                    .map(utf16_len)
                    .sum::<usize>();
            // The lossless wrapper retains toolsAdded even when it is a newer
            // system-message extension than the provider-facing typed value.
            let wire = serde_json::to_value(message).expect("agent message serializes");
            if let Some(tools) = wire.get("toolsAdded").filter(|value| !value.is_null()) {
                chars += utf16_len(&tools.to_string());
            }
            chars
        }
        Some(Message::User(message)) => content_chars(&message.content),
        Some(Message::Assistant(message)) => message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => utf16_len(&text.text),
                ContentBlock::Thinking(thinking) => utf16_len(&thinking.thinking),
                ContentBlock::Image(_) => 0,
                ContentBlock::ToolCall(call) => {
                    utf16_len(&call.name) + utf16_len(&call.arguments.to_string())
                }
            })
            .sum(),
        Some(Message::ToolResult(message)) => content_chars(&message.content),
        Some(Message::Custom(message)) => match &message.content {
            pi_core::CustomMessageContent::Text(text) => utf16_len(text),
            pi_core::CustomMessageContent::Blocks(content) => content_chars(content),
        },
        None => custom_message_chars(message),
    };
    u64::try_from(chars.saturating_add(3) / 4).unwrap_or(u64::MAX)
}

/// Selects a source-aware cut from an already-projected history snapshot.
pub(super) fn prepare_projected_compaction(
    projected: Vec<ProjectedSessionEntry>,
    settings: CompactionSettings,
    tokens_before: u64,
) -> Option<CompactionPreparation> {
    let compactable = projected
        .into_iter()
        .map(CompactableEntry::from_projected)
        .collect::<Vec<_>>();
    let previous_index = compactable
        .iter()
        .position(|entry| entry.is_compaction && !entry.messages.is_empty());
    let previous_summary =
        previous_index.and_then(|index| match &compactable[index].source_entry.entry {
            SessionEntry::Compaction(previous) => Some(previous.summary.clone()),
            _ => None,
        });
    let boundary_start = previous_index.map_or(0, |index| index + 1);
    let cut = find_cut_point_compactable(
        &compactable,
        boundary_start,
        compactable.len(),
        settings.keep_recent_tokens,
    );
    if cut.first_kept_entry_index >= compactable.len() {
        return None;
    }
    let history_end = if cut.is_split_turn {
        cut.turn_start_index.unwrap_or(cut.first_kept_entry_index)
    } else {
        cut.first_kept_entry_index
    };
    let messages_to_summarize = messages_in(&compactable[boundary_start..history_end]);
    let turn_prefix_messages = cut
        .turn_start_index
        .filter(|_| cut.is_split_turn)
        .map_or_else(Vec::new, |start| {
            messages_in(&compactable[start..cut.first_kept_entry_index])
        });
    let retained_tail = messages_in(&compactable[cut.first_kept_entry_index..]);

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let preparation = CompactionPreparation {
        first_kept_entry_id: Some(
            compactable[cut.first_kept_entry_index]
                .source_entry
                .id
                .clone(),
        ),
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn: cut.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops: FileOperations::default(),
        settings,
    };
    Some(preparation)
}

pub(super) fn format_conversation(messages: impl IntoIterator<Item = Message>) -> String {
    let mut parts = Vec::new();
    for message in messages {
        match message {
            Message::System(_) => {}
            Message::User(message) => {
                let content = content_text(&message.content);
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(message) => {
                let mut text = Vec::new();
                let mut calls = Vec::new();
                for block in &message.content {
                    match block {
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
                        ContentBlock::Thinking(_) | ContentBlock::Image(_) => {}
                    }
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

pub(super) fn summary_prompt(
    messages: impl IntoIterator<Item = Message>,
    previous_summary: Option<&str>,
    policy: &dyn SessionCompactionPolicy,
    custom_instructions: Option<&str>,
) -> String {
    let previous_summary = previous_summary.filter(|value| !value.is_empty());
    let mut base_prompt = policy
        .summary_prompt(previous_summary.is_some())
        .to_string();
    if let Some(instructions) = custom_instructions.filter(|value| !value.is_empty()) {
        base_prompt.push_str("\n\nAdditional focus: ");
        base_prompt.push_str(instructions);
    }
    let mut prompt = format!(
        "<conversation>\n{}\n</conversation>\n\n",
        format_conversation(messages)
    );
    if let Some(previous) = previous_summary {
        prompt.push_str("<previous-summary>\n");
        prompt.push_str(previous);
        prompt.push_str("\n</previous-summary>\n\n");
    }
    prompt.push_str(&base_prompt);
    prompt
}

pub(super) fn turn_prefix_prompt(
    messages: impl IntoIterator<Item = Message>,
    policy: &dyn SessionCompactionPolicy,
    custom_instructions: Option<&str>,
) -> String {
    let mut prompt = format!(
        "# Conversation\n{}\n\n# Instructions\n{}",
        format_conversation(messages),
        policy.turn_prefix_prompt()
    );
    if let Some(instructions) = custom_instructions.filter(|value| !value.is_empty()) {
        prompt.push_str("\n\nAdditional focus: ");
        prompt.push_str(instructions);
    }
    prompt
}

pub(super) fn compaction_thinking_level(
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
struct CompactableEntry {
    source_entry: SessionRecord,
    messages: Vec<AgentMessage>,
    is_compaction: bool,
    intrinsically_visible: bool,
}

impl CompactableEntry {
    fn from_projected(entry: ProjectedSessionEntry) -> Self {
        // Snapshot-only older Rust checkpoints may not have retained provenance.
        // Their tail messages use the checkpoint as source, but are conversation,
        // not the checkpoint's own summary/system contribution.
        let is_compaction = matches!(entry.source_entry.entry, SessionEntry::Compaction(_))
            && (entry.messages.is_empty()
                || entry
                    .messages
                    .iter()
                    .any(|message| message.role() == "compactionSummary"));
        // Edits can omit message/custom_message records. Other entries cannot
        // be edited, so use their already-projected visibility without invoking
        // potentially stateful custom projectors a second time.
        let intrinsically_visible = match &entry.source_entry.entry {
            SessionEntry::Message(message) => !matches!(
                message.message.as_standard(),
                Some(Message::Assistant(assistant)) if assistant.stop_reason == StopReason::Deferred
            ),
            SessionEntry::CustomMessage(_) => true,
            _ => !entry.messages.is_empty(),
        };
        Self {
            source_entry: entry.source_entry,
            messages: entry.messages,
            is_compaction,
            intrinsically_visible,
        }
    }

    fn is_valid_cut(&self) -> bool {
        !self.is_compaction
            && self.messages.iter().any(|message| {
                matches!(
                    message.role(),
                    "user"
                        | "assistant"
                        | "bashExecution"
                        | "custom"
                        | "branchSummary"
                        | "compactionSummary"
                )
            })
    }

    fn starts_turn(&self) -> bool {
        !self.is_compaction
            && self.messages.iter().any(|message| {
                matches!(
                    message.role(),
                    "user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
                )
            })
    }

    fn is_omitted(&self) -> bool {
        self.intrinsically_visible && self.messages.is_empty()
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
    let mut exceeded_budget = false;
    let mut cut_index = first_cut;
    for index in (start_index..end_index).rev() {
        let tokens: u64 = entries[index].messages.iter().map(estimate_tokens).sum();
        if tokens == 0 {
            continue;
        }
        accumulated = accumulated.saturating_add(tokens);
        if accumulated >= keep_recent_tokens {
            exceeded_budget = true;
            cut_index = cut_points
                .iter()
                .copied()
                .find(|point| *point >= index)
                .unwrap_or(*cut_points.last().expect("nonempty cut points"));
            break;
        }
    }

    // Advance past an input only for a closed, context-invisible recovery suffix.
    // Arbitrary metadata or an external replacement must not swallow unsent input.
    let suffix = &entries[cut_index + 1..end_index];
    let omitted_ids = suffix
        .iter()
        .filter(|entry| entry.is_omitted())
        .map(|entry| entry.source_entry.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let external_replacement = suffix.iter().any(|entry| matches!(
        &entry.source_entry.entry,
        SessionEntry::ContextEdit(edit) if edit.replacement.is_some() && !omitted_ids.contains(edit.target_id.as_str())
    ));
    let recovery_suffix = exceeded_budget && !external_replacement
        && suffix.iter().any(|entry| entry.is_omitted() && matches!(
            &entry.source_entry.entry, SessionEntry::Message(message) if message.message.role() == "assistant"
        ))
        && suffix.iter().all(|entry| !entry.is_compaction && (!entry.intrinsically_visible || entry.is_omitted()));
    if recovery_suffix {
        cut_index += 1;
    }
    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if previous.is_compaction || !previous.messages.is_empty() {
            break;
        }
        cut_index -= 1;
    }
    let starts_turn = entries[cut_index].starts_turn();
    let turn_start_index = (!starts_turn)
        .then(|| {
            (start_index..=cut_index)
                .rev()
                .find(|index| entries[*index].starts_turn())
        })
        .flatten();
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

fn messages_in(entries: &[CompactableEntry]) -> Vec<AgentMessage> {
    entries
        .iter()
        .filter(|entry| !entry.is_compaction)
        .flat_map(|entry| entry.messages.iter())
        .filter(|message| message.role() != "system")
        .cloned()
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

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn content_chars(content: &[ContentBlock]) -> usize {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => utf16_len(&text.text),
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
                .map_or(0, utf16_len)
                + value
                    .get("output")
                    .and_then(Value::as_str)
                    .map_or(0, utf16_len)
        }
        "branchSummary" | "compactionSummary" => value
            .get("summary")
            .and_then(Value::as_str)
            .map_or(0, utf16_len),
        _ => 0,
    }
}

fn json_content_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => utf16_len(text),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map_or(0, utf16_len),
                Some("image") => ESTIMATED_IMAGE_CHARS,
                _ => 0,
            })
            .sum(),
        _ => 0,
    }
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            ContentBlock::Thinking(_) | ContentBlock::Image(_) | ContentBlock::ToolCall(_) => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let char_count = text.encode_utf16().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let mut units = 0;
    let prefix = text
        .chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= max_chars
        })
        .collect::<String>();
    format!(
        "{prefix}\n\n[... {} more characters truncated]",
        char_count - max_chars
    )
}

pub(super) fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input.saturating_add(second.input),
        output: first.output.saturating_add(second.output),
        cache_read: first.cache_read.saturating_add(second.cache_read),
        cache_write: first.cache_write.saturating_add(second.cache_write),
        cache_write_1h: combine_optional(first.cache_write_1h, second.cache_write_1h),
        reasoning: combine_optional(first.reasoning, second.reasoning),
        total_tokens: first.total_tokens.saturating_add(second.total_tokens),
        cost: UsageCost {
            total_from_provider: None,
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
