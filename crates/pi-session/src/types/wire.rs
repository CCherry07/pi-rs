//! Stable, implementation-independent values shared by Pi session storage and
//! session lifecycle plugins.

use std::collections::HashSet;

use pi_core::{CustomMessage, CustomMessageContent, Message, ModelId, ProviderId, Usage};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum SessionWireError {
    #[error("invalid session payload: {0}")]
    InvalidPayload(String),
}

/// Pi's extensible agent-level message. Standard provider messages retain
/// their typed representation; extension-defined roles are preserved as JSON.
#[derive(Debug, Clone)]
pub struct AgentMessage(AgentMessageKind);

#[derive(Debug, Clone)]
enum AgentMessageKind {
    Standard {
        message: Message,
        original: Option<Value>,
    },
    Custom {
        role: String,
        value: Value,
    },
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
            let message = serde_json::from_value(value.clone())
                .map_err(|error| SessionWireError::InvalidPayload(error.to_string()))?;
            Ok(Self(AgentMessageKind::Standard {
                message,
                original: Some(value),
            }))
        } else {
            Ok(Self(AgentMessageKind::Custom { role, value }))
        }
    }

    pub fn role(&self) -> &str {
        match &self.0 {
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
        match &self.0 {
            AgentMessageKind::Standard { message, .. } => Some(message),
            AgentMessageKind::Custom { .. } => None,
        }
    }

    pub fn as_custom(&self) -> Option<&Value> {
        match &self.0 {
            AgentMessageKind::Standard { .. } => None,
            AgentMessageKind::Custom { value, .. } => Some(value),
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
        match &self.0 {
            AgentMessageKind::Standard {
                original: Some(Value::Object(object)),
                ..
            } => object
                .get("piRs")
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get("displayText"))
                .and_then(Value::as_str),
            _ => None,
        }
    }
}

impl PartialEq for AgentMessage {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
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
            message => Self(AgentMessageKind::Standard {
                message,
                original: None,
            }),
        }
    }
}

impl Serialize for AgentMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            AgentMessageKind::Standard { message, original } => match original {
                Some(value) => value.serialize(serializer),
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
    pub parent_id: Option<String>,
    #[serde(rename = "timestamp")]
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
