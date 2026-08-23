#![forbid(unsafe_code)]

//! Reusable Anthropic Messages wire projection and SSE stream adaptation.

use std::collections::HashMap;

use async_stream::stream;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, Message, ProviderError, ProviderId, ProviderRequest, ProviderStream,
    ResponseMetadata, StopReason, StreamEvent, ToolCallId, Usage,
};
use pi_provider::{HttpBodyStream, SseDecoder, TransportError};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnthropicMode {
    #[default]
    Standard,
    ClaudeCode,
}

/// Projects a semantic request into the Anthropic Messages payload.
pub fn request_body(request: &ProviderRequest, mode: AnthropicMode) -> Value {
    let messages = request
        .messages
        .iter()
        .map(|message| project_message(message, mode))
        .collect::<Vec<_>>();
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": outbound_tool_name(&tool.name, mode),
                "description": tool.description,
                "input_schema": tool.parameters
            })
        })
        .collect::<Vec<_>>();
    let mut system = Vec::new();
    if mode == AnthropicMode::ClaudeCode {
        system.push(json!({
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude."
        }));
    }
    if !request.system_prompt.is_empty() {
        system.push(json!({"type": "text", "text": request.system_prompt}));
    }
    let mut body = json!({
        "model": request.model.as_str(),
        "messages": messages,
        "max_tokens": request.max_output_tokens.unwrap_or(16_384),
        "stream": true
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if request.thinking_level != pi_core::ThinkingLevel::Off {
        body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
        body["output_config"] = json!({"effort": request.thinking_level.as_str()});
    }
    if let Value::Object(body) = &mut body {
        body.extend(request.sampling_params.clone());
    }
    body
}

/// Adapts an accepted Anthropic Messages SSE body into a semantic provider stream.
pub fn stream(
    provider: ProviderId,
    model: pi_core::ModelId,
    api: impl Into<String>,
    mut body: HttpBodyStream,
    signal: AbortSignal,
    mode: AnthropicMode,
) -> ProviderStream {
    let api = api.into();
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(provider, model, api, now_ms()),
        });
        let mut decoder = SseDecoder::new();
        let mut state = StreamState::new(mode);
        loop {
            let next = tokio::select! {
                _ = signal.wait() => { yield Err(ProviderError::Aborted); return; }
                next = body.next() => next,
            };
            match next {
                Some(Ok(bytes)) => {
                    let events = match decoder.push(&bytes) {
                        Ok(events) => events,
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    };
                    for event in events {
                        match state.consume(&event.data) {
                            Ok(events) => {
                                let done = events.iter().any(|event| matches!(event, StreamEvent::Done { .. }));
                                for event in events { yield Ok(event); }
                                if done { return; }
                            }
                            Err(error) => { yield Err(error); return; }
                        }
                    }
                }
                Some(Err(error)) => { yield Err(map_transport_error(error)); return; }
                None => {
                    match decoder.finish() {
                        Ok(Some(event)) => match state.consume(&event.data) {
                            Ok(events) => for event in events { yield Ok(event); },
                            Err(error) => { yield Err(error); return; }
                        },
                        Ok(None) => {}
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    }
                    for event in state.finish() { yield Ok(event); }
                    return;
                }
            }
        }
    })
}

fn project_message(message: &Message, mode: AnthropicMode) -> Value {
    match message {
        Message::User(message) => json!({
            "role": "user",
            "content": message.content.iter().filter_map(project_user_block).collect::<Vec<_>>()
        }),
        Message::Assistant(message) => json!({
            "role": "assistant",
            "content": message.content.iter().filter_map(|block| project_assistant_block(block, mode)).collect::<Vec<_>>()
        }),
        Message::ToolResult(message) => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": message.tool_call_id.as_str(),
                "content": message.content.iter().filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    ContentBlock::Thinking(text) => Some(text.thinking.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n"),
                "is_error": message.is_error
            }]
        }),
    }
}

fn project_user_block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text(text) => Some(json!({"type": "text", "text": text.text})),
        ContentBlock::Image(image) => Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": image.mime_type, "data": image.data}
        })),
        _ => None,
    }
}

fn project_assistant_block(block: &ContentBlock, mode: AnthropicMode) -> Option<Value> {
    match block {
        ContentBlock::Text(text) => Some(json!({"type": "text", "text": text.text})),
        ContentBlock::Thinking(thinking) if thinking.redacted == Some(true) => Some(json!({
            "type": "redacted_thinking",
            "data": thinking.thinking_signature.clone().unwrap_or_default()
        })),
        ContentBlock::Thinking(thinking) => Some(json!({
            "type": "thinking", "thinking": thinking.thinking,
            "signature": thinking.thinking_signature.clone().unwrap_or_default()
        })),
        ContentBlock::ToolCall(call) => Some(json!({
            "type": "tool_use", "id": call.id.as_str(),
            "name": outbound_tool_name(&call.name, mode), "input": call.arguments
        })),
        ContentBlock::Image(_) => None,
    }
}

#[derive(Default)]
struct BlockState {
    content_index: usize,
    kind: BlockKind,
    signature: String,
}

#[derive(Default, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
    #[default]
    Unknown,
}

struct StreamState {
    blocks: HashMap<u64, BlockState>,
    next_index: usize,
    usage: Usage,
    reason: Option<StopReason>,
    closed: bool,
    mode: AnthropicMode,
}

impl StreamState {
    fn new(mode: AnthropicMode) -> Self {
        Self {
            blocks: HashMap::new(),
            next_index: 0,
            usage: Usage::default(),
            reason: None,
            closed: false,
            mode,
        }
    }

    fn consume(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|error| {
            ProviderError::Protocol(format!("invalid Anthropic SSE JSON: {error}"))
        })?;
        let mut events = Vec::new();
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "message_start" => self.update_usage(value.pointer("/message/usage")),
            "content_block_start" => {
                let wire_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = value.get("content_block").unwrap_or(&Value::Null);
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let index = self.next_index;
                self.next_index += 1;
                let kind = match kind {
                    "text" => {
                        events.push(StreamEvent::TextStart {
                            content_index: index,
                        });
                        BlockKind::Text
                    }
                    "thinking" | "redacted_thinking" => {
                        events.push(StreamEvent::ThinkingStart {
                            content_index: index,
                        });
                        BlockKind::Thinking
                    }
                    "tool_use" => {
                        events.push(StreamEvent::ToolCallStart {
                            content_index: index,
                            id: ToolCallId::new(
                                block.get("id").and_then(Value::as_str).unwrap_or_default(),
                            ),
                            name: inbound_tool_name(
                                block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                                self.mode,
                            ),
                        });
                        BlockKind::Tool
                    }
                    _ => BlockKind::Unknown,
                };
                if kind != BlockKind::Unknown {
                    let signature =
                        if block.get("type").and_then(Value::as_str) == Some("redacted_thinking") {
                            block
                                .get("data")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        } else {
                            block
                                .get("signature")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        };
                    self.blocks.insert(
                        wire_index,
                        BlockState {
                            content_index: index,
                            kind,
                            signature,
                        },
                    );
                }
            }
            "content_block_delta" => {
                let wire_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(block) = self.blocks.get_mut(&wire_index) {
                    let delta = value.get("delta").unwrap_or(&Value::Null);
                    match block.kind {
                        BlockKind::Text => {
                            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                events.push(StreamEvent::TextDelta {
                                    content_index: block.content_index,
                                    delta: text.to_string(),
                                });
                            }
                        }
                        BlockKind::Thinking => {
                            if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                                events.push(StreamEvent::ThinkingDelta {
                                    content_index: block.content_index,
                                    delta: text.to_string(),
                                });
                            }
                            if let Some(signature) = delta.get("signature").and_then(Value::as_str)
                            {
                                block.signature.push_str(signature);
                            }
                        }
                        BlockKind::Tool => {
                            if let Some(json) = delta.get("partial_json").and_then(Value::as_str) {
                                events.push(StreamEvent::ToolCallDelta {
                                    content_index: block.content_index,
                                    arguments_delta: json.to_string(),
                                });
                            }
                        }
                        BlockKind::Unknown => {}
                    }
                }
            }
            "content_block_stop" => {
                let wire_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(block) = self.blocks.remove(&wire_index) {
                    match block.kind {
                        BlockKind::Text => events.push(StreamEvent::TextEnd {
                            content_index: block.content_index,
                            text_signature: None,
                        }),
                        BlockKind::Thinking => events.push(StreamEvent::ThinkingEnd {
                            content_index: block.content_index,
                            thinking_signature: (!block.signature.is_empty())
                                .then_some(block.signature),
                        }),
                        BlockKind::Tool => events.push(StreamEvent::ToolCallEnd {
                            content_index: block.content_index,
                            thought_signature: None,
                        }),
                        BlockKind::Unknown => {}
                    }
                }
            }
            "message_delta" => {
                self.update_usage(value.get("usage"));
                self.reason = value
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(map_stop_reason);
            }
            "message_stop" => {
                self.closed = true;
                events.push(StreamEvent::Done {
                    reason: self.reason.unwrap_or(StopReason::Stop),
                    usage: self.usage.clone(),
                });
            }
            "error" => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic request failed");
                return Err(ProviderError::Failure(message.to_string()));
            }
            _ => {}
        }
        Ok(events)
    }

    fn update_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        if let Some(value) = usage.get("input_tokens").and_then(Value::as_u64) {
            self.usage.input = value;
        }
        if let Some(value) = usage.get("output_tokens").and_then(Value::as_u64) {
            self.usage.output = value;
        }
        if let Some(value) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
            self.usage.cache_read = value;
        }
        if let Some(value) = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
        {
            self.usage.cache_write = value;
        }
        self.usage.reasoning = usage
            .pointer("/output_tokens_details/thinking_tokens")
            .and_then(Value::as_u64);
        self.usage.total_tokens =
            self.usage.input + self.usage.output + self.usage.cache_read + self.usage.cache_write;
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.closed {
            return Vec::new();
        }
        self.closed = true;
        vec![StreamEvent::Done {
            reason: self.reason.unwrap_or(StopReason::Stop),
            usage: self.usage.clone(),
        }]
    }
}

fn outbound_tool_name(name: &str, mode: AnthropicMode) -> String {
    if mode != AnthropicMode::ClaudeCode {
        return name.to_string();
    }
    match name.to_ascii_lowercase().as_str() {
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "bash" => "Bash",
        "grep" => "Grep",
        "find" | "glob" => "Glob",
        _ => name,
    }
    .to_string()
}

fn inbound_tool_name(name: &str, mode: AnthropicMode) -> String {
    if mode != AnthropicMode::ClaudeCode {
        return name.to_string();
    }
    match name.to_ascii_lowercase().as_str() {
        "read" => "read",
        "write" => "write",
        "edit" => "edit",
        "bash" => "bash",
        "grep" => "grep",
        "glob" => "find",
        _ => name,
    }
    .to_string()
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" | "model_context_window_exceeded" => StopReason::Length,
        _ => StopReason::Stop,
    }
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        TransportError::InvalidConfiguration(message) | TransportError::InvalidSse(message) => {
            ProviderError::Protocol(message)
        }
        other => ProviderError::Failure(other.to_string()),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ModelId, ThinkingLevel, ToolExecutionMode, ToolSpec, UserMessage};

    #[test]
    fn claude_code_mode_adds_identity_and_maps_tool_names() {
        let request = ProviderRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            system_prompt: "system".to_string(),
            messages: vec![Message::User(UserMessage::text("hello", 0))],
            tools: vec![ToolSpec {
                name: "read".to_string(),
                label: "Read".to_string(),
                description: "read".to_string(),
                parameters: json!({"type": "object"}),
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            }],
            thinking_level: ThinkingLevel::High,
            max_output_tokens: Some(100),
            headers: Default::default(),
            sampling_params: Default::default(),
        };
        let body = request_body(&request, AnthropicMode::ClaudeCode);
        assert_eq!(
            body["system"][0]["text"],
            "You are Claude Code, Anthropic's official CLI for Claude."
        );
        assert_eq!(body["tools"][0]["name"], "Read");
        assert_eq!(body["output_config"]["effort"], "high");
    }
}
