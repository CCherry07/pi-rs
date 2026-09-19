//! Stable, implementation-independent values shared by Pi session storage and
//! session lifecycle plugins.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use crate::{CustomMessage, CustomMessageContent, Message, ModelId, ProviderId, Usage};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, value::RawValue};

#[derive(Debug, thiserror::Error)]
pub enum SessionWireError {
    #[error("invalid session payload: {0}")]
    InvalidPayload(String),
}

/// Pi's extensible agent-level message. Standard provider messages retain
/// their typed representation; extension-defined roles are preserved as JSON.
#[derive(Debug, Clone)]
pub struct AgentMessage(Arc<AgentMessageKind>);

#[derive(Debug)]
enum AgentMessageKind {
    Standard {
        message: Message,
        original: Option<OriginalMessage>,
        display_text_cache: OnceLock<Option<String>>,
    },
    Custom {
        role: String,
        value: Value,
    },
}

enum OriginalMessage {
    Value(Value),
    SharedRaw {
        source: Arc<Vec<u8>>,
        range: Range<usize>,
    },
}

impl std::fmt::Debug for OriginalMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(value) => formatter.debug_tuple("Value").field(value).finish(),
            Self::SharedRaw { source, range } => formatter
                .debug_struct("SharedRaw")
                .field("source_bytes", &source.len())
                .field("range", range)
                .finish(),
        }
    }
}

impl OriginalMessage {
    fn shared_raw_str<'a>(source: &'a [u8], range: &Range<usize>) -> Option<&'a str> {
        std::str::from_utf8(source.get(range.clone())?).ok()
    }

    fn display_text(&self) -> Option<String> {
        match self {
            Self::Value(value) => display_text_from_value(value),
            Self::SharedRaw { source, range } => {
                serde_json::from_str::<Value>(Self::shared_raw_str(source, range)?)
                    .ok()
                    .as_ref()
                    .and_then(display_text_from_value)
            }
        }
    }
}

fn display_text_from_value(value: &Value) -> Option<String> {
    value
        .as_object()
        .and_then(|object| object.get("piRs"))
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("displayText"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

impl AgentMessage {
    pub fn custom(value: Value) -> Result<Self, SessionWireError> {
        let role = value
            .as_object()
            .and_then(|object| object.get("role"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SessionWireError::InvalidPayload(
                    "agent custom message must be an object with a string role".to_string(),
                )
            })?
            .to_string();
        if matches!(role.as_str(), "user" | "assistant" | "toolResult") {
            // Deserialize through a borrowed Value so the original wire object
            // can preserve unknown extensions without first cloning the whole
            // nested message tree.
            let message = Message::deserialize(&value)
                .map_err(|error| SessionWireError::InvalidPayload(error.to_string()))?;
            Ok(Self(Arc::new(AgentMessageKind::Standard {
                message,
                original: Some(OriginalMessage::Value(value)),
                display_text_cache: OnceLock::new(),
            })))
        } else {
            Ok(Self(Arc::new(AgentMessageKind::Custom { role, value })))
        }
    }

    #[doc(hidden)]
    pub fn from_shared_raw_json(
        source: Arc<Vec<u8>>,
        range: Range<usize>,
    ) -> Result<Self, SessionWireError> {
        let raw = OriginalMessage::shared_raw_str(&source, &range).ok_or_else(|| {
            SessionWireError::InvalidPayload("agent message source range is invalid".to_string())
        })?;
        match serde_json::from_str::<Message>(raw) {
            Ok(message @ (Message::User(_) | Message::Assistant(_) | Message::ToolResult(_))) => {
                Ok(Self(Arc::new(AgentMessageKind::Standard {
                    message,
                    original: Some(OriginalMessage::SharedRaw { source, range }),
                    display_text_cache: OnceLock::new(),
                })))
            }
            Ok(Message::Custom(_)) | Err(_) => {
                let value = serde_json::from_str(raw)
                    .map_err(|error| SessionWireError::InvalidPayload(error.to_string()))?;
                Self::custom(value)
            }
        }
    }

    pub fn role(&self) -> &str {
        match self.0.as_ref() {
            AgentMessageKind::Standard {
                message: Message::User(_),
                ..
            } => "user",
            AgentMessageKind::Standard {
                message: Message::Assistant(_),
                ..
            } => "assistant",
            AgentMessageKind::Standard {
                message: Message::ToolResult(_),
                ..
            } => "toolResult",
            AgentMessageKind::Standard {
                message: Message::Custom(_),
                ..
            } => "custom",
            AgentMessageKind::Custom { role, .. } => role,
        }
    }

    pub fn as_standard(&self) -> Option<&Message> {
        match self.0.as_ref() {
            AgentMessageKind::Standard { message, .. } => Some(message),
            AgentMessageKind::Custom { .. } => None,
        }
    }

    pub fn as_custom(&self) -> Option<&Value> {
        match self.0.as_ref() {
            AgentMessageKind::Standard { .. } => None,
            AgentMessageKind::Custom { value, .. } => Some(value),
        }
    }

    #[doc(hidden)]
    pub fn shares_replay_source_with(&self, other: &Self) -> bool {
        match (self.0.as_ref(), other.0.as_ref()) {
            (
                AgentMessageKind::Standard {
                    original:
                        Some(OriginalMessage::SharedRaw {
                            source: left_source,
                            ..
                        }),
                    ..
                },
                AgentMessageKind::Standard {
                    original:
                        Some(OriginalMessage::SharedRaw {
                            source: right_source,
                            ..
                        }),
                    ..
                },
            ) => Arc::ptr_eq(left_source, right_source),
            _ => false,
        }
    }

    pub fn with_display_text(
        message: Message,
        display_text: impl Into<String>,
    ) -> Result<Self, SessionWireError> {
        if !matches!(message, Message::User(_)) {
            return Err(SessionWireError::InvalidPayload(
                "display text can only annotate a user message".to_string(),
            ));
        }
        let mut value = serde_json::to_value(message)
            .map_err(|error| SessionWireError::InvalidPayload(error.to_string()))?;
        let object = value.as_object_mut().ok_or_else(|| {
            SessionWireError::InvalidPayload("standard user message must be an object".to_string())
        })?;
        object.insert(
            "piRs".to_string(),
            serde_json::json!({"displayText": display_text.into()}),
        );
        Self::custom(value)
    }

    pub fn display_text(&self) -> Option<&str> {
        match self.0.as_ref() {
            AgentMessageKind::Standard {
                original,
                display_text_cache,
                ..
            } => display_text_cache
                .get_or_init(|| original.as_ref().and_then(OriginalMessage::display_text))
                .as_deref(),
            _ => None,
        }
    }
}

impl PartialEq for AgentMessage {
    fn eq(&self, other: &Self) -> bool {
        match (self.0.as_ref(), other.0.as_ref()) {
            (
                AgentMessageKind::Standard { message: left, .. },
                AgentMessageKind::Standard { message: right, .. },
            ) => left == right,
            (
                AgentMessageKind::Custom { value: left, .. },
                AgentMessageKind::Custom { value: right, .. },
            ) => left == right,
            _ => false,
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        match message {
            Message::Custom(_) => {
                let value = serde_json::to_value(message)
                    .expect("pi-core custom messages always serialize to JSON");
                Self::custom(value).expect("pi-core custom messages always contain a role")
            }
            message => Self(Arc::new(AgentMessageKind::Standard {
                message,
                original: None,
                display_text_cache: OnceLock::new(),
            })),
        }
    }
}

impl Serialize for AgentMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0.as_ref() {
            AgentMessageKind::Standard {
                message, original, ..
            } => match original {
                Some(OriginalMessage::Value(value)) => value.serialize(serializer),
                Some(OriginalMessage::SharedRaw { source, range }) => {
                    let raw = OriginalMessage::shared_raw_str(source, range).ok_or_else(|| {
                        serde::ser::Error::custom("agent message source range is invalid")
                    })?;
                    let raw: &RawValue = serde_json::from_str(raw).map_err(|error| {
                        serde::ser::Error::custom(format!(
                            "agent message source is no longer valid JSON: {error}"
                        ))
                    })?;
                    raw.serialize(serializer)
                }
                None => message.serialize(serializer),
            },
            AgentMessageKind::Custom { value, .. } => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AgentMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::custom(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    pub message: AgentMessage,
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    pub provider: ProviderId,
    pub model_id: ModelId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelEntry {
    pub thinking_level: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsEntry {
    pub active_tool_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    pub summary: String,
    #[serde(default)]
    pub retained_tail: Vec<AgentMessage>,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    pub custom_type: String,
    #[serde(default)]
    pub content: CustomMessageContent,
    #[serde(default)]
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl CustomMessageEntry {
    pub fn to_message(&self, timestamp_ms: i64) -> Message {
        Message::custom(CustomMessage {
            custom_type: self.custom_type.clone(),
            content: self.content.clone(),
            display: self.display,
            details: self.details.clone(),
            timestamp_ms,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(MessageEntry),
    CustomMessage(CustomMessageEntry),
    ModelChange(ModelChangeEntry),
    ThinkingLevelChange(ThinkingLevelEntry),
    ActiveToolsChange(ActiveToolsEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
}

impl SessionEntry {
    pub fn message(message: impl Into<AgentMessage>) -> Self {
        Self::Message(MessageEntry {
            message: message.into(),
            terminate: false,
        })
    }

    pub fn custom_message(message: &CustomMessage) -> Self {
        Self::CustomMessage(CustomMessageEntry {
            custom_type: message.custom_type.clone(),
            content: message.content.clone(),
            display: message.display,
            details: message.details.clone(),
        })
    }

    pub fn entry_type(&self) -> SessionEntryType {
        match self {
            Self::Message(_) => SessionEntryType::Message,
            Self::CustomMessage(_) => SessionEntryType::CustomMessage,
            Self::ModelChange(_) => SessionEntryType::ModelChange,
            Self::ThinkingLevelChange(_) => SessionEntryType::ThinkingLevelChange,
            Self::ActiveToolsChange(_) => SessionEntryType::ActiveToolsChange,
            Self::Compaction(_) => SessionEntryType::Compaction,
            Self::BranchSummary(_) => SessionEntryType::BranchSummary,
            Self::Custom(_) => SessionEntryType::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEntryType {
    Message,
    CustomMessage,
    ModelChange,
    ThinkingLevelChange,
    ActiveToolsChange,
    Compaction,
    BranchSummary,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionedEntry {
    pub id: String,
    #[serde(flatten)]
    pub entry: SessionEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub seq: u64,
    #[serde(deserialize_with = "super::required_nullable::deserialize")]
    pub parent_id: Option<String>,
    #[serde(rename = "timestamp", with = "super::iso_timestamp_ms")]
    pub timestamp_ms: i64,
    #[serde(flatten)]
    pub entry: SessionEntry,
}

impl SessionRecord {
    pub fn provisioned(&self) -> ProvisionedEntry {
        ProvisionedEntry {
            id: self.id.clone(),
            entry: self.entry.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: HashSet<String>,
    pub written: HashSet<String>,
    pub edited: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub retained_tail: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: u64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

fn is_false(value: &bool) -> bool {
    !value
}
