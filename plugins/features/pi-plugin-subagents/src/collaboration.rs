use std::collections::HashSet;

use pi_core::Message;
use pi_session::{SessionEntry, SessionRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const COLLABORATION_ENTRY_TYPE: &str = "pi.subagents.event";
const COLLABORATION_EVENT_VERSION: u64 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CollaborationMessage {
    pub id: String,
    pub sequence: u64,
    pub from: String,
    pub message: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationEventData {
    version: u64,
    root_session_id: String,
    recipient_session_id: String,
    #[serde(flatten)]
    event: CollaborationEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CollaborationEventKind {
    Message { from: String, message: String },
}

pub(crate) fn message_event_data(
    root_session_id: &str,
    recipient_session_id: &str,
    from: &str,
    message: &str,
) -> Value {
    serde_json::to_value(CollaborationEventData {
        version: COLLABORATION_EVENT_VERSION,
        root_session_id: root_session_id.to_string(),
        recipient_session_id: recipient_session_id.to_string(),
        event: CollaborationEventKind::Message {
            from: from.to_string(),
            message: message.to_string(),
        },
    })
    .expect("collaboration event data is always serializable")
}

pub(crate) fn messages_for_recipient<'a>(
    records: impl IntoIterator<Item = &'a SessionRecord>,
    root_session_id: &str,
    recipient_session_id: &str,
) -> Vec<CollaborationMessage> {
    records
        .into_iter()
        .filter_map(|record| message_from_record(record, root_session_id, recipient_session_id))
        .collect()
}

fn message_from_record(
    record: &SessionRecord,
    root_session_id: &str,
    recipient_session_id: &str,
) -> Option<CollaborationMessage> {
    let SessionEntry::Custom(entry) = &record.entry else {
        return None;
    };
    if entry.custom_type != COLLABORATION_ENTRY_TYPE {
        return None;
    }
    let event: CollaborationEventData = serde_json::from_value(entry.data.clone()?).ok()?;
    if event.version != COLLABORATION_EVENT_VERSION
        || event.root_session_id != root_session_id
        || event.recipient_session_id != recipient_session_id
    {
        return None;
    }
    let CollaborationEventKind::Message { from, message } = event.event;
    Some(CollaborationMessage {
        id: record.id.clone(),
        sequence: record.seq,
        from,
        message,
        created_at: u64::try_from(record.timestamp_ms).unwrap_or_default(),
    })
}

pub(crate) fn consumed_event_ids_from_records<'a>(
    records: impl IntoIterator<Item = &'a SessionRecord>,
) -> HashSet<String> {
    let mut consumed = HashSet::new();
    for record in records {
        match &record.entry {
            SessionEntry::Message(entry) => {
                if let Some(message) = entry.message.as_standard() {
                    extend_wait_result_ids(&mut consumed, message);
                    if let Some(id) = projection_source_record_id(message) {
                        consumed.insert(id.to_string());
                    }
                }
            }
            SessionEntry::CustomMessage(entry) => {
                if entry.custom_type == "agent_message"
                    && let Some(id) = source_record_id(entry.details.as_ref())
                {
                    consumed.insert(id.to_string());
                }
            }
            _ => {}
        }
    }
    consumed
}

pub(crate) fn consumed_event_ids_from_messages(messages: &[Message]) -> HashSet<String> {
    let mut consumed = HashSet::new();
    for message in messages {
        extend_wait_result_ids(&mut consumed, message);
    }
    consumed
}

pub(crate) fn remove_consumed_projections(
    messages: Vec<Message>,
    consumed: &HashSet<String>,
) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|message| {
            projection_source_record_id(message).is_none_or(|id| !consumed.contains(id))
        })
        .collect()
}

pub(crate) fn projection_details(
    event: &CollaborationMessage,
    recipient_session_id: &str,
) -> Value {
    json!({
        "from": event.from,
        "message": event.message,
        "recipientSessionId": recipient_session_id,
        "sourceRecordId": event.id,
        "sourceRecordSeq": event.sequence,
    })
}

fn extend_wait_result_ids(ids: &mut HashSet<String>, message: &Message) {
    let Message::ToolResult(result) = message else {
        return;
    };
    if result.tool_name != "wait_agent" {
        return;
    }
    let Some(messages) = result
        .details
        .as_ref()
        .and_then(|details| details.get("messages"))
        .and_then(Value::as_array)
    else {
        return;
    };
    ids.extend(messages.iter().filter_map(|message| {
        message
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }));
}

fn projection_source_record_id(message: &Message) -> Option<&str> {
    let Message::Custom(message) = message else {
        return None;
    };
    if message.custom_type != "agent_message" {
        return None;
    }
    source_record_id(message.details.as_ref())
}

fn source_record_id(details: Option<&Value>) -> Option<&str> {
    details?.get("sourceRecordId").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pi_core::{CustomMessage, CustomMessageContent, ToolCallId, ToolResultMessage, Usage};

    use super::*;

    #[test]
    fn wait_result_consumes_the_matching_semantic_projection() {
        let projection = Message::custom(CustomMessage {
            custom_type: "agent_message".into(),
            content: CustomMessageContent::Text("Message from agent child:\nready".into()),
            display: true,
            details: Some(json!({"sourceRecordId":"event-1"})),
            timestamp_ms: 1,
        });
        let wait_result = Message::ToolResult(Arc::new(ToolResultMessage {
            tool_call_id: ToolCallId::new("wait-1"),
            tool_name: "wait_agent".into(),
            content: Vec::new(),
            details: Some(json!({"messages":[{"id":"event-1","message":"ready"}]})),
            usage: Some(Usage::default()),
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 2,
        }));
        let unrelated = Message::custom(CustomMessage {
            custom_type: "agent_message".into(),
            content: CustomMessageContent::Text("later".into()),
            display: true,
            details: Some(json!({"sourceRecordId":"event-2"})),
            timestamp_ms: 3,
        });

        let messages = vec![projection, wait_result.clone(), unrelated.clone()];
        let consumed = consumed_event_ids_from_messages(&messages);
        let projected = remove_consumed_projections(messages, &consumed);

        assert_eq!(consumed, HashSet::from(["event-1".to_string()]));
        assert_eq!(projected, vec![wait_result, unrelated]);
    }
}
