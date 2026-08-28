use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pi_core::{
    ContentBlock, Message, ModelId, ProviderId, StopReason, ToolCallId, ToolResultMessage,
    UserMessage,
};
use serde_json::{Value, json};

use crate::{
    AgentMessage, CustomEntry, MAIN_LANE, SessionDocument, SessionEntry, SessionError,
    SessionRecord,
};

const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";
const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionModel {
    pub provider: ProviderId,
    pub model_id: ModelId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionModel>,
    pub active_tool_names: Option<Vec<String>>,
}

pub type ContextEntryTransform = Arc<dyn Fn(&[SessionRecord]) -> Vec<SessionRecord> + Send + Sync>;
pub type CustomEntryContextMessageProjector =
    Arc<dyn Fn(&CustomEntry, usize, &[SessionRecord]) -> Vec<AgentMessage> + Send + Sync>;

#[derive(Clone, Default)]
pub struct SessionContextBuildOptions {
    pub entry_transforms: Vec<ContextEntryTransform>,
    pub entry_projectors: HashMap<String, CustomEntryContextMessageProjector>,
}

/// Applies Pi's built-in context boundary: only the latest compaction and the
/// entries after it remain. The compaction's retained tail is materialized by
/// `session_entry_to_context_messages` rather than spliced into the tree.
pub fn default_context_entry_transform(path_entries: &[SessionRecord]) -> Vec<SessionRecord> {
    match path_entries
        .iter()
        .rposition(|record| matches!(record.entry, SessionEntry::Compaction(_)))
    {
        Some(index) => path_entries[index..].to_vec(),
        None => path_entries.to_vec(),
    }
}

pub fn build_context_entries(
    path_entries: &[SessionRecord],
    options: &SessionContextBuildOptions,
) -> Vec<SessionRecord> {
    let mut entries = default_context_entry_transform(path_entries);
    for transform in &options.entry_transforms {
        entries = transform(&entries);
    }
    entries
}

pub fn session_entry_to_context_messages(
    record: &SessionRecord,
    index: usize,
    entries: &[SessionRecord],
    options: &SessionContextBuildOptions,
) -> Vec<AgentMessage> {
    match &record.entry {
        SessionEntry::Message(message) => {
            if matches!(
                message.message.as_standard(),
                Some(Message::Assistant(assistant)) if assistant.stop_reason == StopReason::Deferred
            ) {
                Vec::new()
            } else {
                vec![message.message.clone()]
            }
        }
        SessionEntry::CustomMessage(message) => {
            vec![AgentMessage::from(message.to_message(record.timestamp_ms))]
        }
        SessionEntry::Compaction(compaction) => {
            let mut messages = Vec::with_capacity(compaction.retained_tail.len() + 1);
            messages.push(
                AgentMessage::custom(json!({
                    "role": "compactionSummary",
                    "summary": compaction.summary,
                    "tokensBefore": compaction.tokens_before,
                    "timestamp": record.timestamp_ms,
                }))
                .expect("built-in compaction summary is a valid agent message"),
            );
            messages.extend(compaction.retained_tail.clone());
            messages
        }
        SessionEntry::BranchSummary(summary) if !summary.summary.is_empty() => {
            vec![
                AgentMessage::custom(json!({
                    "role": "branchSummary",
                    "summary": summary.summary,
                    "fromId": summary.from_id,
                    "timestamp": record.timestamp_ms,
                }))
                .expect("built-in branch summary is a valid agent message"),
            ]
        }
        SessionEntry::Custom(custom) => options
            .entry_projectors
            .get(&custom.custom_type)
            .map_or_else(Vec::new, |projector| projector(custom, index, entries)),
        SessionEntry::ModelChange(_)
        | SessionEntry::ThinkingLevelChange(_)
        | SessionEntry::ActiveToolsChange(_)
        | SessionEntry::BranchSummary(_) => Vec::new(),
    }
}

pub fn build_session_context(
    path_entries: &[SessionRecord],
    options: &SessionContextBuildOptions,
) -> SessionContext {
    // Configuration derives from the full path, not the post-compaction
    // projection. This is the subtle but intentional v4 behavior.
    let mut thinking_level = "off".to_string();
    let mut model = None;
    let mut active_tool_names = None;
    for record in path_entries {
        match &record.entry {
            SessionEntry::ThinkingLevelChange(change) => {
                thinking_level.clone_from(&change.thinking_level);
            }
            SessionEntry::ModelChange(change) => {
                model = Some(SessionModel {
                    provider: change.provider.clone(),
                    model_id: change.model_id.clone(),
                });
            }
            SessionEntry::Message(message) => {
                if let Some(Message::Assistant(assistant)) = message.message.as_standard()
                    && assistant.provider.as_str() != "unknown"
                    && assistant.model.as_str() != "unknown"
                {
                    model = Some(SessionModel {
                        provider: assistant.provider.clone(),
                        model_id: assistant.model.clone(),
                    });
                }
            }
            SessionEntry::ActiveToolsChange(change) => {
                active_tool_names = Some(change.active_tool_names.clone());
            }
            SessionEntry::Compaction(_)
            | SessionEntry::BranchSummary(_)
            | SessionEntry::CustomMessage(_)
            | SessionEntry::Custom(_) => {}
        }
    }

    let context_entries = build_context_entries(path_entries, options);
    let messages = context_entries
        .iter()
        .enumerate()
        .flat_map(|(index, record)| {
            session_entry_to_context_messages(record, index, &context_entries, options)
        })
        .collect();
    SessionContext {
        messages,
        thinking_level,
        model,
        active_tool_names,
    }
}

impl SessionContext {
    /// Applies Pi's default `convertToLlm` projection. Extension roles unknown
    /// to the default converter remain in `messages` but are omitted here.
    /// Histories produced by older pi-rs versions may end a run immediately
    /// after an assistant tool call; synthesize failed results for those calls
    /// so providers receive a structurally valid message sequence.
    pub fn provider_messages(&self) -> Vec<Message> {
        let messages = self
            .messages
            .iter()
            .filter_map(agent_message_to_provider_message)
            .collect();
        repair_dangling_tool_calls(messages)
    }

    /// Rebuilds agent state without collapsing Pi custom messages into their
    /// provider-facing user-message projection.
    pub fn runtime_messages(&self) -> Vec<Message> {
        let messages = self
            .messages
            .iter()
            .filter_map(agent_message_to_runtime_message)
            .collect();
        repair_dangling_tool_calls(messages)
    }
}

fn repair_dangling_tool_calls(messages: Vec<Message>) -> Vec<Message> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut pending = Vec::<(ToolCallId, String, i64)>::new();

    for message in messages {
        if !matches!(message, Message::ToolResult(_)) {
            append_missing_tool_results(&mut repaired, &mut pending);
        }
        match &message {
            Message::Assistant(assistant) => {
                pending.extend(
                    assistant
                        .tool_calls()
                        .into_iter()
                        .map(|call| (call.id, call.name, assistant.timestamp_ms)),
                );
            }
            Message::ToolResult(result) => {
                if let Some(index) = pending
                    .iter()
                    .position(|(id, _, _)| id == &result.tool_call_id)
                {
                    pending.remove(index);
                }
            }
            Message::User(_) | Message::Custom(_) => {}
        }
        repaired.push(message);
    }
    append_missing_tool_results(&mut repaired, &mut pending);
    repaired
}

fn append_missing_tool_results(
    messages: &mut Vec<Message>,
    pending: &mut Vec<(ToolCallId, String, i64)>,
) {
    messages.extend(pending.drain(..).map(|(tool_call_id, tool_name, timestamp_ms)| {
        Message::tool_result(ToolResultMessage {
            content: vec![ContentBlock::Text(pi_core::TextContent::new(format!(
                "Tool call \"{tool_name}\" did not complete because the previous agent run ended before recording a result. Continue without assuming it succeeded."
            )))],
            tool_call_id,
            tool_name,
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: true,
            timestamp_ms,
        })
    }));
}

pub fn agent_message_to_provider_message(message: &AgentMessage) -> Option<Message> {
    if let Some(message) = message.as_standard() {
        return Some(message.clone().into_provider_message());
    }
    let value = message.as_custom()?;
    match value.get("role").and_then(Value::as_str)? {
        "branchSummary" => Some(Message::User(UserMessage::text(
            format!(
                "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                value.get("summary")?.as_str()?
            ),
            json_timestamp(value)?,
        ))),
        "compactionSummary" => Some(Message::User(UserMessage::text(
            format!(
                "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                value.get("summary")?.as_str()?
            ),
            json_timestamp(value)?,
        ))),
        "custom" => {
            let content = match value.get("content")? {
                Value::String(text) => {
                    vec![ContentBlock::Text(pi_core::TextContent::new(text.clone()))]
                }
                Value::Array(_) => serde_json::from_value(value.get("content")?.clone()).ok()?,
                _ => return None,
            };
            Some(Message::User(UserMessage {
                content,
                timestamp_ms: json_timestamp(value)?,
            }))
        }
        "bashExecution" if value.get("excludeFromContext") == Some(&Value::Bool(true)) => None,
        "bashExecution" => Some(Message::User(UserMessage::text(
            bash_execution_to_text(value)?,
            json_timestamp(value)?,
        ))),
        _ => None,
    }
}

pub fn agent_message_to_runtime_message(message: &AgentMessage) -> Option<Message> {
    if let Some(message) = message.as_standard() {
        return Some(message.clone());
    }
    let value = message.as_custom()?;
    if value.get("role").and_then(Value::as_str) == Some("custom") {
        return serde_json::from_value(value.clone()).ok();
    }
    agent_message_to_provider_message(message)
}

fn json_timestamp(value: &Value) -> Option<i64> {
    value.get("timestamp")?.as_i64().or_else(|| {
        value
            .get("timestamp")?
            .as_u64()
            .and_then(|timestamp| i64::try_from(timestamp).ok())
    })
}

fn bash_execution_to_text(value: &Value) -> Option<String> {
    let command = value.get("command")?.as_str()?;
    let output = value.get("output")?.as_str()?;
    let mut text = format!("Ran `{command}`\n");
    if output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str("```\n");
        text.push_str(output);
        text.push_str("\n```");
    }
    if value.get("cancelled").and_then(Value::as_bool) == Some(true) {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(exit_code) = value.get("exitCode").and_then(Value::as_i64)
        && exit_code != 0
    {
        text.push_str(&format!("\n\nCommand exited with code {exit_code}"));
    }
    if value.get("truncated").and_then(Value::as_bool) == Some(true)
        && let Some(path) = value.get("fullOutputPath").and_then(Value::as_str)
    {
        text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
    }
    Some(text)
}

impl SessionDocument {
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.entries
            .iter()
            .filter_map(|record| match &record.entry {
                SessionEntry::Message(message) => Some(message.message.clone()),
                SessionEntry::CustomMessage(message) => {
                    Some(AgentMessage::from(message.to_message(record.timestamp_ms)))
                }
                _ => None,
            })
            .collect()
    }

    pub fn leaf_id(&self, lane: &str) -> Result<Option<&str>, SessionError> {
        self.lanes
            .iter()
            .find(|pointer| pointer.lane == lane)
            .map(|pointer| pointer.leaf_id.as_deref())
            .ok_or_else(|| SessionError::InvalidLane(format!("lane not found: {lane}")))
    }

    pub fn branch(&self) -> Result<Vec<&SessionRecord>, SessionError> {
        self.branch_for_lane(MAIN_LANE)
    }

    pub fn branch_for_lane(&self, lane: &str) -> Result<Vec<&SessionRecord>, SessionError> {
        self.branch_at(self.leaf_id(lane)?)
    }

    pub fn branch_at(&self, leaf_id: Option<&str>) -> Result<Vec<&SessionRecord>, SessionError> {
        let by_id = self
            .entries
            .iter()
            .map(|record| (record.id.as_str(), record))
            .collect::<HashMap<_, _>>();
        let mut current = match leaf_id {
            Some(id) => Some(
                *by_id
                    .get(id)
                    .ok_or_else(|| SessionError::NotFound(id.to_string()))?,
            ),
            None => None,
        };
        let mut visited = HashSet::new();
        let mut branch = Vec::new();
        while let Some(record) = current {
            if !visited.insert(record.id.as_str()) {
                return Err(SessionError::InvalidEntry(format!(
                    "session branch contains a cycle at {}",
                    record.id
                )));
            }
            branch.push(record);
            current = match record.parent_id.as_deref() {
                Some(parent_id) => Some(*by_id.get(parent_id).ok_or_else(|| {
                    SessionError::InvalidEntry(format!("entry not found: {parent_id}"))
                })?),
                None => None,
            };
        }
        branch.reverse();
        Ok(branch)
    }

    pub fn context(&self) -> Result<SessionContext, SessionError> {
        self.context_with_options(&SessionContextBuildOptions::default())
    }

    pub fn context_with_options(
        &self,
        options: &SessionContextBuildOptions,
    ) -> Result<SessionContext, SessionError> {
        self.context_at_with_options(self.leaf_id(MAIN_LANE)?, options)
    }

    pub fn context_at(&self, leaf_id: Option<&str>) -> Result<SessionContext, SessionError> {
        self.context_at_with_options(leaf_id, &SessionContextBuildOptions::default())
    }

    pub fn context_at_with_options(
        &self,
        leaf_id: Option<&str>,
        options: &SessionContextBuildOptions,
    ) -> Result<SessionContext, SessionError> {
        let path = self
            .branch_at(leaf_id)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        Ok(build_session_context(&path, options))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pi_core::{
        AssistantMessage, ContentBlock, CustomMessageContent, Message, ModelId, ProviderId,
        StopReason, TextContent, ToolCall, Usage, UserMessage,
    };
    use serde_json::json;

    use super::*;
    use crate::{
        CompactionEntry, CustomEntry, CustomMessageEntry, MessageEntry, ModelChangeEntry,
        SessionEntry, ThinkingLevelEntry,
    };

    fn record(id: &str, seq: u64, parent_id: Option<&str>, entry: SessionEntry) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            seq,
            parent_id: parent_id.map(str::to_string),
            timestamp_ms: i64::try_from(seq).unwrap(),
            entry,
        }
    }

    fn user(text: &str) -> AgentMessage {
        Message::User(UserMessage::text(text, 1)).into()
    }

    fn assistant(text: &str, stop_reason: StopReason) -> AgentMessage {
        Message::assistant(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            api: "anthropic-messages".to_string(),
            provider: ProviderId::new("anthropic"),
            model: ModelId::new("claude"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        })
        .into()
    }

    fn assistant_tool_call(id: &str, name: &str) -> AgentMessage {
        Message::assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall(ToolCall::new(id, name, json!({})))],
            api: "openai-completions".to_string(),
            provider: ProviderId::new("openai"),
            model: ModelId::new("gpt"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 2,
        })
        .into()
    }

    #[test]
    fn latest_compaction_materializes_retained_tail_and_preserves_full_path_state() {
        let path = vec![
            record("old", 1, None, SessionEntry::message(user("old"))),
            record(
                "compact",
                2,
                Some("old"),
                SessionEntry::Compaction(CompactionEntry {
                    summary: "summary".to_string(),
                    retained_tail: vec![user("retained"), assistant("answer", StopReason::Stop)],
                    tokens_before: 100,
                    details: None,
                    usage: None,
                }),
            ),
            record(
                "model",
                3,
                Some("compact"),
                SessionEntry::ModelChange(ModelChangeEntry {
                    provider: ProviderId::new("openai"),
                    model_id: ModelId::new("gpt-5"),
                }),
            ),
            record(
                "thinking",
                4,
                Some("model"),
                SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                    thinking_level: "high".to_string(),
                }),
            ),
            record(
                "tail",
                5,
                Some("thinking"),
                SessionEntry::Message(MessageEntry {
                    message: user("tail"),
                    terminate: false,
                }),
            ),
        ];

        let context = build_session_context(&path, &SessionContextBuildOptions::default());
        assert_eq!(context.messages.len(), 4);
        assert_eq!(
            context
                .messages
                .iter()
                .map(AgentMessage::role)
                .collect::<Vec<_>>(),
            vec!["compactionSummary", "user", "assistant", "user"]
        );
        assert_eq!(context.provider_messages().len(), 4);
        assert_eq!(context.thinking_level, "high");
        assert_eq!(context.model.unwrap().model_id.as_str(), "gpt-5");
    }

    #[test]
    fn transforms_run_after_compaction_and_custom_projectors_omit_deferred_handles() {
        let path = vec![
            record("user", 1, None, SessionEntry::message(user("hello"))),
            record(
                "deferred",
                2,
                Some("user"),
                SessionEntry::message(assistant("", StopReason::Deferred)),
            ),
            record(
                "custom",
                3,
                Some("deferred"),
                SessionEntry::Custom(CustomEntry {
                    custom_type: "note".to_string(),
                    data: Some(json!("project me")),
                }),
            ),
        ];
        let mut options = SessionContextBuildOptions::default();
        options.entry_projectors.insert(
            "note".to_string(),
            Arc::new(|custom, _, _| {
                vec![user(&format!(
                    "note: {}",
                    custom
                        .data
                        .as_ref()
                        .and_then(|value| value.as_str())
                        .unwrap()
                ))]
            }),
        );

        let context = build_session_context(&path, &options);
        assert_eq!(context.messages.len(), 2);
    }

    #[test]
    fn default_provider_projection_matches_pi_custom_and_bash_messages() {
        let custom = AgentMessage::custom(json!({
            "role": "custom",
            "customType": "notice",
            "content": "visible",
            "display": true,
            "timestamp": 5
        }))
        .unwrap();
        let bash = AgentMessage::custom(json!({
            "role": "bashExecution",
            "command": "pwd",
            "output": "/repo",
            "exitCode": 0,
            "cancelled": false,
            "truncated": false,
            "timestamp": 6
        }))
        .unwrap();
        let hidden = AgentMessage::custom(json!({
            "role": "bashExecution",
            "command": "secret",
            "output": "hidden",
            "cancelled": false,
            "truncated": false,
            "excludeFromContext": true,
            "timestamp": 7
        }))
        .unwrap();
        let context = SessionContext {
            messages: vec![custom, bash, hidden],
            thinking_level: "off".to_string(),
            model: None,
            active_tool_names: None,
        };
        let projected = context.provider_messages();
        assert_eq!(projected.len(), 2);
        assert!(matches!(&projected[0], Message::User(user)
            if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "visible")));
        assert!(matches!(&projected[1], Message::User(user)
            if matches!(&user.content[0], ContentBlock::Text(text) if text.text.contains("Ran `pwd`"))));
    }

    #[test]
    fn custom_message_entries_preserve_runtime_identity_and_project_to_provider_user_messages() {
        let entry = SessionEntry::CustomMessage(CustomMessageEntry {
            custom_type: "fixture-context".to_string(),
            content: CustomMessageContent::Text("injected context".to_string()),
            display: false,
            details: Some(json!({"source": "fixture"})),
        });
        let wire = serde_json::to_value(&entry).unwrap();
        assert_eq!(wire["type"], "custom_message");
        assert_eq!(wire["customType"], "fixture-context");

        let context = build_session_context(
            &[record("custom", 5, None, entry)],
            &SessionContextBuildOptions::default(),
        );
        assert_eq!(context.messages[0].role(), "custom");
        assert!(matches!(
            &context.runtime_messages()[0],
            Message::Custom(message)
                if message.custom_type == "fixture-context"
                    && message.details == Some(json!({"source": "fixture"}))
        ));
        assert!(matches!(
            &context.provider_messages()[0],
            Message::User(message)
                if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "injected context")
        ));
    }

    #[test]
    fn provider_projection_repairs_a_dangling_tool_call_before_the_next_message() {
        let context = SessionContext {
            messages: vec![
                user("run this"),
                assistant_tool_call("call-missing", "bash"),
                user("continue"),
            ],
            thinking_level: "off".to_string(),
            model: None,
            active_tool_names: None,
        };

        let projected = context.provider_messages();

        assert_eq!(projected.len(), 4);
        assert!(matches!(&projected[2], Message::ToolResult(result)
            if result.tool_call_id.as_str() == "call-missing"
                && result.tool_name == "bash"
                && result.is_error));
        assert!(matches!(&projected[3], Message::User(_)));
    }
}
