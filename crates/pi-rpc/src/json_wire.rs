//! Pi-compatible JSON event projection shared by one-shot JSON and RPC modes.
//!
//! Product events remain strongly typed inside Rust. This module is the only
//! Adapter allowed to erase them into the coding-agent JSON wire, which keeps
//! cumulative assistant snapshots and Rust-only revision envelopes out of
//! externally consumed streams.

use pi_core::{
    AgentEvent, AssistantMessage, AssistantMessageEvent, ContentBlock, StopReason, ToolResult,
};
use pi_session::{
    AgentMessage, AgentSession, AgentSessionEvent, CompactionEntry, SessionDocument, SessionEntry,
    SessionHeader, SessionRecord,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub fn session_header_json(header: &SessionHeader) -> Result<Value, String> {
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(header.created_at).saturating_mul(1_000_000),
    )
    .map_err(|error| format!("invalid session timestamp: {error}"))?
    .format(&Rfc3339)
    .map_err(|error| format!("cannot format session timestamp: {error}"))?;
    let mut value = json!({
        "type": "session",
        "version": 3,
        "id": header.id,
        "timestamp": timestamp,
        "cwd": header.cwd,
    });
    if let Some(parent) = header.legacy_parent_session_path.as_ref() {
        value["parentSession"] = json!(parent);
    }
    Ok(value)
}

/// Returns `None` for Rust-only diagnostic/shell lifecycle events that have no
/// coding-agent JSON equivalent. Every Pi event variant is projected explicitly;
/// no `Debug` strings or catch-all `unknown` records cross the wire.
pub fn session_event_json(
    event: AgentSessionEvent,
    session: &AgentSession,
) -> Result<Option<Value>, String> {
    let value = match event {
        AgentSessionEvent::Agent(event) => agent_event_json(*event)?,
        AgentSessionEvent::AgentEnd {
            messages,
            will_retry,
        } => json!({"type":"agent_end","messages":messages,"willRetry":will_retry}),
        AgentSessionEvent::AgentSettled => json!({"type":"agent_settled"}),
        AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        } => json!({"type":"queue_update","steering":steering,"followUp":follow_up}),
        AgentSessionEvent::CompactionStart { reason } => {
            json!({"type":"compaction_start","reason":compaction_reason(reason)})
        }
        AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            will_retry,
            error_message,
        } => {
            let mut object = Map::new();
            object.insert("type".to_string(), json!("compaction_end"));
            object.insert("reason".to_string(), json!(compaction_reason(reason)));
            object.insert("aborted".to_string(), json!(aborted));
            object.insert("willRetry".to_string(), json!(will_retry));
            if let Some(record) = result {
                let document = session.log().load().map_err(|error| error.to_string())?;
                object.insert(
                    "result".to_string(),
                    compaction_result_json(&record, &document)?,
                );
            }
            insert_optional(&mut object, "errorMessage", error_message)?;
            Value::Object(object)
        }
        AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => json!({
            "type":"auto_retry_start",
            "attempt":attempt,
            "maxAttempts":max_attempts,
            "delayMs":delay_ms,
            "errorMessage":error_message,
        }),
        AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => {
            let mut object = Map::new();
            object.insert("type".to_string(), json!("auto_retry_end"));
            object.insert("success".to_string(), json!(success));
            object.insert("attempt".to_string(), json!(attempt));
            insert_optional(&mut object, "finalError", final_error)?;
            Value::Object(object)
        }
        AgentSessionEvent::EntryAppended { entry } => {
            let document = session.log().load().map_err(|error| error.to_string())?;
            json!({"type":"entry_appended","entry":session_entry_json(&entry, &document)?})
        }
        AgentSessionEvent::SessionInfoChanged { name } => {
            let mut object = Map::new();
            object.insert("type".to_string(), json!("session_info_changed"));
            insert_optional(&mut object, "name", name)?;
            Value::Object(object)
        }
        AgentSessionEvent::ThinkingLevelChanged { level } => {
            json!({"type":"thinking_level_changed","level":level})
        }
        AgentSessionEvent::BashExecutionUpdate { id, delta, .. } => {
            json!({"type":"bash_execution_update","id":id,"delta":delta})
        }
        AgentSessionEvent::ExtensionNotice { .. }
        | AgentSessionEvent::BashExecutionStart { .. }
        | AgentSessionEvent::BashExecutionEnd { .. } => return Ok(None),
        _ => return Ok(None),
    };
    Ok(Some(value))
}

/// Projects one v4 tree record into the current coding-agent v3 SessionEntry
/// wire used by JSON and RPC consumers.
pub(crate) fn session_entry_json(
    record: &SessionRecord,
    document: &SessionDocument,
) -> Result<Value, String> {
    if let SessionEntry::Custom(custom) = &record.entry
        && custom.custom_type.starts_with("pi.coding-agent.legacy.")
        && let Some(original @ Value::Object(_)) = &custom.data
    {
        return Ok(original.clone());
    }
    let mut object = Map::new();
    object.insert("id".to_string(), json!(record.id));
    object.insert("parentId".to_string(), json!(record.parent_id));
    object.insert(
        "timestamp".to_string(),
        Value::String(timestamp_string(record.timestamp_ms)?),
    );
    match &record.entry {
        SessionEntry::Message(message) => {
            object.insert("type".to_string(), json!("message"));
            object.insert("message".to_string(), json!(message.message));
        }
        SessionEntry::CustomMessage(message) => {
            object.insert("type".to_string(), json!("custom_message"));
            object.insert("customType".to_string(), json!(message.custom_type));
            object.insert("content".to_string(), json!(message.content));
            object.insert("display".to_string(), json!(message.display));
            if let Some(details) = &message.details {
                object.insert("details".to_string(), details.clone());
            }
        }
        SessionEntry::ModelChange(change) => {
            object.insert("type".to_string(), json!("model_change"));
            object.insert("provider".to_string(), json!(change.provider));
            object.insert("modelId".to_string(), json!(change.model_id));
        }
        SessionEntry::ThinkingLevelChange(change) => {
            object.insert("type".to_string(), json!("thinking_level_change"));
            object.insert("thinkingLevel".to_string(), json!(change.thinking_level));
        }
        SessionEntry::Compaction(_) => {
            object.insert("type".to_string(), json!("compaction"));
            let Value::Object(result) = compaction_result_json(record, document)? else {
                unreachable!("compaction result projection is always an object");
            };
            object.extend(result);
        }
        SessionEntry::BranchSummary(summary) => {
            object.insert("type".to_string(), json!("branch_summary"));
            object.insert("fromId".to_string(), json!(summary.from_id));
            object.insert("summary".to_string(), json!(summary.summary));
            if let Some(details) = &summary.details {
                object.insert("details".to_string(), details.clone());
            }
            if let Some(usage) = &summary.usage {
                object.insert("usage".to_string(), json!(usage));
            }
        }
        SessionEntry::Custom(custom) => {
            object.insert("type".to_string(), json!("custom"));
            object.insert("customType".to_string(), json!(custom.custom_type));
            if let Some(data) = &custom.data {
                object.insert("data".to_string(), data.clone());
            }
        }
        SessionEntry::ActiveToolsChange(change) => {
            object.insert("type".to_string(), json!("custom"));
            object.insert("customType".to_string(), json!("pi-rs.active_tools_change"));
            object.insert(
                "data".to_string(),
                json!({"activeToolNames":change.active_tool_names}),
            );
        }
    }
    Ok(Value::Object(object))
}

/// Projects the public coding-agent compaction result. The v4 journal stores
/// the retained messages; the tree record restores Pi's boundary entry ID.
pub(crate) fn compaction_result_json(
    record: &SessionRecord,
    document: &SessionDocument,
) -> Result<Value, String> {
    let SessionEntry::Compaction(compaction) = &record.entry else {
        return Err(format!("session entry {} is not a compaction", record.id));
    };
    let mut object = Map::new();
    object.insert("summary".to_string(), json!(compaction.summary));
    object.insert(
        "firstKeptEntryId".to_string(),
        json!(first_kept_entry_id(record, compaction, document)),
    );
    object.insert("tokensBefore".to_string(), json!(compaction.tokens_before));
    if let Some(details) = &compaction.details {
        object.insert("details".to_string(), details.clone());
    }
    if let Some(usage) = &compaction.usage {
        object.insert("usage".to_string(), json!(usage));
    }
    Ok(Value::Object(object))
}

fn first_kept_entry_id(
    compaction_record: &SessionRecord,
    compaction: &CompactionEntry,
    document: &SessionDocument,
) -> String {
    let Some(first) = compaction.retained_tail.first() else {
        return compaction_record
            .parent_id
            .clone()
            .unwrap_or_else(|| compaction_record.id.clone());
    };
    let by_id = document
        .entries
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<std::collections::HashMap<_, _>>();
    let mut path = Vec::new();
    let mut current = compaction_record.parent_id.as_deref();
    while let Some(id) = current {
        let Some(record) = by_id.get(id) else {
            break;
        };
        path.push(*record);
        current = record.parent_id.as_deref();
    }
    path.into_iter()
        .rev()
        .find(|record| match &record.entry {
            SessionEntry::Message(message) => &message.message == first,
            SessionEntry::CustomMessage(message) => {
                AgentMessage::from(message.to_message(record.timestamp_ms)) == *first
            }
            _ => false,
        })
        .map(|record| record.id.clone())
        .or_else(|| compaction_record.parent_id.clone())
        .unwrap_or_else(|| compaction_record.id.clone())
}

fn timestamp_string(timestamp_ms: i64) -> Result<String, String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp_ms).saturating_mul(1_000_000))
        .map_err(|error| error.to_string())?
        .format(&Rfc3339)
        .map_err(|error| error.to_string())
}

fn agent_event_json(event: AgentEvent) -> Result<Value, String> {
    Ok(match event {
        AgentEvent::AgentStart => json!({"type":"agent_start"}),
        AgentEvent::AgentEnd { messages } => {
            // Production session streams use AgentSessionEvent::AgentEnd,
            // which carries the session retry decision. Keep this fallback
            // for directly constructed core events.
            json!({"type":"agent_end","messages":messages,"willRetry":false})
        }
        AgentEvent::TurnStart => json!({"type":"turn_start"}),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => json!({"type":"turn_end","message":message,"toolResults":tool_results}),
        AgentEvent::MessageStart { message } => json!({"type":"message_start","message":message}),
        AgentEvent::MessageUpdate { message, event } => message_update_json(&message, event)?,
        AgentEvent::MessageEnd { message } => json!({"type":"message_end","message":message}),
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => json!({
            "type":"tool_execution_start",
            "toolCallId":tool_call_id,
            "toolName":tool_name,
            "args":args,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => json!({
            "type":"tool_execution_update",
            "toolCallId":tool_call_id,
            "toolName":tool_name,
            "args":args,
            "partialResult":tool_result_json(&partial_result),
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => json!({
            "type":"tool_execution_end",
            "toolCallId":tool_call_id,
            "toolName":tool_name,
            "result":tool_result_json(&result),
            "isError":is_error,
        }),
    })
}

fn message_update_json(
    message: &AssistantMessage,
    event: AssistantMessageEvent,
) -> Result<Value, String> {
    let assistant_message_event = match event {
        AssistantMessageEvent::Start => json!({"type":"start"}),
        AssistantMessageEvent::TextStart { content_index } => {
            json!({"type":"text_start","contentIndex":content_index})
        }
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => json!({"type":"text_delta","contentIndex":content_index,"delta":delta}),
        AssistantMessageEvent::TextEnd { content_index } => json!({
            "type":"text_end",
            "contentIndex":content_index,
            "content":text_at(message, content_index)?,
        }),
        AssistantMessageEvent::ThinkingStart { content_index } => {
            json!({"type":"thinking_start","contentIndex":content_index})
        }
        AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => json!({"type":"thinking_delta","contentIndex":content_index,"delta":delta}),
        AssistantMessageEvent::ThinkingEnd { content_index } => json!({
            "type":"thinking_end",
            "contentIndex":content_index,
            "content":thinking_at(message, content_index)?,
        }),
        AssistantMessageEvent::ToolCallStart { content_index } => {
            let call = tool_call_at(message, content_index)?;
            json!({
                "type":"toolcall_start",
                "contentIndex":content_index,
                "id":call.id,
                "toolName":call.name,
            })
        }
        AssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => json!({"type":"toolcall_delta","contentIndex":content_index,"delta":delta}),
        AssistantMessageEvent::ToolCallEnd { content_index } => json!({
            "type":"toolcall_end",
            "contentIndex":content_index,
            "toolCall":tool_call_at(message, content_index)?,
        }),
        AssistantMessageEvent::Done => match message.stop_reason {
            StopReason::Error | StopReason::Aborted => json!({
                "type":"error",
                "reason":message.stop_reason,
                "error":message,
            }),
            StopReason::Stop | StopReason::Length | StopReason::ToolUse | StopReason::Deferred => {
                json!({"type":"done","reason":message.stop_reason,"message":message})
            }
            StopReason::Pending => {
                return Err("message_update done event has pending stop reason".to_string());
            }
        },
    };
    Ok(json!({
        "type":"message_update",
        "usage":message.usage,
        "assistantMessageEvent":assistant_message_event,
    }))
}

fn text_at(message: &AssistantMessage, index: usize) -> Result<&str, String> {
    match message.content.get(index) {
        Some(ContentBlock::Text(text)) => Ok(&text.text),
        _ => Err(format!("text event content at index {index} is not text")),
    }
}

fn thinking_at(message: &AssistantMessage, index: usize) -> Result<&str, String> {
    match message.content.get(index) {
        Some(ContentBlock::Thinking(thinking)) => Ok(&thinking.thinking),
        _ => Err(format!(
            "thinking event content at index {index} is not thinking"
        )),
    }
}

fn tool_call_at(message: &AssistantMessage, index: usize) -> Result<&pi_core::ToolCall, String> {
    match message.content.get(index) {
        Some(ContentBlock::ToolCall(call)) => Ok(call),
        _ => Err(format!(
            "toolcall event content at index {index} is not a tool call"
        )),
    }
}

fn tool_result_json(result: &ToolResult) -> Value {
    let mut object = Map::new();
    object.insert("content".to_string(), json!(result.content));
    if let Some(details) = &result.details {
        object.insert("details".to_string(), details.clone());
    }
    if let Some(usage) = &result.usage {
        object.insert("usage".to_string(), json!(usage));
    }
    if let Some(names) = &result.added_tool_names {
        object.insert("addedToolNames".to_string(), json!(names));
    }
    if result.terminate {
        object.insert("terminate".to_string(), Value::Bool(true));
    }
    Value::Object(object)
}

fn insert_optional<T: Serialize>(
    object: &mut Map<String, Value>,
    key: &str,
    value: Option<T>,
) -> Result<(), String> {
    if let Some(value) = value {
        object.insert(
            key.to_string(),
            serde_json::to_value(value).map_err(|error| error.to_string())?,
        );
    }
    Ok(())
}

fn compaction_reason(reason: pi_session::CompactionReason) -> &'static str {
    match reason {
        pi_session::CompactionReason::Manual => "manual",
        pi_session::CompactionReason::Threshold => "threshold",
        pi_session::CompactionReason::Overflow => "overflow",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pi_core::{AssistantMessage, Message, TextContent, ToolCall, Usage, UserMessage};
    use pi_session::{HeaderKind, SESSION_SCHEMA_VERSION, SessionLog};

    use super::*;

    fn assistant(content: Vec<ContentBlock>, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "test".to_string(),
            provider: "test".into(),
            model: "model".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage {
                input: 1,
                output: 2,
                total_tokens: 3,
                ..Usage::default()
            },
            stop_reason,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        }
    }

    #[test]
    fn header_uses_coding_agent_v3_wire() {
        let value = session_header_json(&SessionHeader {
            kind: HeaderKind::Header,
            version: SESSION_SCHEMA_VERSION,
            id: "session".to_string(),
            created_at: 1_735_689_600_000,
            cwd: PathBuf::from("/tmp/project"),
            parent_session_id: None,
            legacy_parent_session_path: Some(PathBuf::from("/tmp/parent.jsonl")),
            metadata: None,
        })
        .unwrap();

        assert_eq!(value["type"], "session");
        assert_eq!(value["version"], 3);
        assert_eq!(value["timestamp"], "2025-01-01T00:00:00Z");
        assert_eq!(value["parentSession"], "/tmp/parent.jsonl");
        assert!(value.get("kind").is_none());
    }

    #[test]
    fn message_updates_are_delta_only_and_recover_constant_tool_fields() {
        let text = assistant(
            vec![ContentBlock::Text(TextContent::new("hello"))],
            StopReason::Stop,
        );
        assert_eq!(
            agent_event_json(AgentEvent::MessageUpdate {
                message: text,
                event: AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: "lo".to_string(),
                },
            })
            .unwrap(),
            json!({
                "type":"message_update",
                "usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},
                "assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"lo"},
            })
        );

        let tool = assistant(
            vec![ContentBlock::ToolCall(ToolCall::new(
                "call-1",
                "read",
                json!({"path":"README.md"}),
            ))],
            StopReason::ToolUse,
        );
        let value = agent_event_json(AgentEvent::MessageUpdate {
            message: tool,
            event: AssistantMessageEvent::ToolCallStart { content_index: 0 },
        })
        .unwrap();
        assert_eq!(value["assistantMessageEvent"]["type"], "toolcall_start");
        assert_eq!(value["assistantMessageEvent"]["id"], "call-1");
        assert_eq!(value["assistantMessageEvent"]["toolName"], "read");
        assert!(value.get("message").is_none());
    }

    #[test]
    fn tool_updates_keep_args_content_and_details() {
        let value = agent_event_json(AgentEvent::ToolExecutionUpdate {
            tool_call_id: "call-1".into(),
            tool_name: "bash".to_string(),
            args: json!({"command":"pwd"}),
            partial_result: ToolResult {
                content: vec![ContentBlock::Text(TextContent::new("/tmp"))],
                details: Some(json!({"fullOutputPath":null})),
                usage: None,
                added_tool_names: None,
                is_error: false,
                terminate: false,
            },
        })
        .unwrap();

        assert_eq!(value["args"], json!({"command":"pwd"}));
        assert_eq!(value["partialResult"]["content"][0]["text"], "/tmp");
        assert_eq!(
            value["partialResult"]["details"],
            json!({"fullOutputPath":null})
        );
        assert!(value["partialResult"].get("isError").is_none());
        assert!(value["partialResult"].get("terminate").is_none());
        assert!(value["partialResult"].get("usage").is_none());
    }

    #[test]
    fn session_entry_wire_keeps_v3_tree_identity_and_iso_timestamp() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("session.jsonl"),
            SessionHeader::new("session", directory.path()),
        )
        .unwrap();
        let record = log
            .append_session_record(SessionEntry::message(Message::User(UserMessage {
                content: vec![ContentBlock::Text(TextContent::new("hello"))],
                timestamp_ms: 1,
            })))
            .unwrap();
        let document = log.load().unwrap();

        let value = session_entry_json(&record, &document).unwrap();

        assert_eq!(value["type"], "message");
        assert_eq!(value["id"], record.id);
        assert_eq!(value["parentId"], Value::Null);
        assert!(value["timestamp"].as_str().unwrap().contains('T'));
        assert!(value.get("seq").is_none());
        assert!(value.get("entry").is_none());
    }
}
