use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelId, ProviderId, StopReason, ToolCallId, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    User(UserMessage),
    Assistant(Arc<AssistantMessage>),
    ToolResult(Arc<ToolResultMessage>),
    Custom(Arc<CustomMessage>),
}

impl Message {
    pub fn assistant(message: AssistantMessage) -> Self {
        Self::Assistant(Arc::new(message))
    }

    pub fn tool_result(message: ToolResultMessage) -> Self {
        Self::ToolResult(Arc::new(message))
    }

    pub fn custom(message: CustomMessage) -> Self {
        Self::Custom(Arc::new(message))
    }

    pub fn is_assistant(&self) -> bool {
        matches!(self, Self::Assistant(_))
    }

    /// Projects an agent-level extension message into the provider message
    /// vocabulary. Custom messages intentionally remain distinct in agent
    /// state, lifecycle events, and session storage until this seam.
    pub fn into_provider_message(self) -> Self {
        match self {
            Self::Custom(message) => Self::User(UserMessage {
                content: message.content.to_blocks(),
                timestamp_ms: message.timestamp_ms,
            }),
            message => message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    #[serde(deserialize_with = "deserialize_user_content")]
    pub content: Vec<ContentBlock>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

impl UserMessage {
    pub fn text(text: impl Into<String>, timestamp_ms: i64) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            timestamp_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Default for CustomMessageContent {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

impl CustomMessageContent {
    pub fn to_blocks(&self) -> Vec<ContentBlock> {
        match self {
            Self::Text(text) => vec![ContentBlock::Text(TextContent::new(text.clone()))],
            Self::Blocks(blocks) => blocks.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub custom_type: String,
    #[serde(default)]
    pub content: CustomMessageContent,
    #[serde(default)]
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    pub api: String,
    pub provider: ProviderId,
    pub model: ModelId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<Value>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

impl AssistantMessage {
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: ProviderId,
    pub model_id: ModelId,
    pub api: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_signature: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl ToolCall {
    pub fn new(id: impl Into<ToolCallId>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            thought_signature: None,
            namespace: None,
        }
    }
}

fn deserialize_user_content<'de, D>(deserializer: D) -> Result<Vec<ContentBlock>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum UserContent {
        Text(String),
        Blocks(Vec<ContentBlock>),
    }

    Ok(match UserContent::deserialize(deserializer)? {
        UserContent::Text(text) => vec![ContentBlock::Text(TextContent::new(text))],
        UserContent::Blocks(blocks) => blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_redacted_thinking_have_current_pi_wire_shapes() {
        let image = ContentBlock::Image(ImageContent {
            data: "YWJj".to_string(),
            mime_type: "image/png".to_string(),
        });
        assert_eq!(
            serde_json::to_value(image).unwrap(),
            serde_json::json!({"type":"image","data":"YWJj","mimeType":"image/png"})
        );
        let redacted = ContentBlock::Thinking(ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some("opaque".to_string()),
            redacted: Some(true),
        });
        assert_eq!(
            serde_json::to_value(redacted).unwrap(),
            serde_json::json!({
                "type":"thinking",
                "thinking":"",
                "thinkingSignature":"opaque",
                "redacted":true
            })
        );
    }
}
