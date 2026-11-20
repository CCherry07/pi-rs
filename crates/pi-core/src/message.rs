use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelId, ProviderId, StopReason, ToolCallId, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    System(SystemMessage),
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

/// Instructions at a point in the transcript. Sections retain insertion order;
/// `None` removes a previously declared section. This is not user content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    #[serde(
        deserialize_with = "deserialize_user_content",
        serialize_with = "serialize_system_content"
    )]
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "sections")]
    pub sections: Vec<(String, Option<String>)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<crate::ToolDeclaration>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_removed: Option<Vec<crate::ToolReference>>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

impl SystemMessage {
    pub fn text(text: impl Into<String>, timestamp_ms: i64) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            sections: Vec::new(),
            tools_added: None,
            tools_removed: None,
            timestamp_ms,
        }
    }

    /// Replay tool deltas in insertion order, with removal before addition.
    pub fn current_tools<'a>(
        messages: impl IntoIterator<Item = &'a Message>,
    ) -> Vec<crate::ToolDeclaration> {
        let mut tools: Vec<crate::ToolDeclaration> = Vec::new();
        for message in messages {
            let Message::System(message) = message else {
                continue;
            };
            for removed in message.tools_removed.iter().flatten() {
                tools.retain(|tool| tool.name != removed.name);
            }
            for added in message.tools_added.iter().flatten() {
                if let Some(existing) = tools.iter_mut().find(|tool| tool.name == added.name) {
                    *existing = added.clone();
                } else {
                    tools.push(added.clone());
                }
            }
        }
        tools
    }

    pub fn render(&self) -> String {
        std::iter::once(self.instruction_text())
            .chain(self.sections.iter().filter_map(|(_, text)| text.clone()))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Resolve append-only instruction text and ordered section replacements.
    pub fn fold(base: &str, messages: &[Message]) -> String {
        let mut content = vec![base.to_owned()];
        let mut sections: Vec<(String, String)> = Vec::new();
        for message in messages {
            let Message::System(message) = message else {
                continue;
            };
            content.push(message.instruction_text());
            for (name, value) in &message.sections {
                if let Some(index) = sections.iter().position(|(key, _)| key == name) {
                    if let Some(value) = value {
                        sections[index].1.clone_from(value);
                    } else {
                        sections.remove(index);
                    }
                } else if let Some(value) = value {
                    sections.push((name.clone(), value.clone()));
                }
            }
        }
        content.extend(sections.into_iter().map(|(_, value)| value));
        content
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn instruction_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn serialize_system_content<S: serde::Serializer>(
    content: &[ContentBlock],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match content {
        [] => serializer.serialize_str(""),
        [ContentBlock::Text(text)] if text.text_signature.is_none() => {
            serializer.serialize_str(&text.text)
        }
        blocks => blocks.serialize(serializer),
    }
}

mod sections {
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeMap};
    pub fn serialize<S: Serializer>(
        values: &[(String, Option<String>)],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (key, value) in values {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(String, Option<String>)>, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Vec<(String, Option<String>)>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("ordered system sections")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values: Self::Value = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Option<String>>()? {
                    if let Some((_, previous)) = values.iter_mut().find(|(name, _)| *name == key) {
                        *previous = value;
                    } else {
                        values.push((key, value));
                    }
                }
                Ok(values)
            }
        }
        // Missing sections use the field default; explicit null is also harmless.
        #[derive(Deserialize)]
        struct Ordered(#[serde(deserialize_with = "read")] Vec<(String, Option<String>)>);
        fn read<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<(String, Option<String>)>, D::Error> {
            d.deserialize_map(Visitor)
        }
        Ok(Option::<Ordered>::deserialize(deserializer)?
            .map(|v| v.0)
            .unwrap_or_default())
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

/// Extension-supplied custom message fields before the host assigns time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageInput {
    pub custom_type: String,
    #[serde(default)]
    pub content: CustomMessageContent,
    #[serde(default)]
    pub display: bool,
    #[serde(default)]
    pub details: Option<Value>,
}

impl CustomMessageInput {
    pub fn into_message(self, timestamp_ms: i64) -> CustomMessage {
        CustomMessage {
            custom_type: self.custom_type,
            content: self.content,
            display: self.display,
            details: self.details,
            timestamp_ms,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Requested Pi level, independent of the provider-specific mapped level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<crate::ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
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
    /// Execution time, absent when the tool was rejected before execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
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
