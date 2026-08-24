#![forbid(unsafe_code)]

//! Reusable Anthropic Messages wire projection and SSE stream adaptation.

use std::collections::{HashMap, HashSet};

use async_stream::stream;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, Message, ProviderError, ProviderId, ProviderRequest, ProviderStream,
    ResponseMetadata, StopReason, StreamEvent, ToolCallId, Usage,
};
use pi_provider::{HttpBodyStream, SseDecoder, TransportError};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnthropicMode {
    #[default]
    Standard,
    ClaudeCode,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AnthropicMessagesCompat {
    pub supports_eager_tool_input_streaming: Option<bool>,
    pub supports_long_cache_retention: Option<bool>,
    pub send_session_affinity_headers: Option<bool>,
    pub supports_cache_control_on_tools: Option<bool>,
    pub supports_temperature: Option<bool>,
    pub force_adaptive_thinking: Option<bool>,
    pub allow_empty_signature: Option<bool>,
    pub supports_strict_tools: Option<bool>,
    pub allowed_fallback_models: Option<Vec<String>>,
    pub supports_tool_references: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAnthropicMessagesCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub supports_cache_control_on_tools: bool,
    pub supports_temperature: bool,
    pub force_adaptive_thinking: bool,
    pub allow_empty_signature: bool,
    pub supports_tool_references: bool,
}

pub(crate) fn compatibility(request: &ProviderRequest) -> ResolvedAnthropicMessagesCompat {
    let configured = request
        .model_spec
        .as_ref()
        .and_then(|model| model.compat.clone())
        .and_then(|value| serde_json::from_value::<AnthropicMessagesCompat>(value).ok())
        .unwrap_or_default();
    let default_tool_references = request.model_spec.as_ref().is_some_and(|model| {
        if model.provider.as_str() != "anthropic" || model.id.as_str().contains("haiku") {
            return false;
        }
        let id = model.id.as_str();
        id.contains("-4-5") || id.contains("-4-6") || id.contains("-4-7") || id.contains("-5-")
    });
    ResolvedAnthropicMessagesCompat {
        supports_eager_tool_input_streaming: configured
            .supports_eager_tool_input_streaming
            .unwrap_or(true),
        supports_long_cache_retention: configured.supports_long_cache_retention.unwrap_or(true),
        send_session_affinity_headers: configured.send_session_affinity_headers.unwrap_or(false),
        supports_cache_control_on_tools: configured.supports_cache_control_on_tools.unwrap_or(true),
        supports_temperature: configured.supports_temperature.unwrap_or(true),
        force_adaptive_thinking: configured.force_adaptive_thinking.unwrap_or(false),
        allow_empty_signature: configured.allow_empty_signature.unwrap_or(false),
        supports_tool_references: configured
            .supports_tool_references
            .unwrap_or(default_tool_references),
    }
}

/// Projects a semantic request into the Anthropic Messages payload.
pub fn request_body(request: &ProviderRequest, mode: AnthropicMode) -> Value {
    let compat = compatibility(request);
    let deferred_names = if compat.supports_tool_references {
        request
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(message) => message.added_tool_names.as_ref(),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    let mut loaded_tools = HashSet::new();
    let mut messages = request
        .messages
        .iter()
        .flat_map(|message| {
            project_message(message, mode, &compat, &deferred_names, &mut loaded_tools)
        })
        .collect::<Vec<_>>();
    let mut tools = request
        .tools
        .iter()
        .filter(|tool| !deferred_names.contains(&tool.name))
        .map(|tool| {
            let mut value = json!({
                "name": outbound_tool_name(&tool.name, mode),
                "description": tool.description,
                "input_schema": tool.parameters
            });
            if compat.supports_eager_tool_input_streaming {
                value["eager_input_streaming"] = Value::Bool(true);
            }
            value
        })
        .collect::<Vec<_>>();
    tools.extend(
        request
            .tools
            .iter()
            .filter(|tool| deferred_names.contains(&tool.name))
            .map(|tool| {
                let mut value = json!({
                    "name": outbound_tool_name(&tool.name, mode),
                    "description": tool.description,
                    "input_schema": tool.parameters,
                    "defer_loading": true
                });
                if compat.supports_eager_tool_input_streaming {
                    value["eager_input_streaming"] = Value::Bool(true);
                }
                value
            }),
    );
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
        body["system"] = Value::Array(system.clone());
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.clone());
    }
    let reasoning_model = request.model_spec.as_ref().map_or(
        request.thinking_level != pi_core::ThinkingLevel::Off,
        |model| model.reasoning,
    );
    if reasoning_model && request.thinking_level != pi_core::ThinkingLevel::Off {
        if compat.force_adaptive_thinking {
            body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
            let effort = request
                .model_spec
                .as_ref()
                .and_then(|model| {
                    model
                        .thinking_level_map
                        .get(request.thinking_level.as_str())
                })
                .map_or_else(
                    || Some(request.thinking_level.as_str().to_string()),
                    Clone::clone,
                );
            if let Some(effort) = effort {
                body["output_config"] = json!({"effort": effort});
            }
        } else {
            let max_tokens = request.max_output_tokens.unwrap_or(16_384);
            let budget = thinking_budget(request.thinking_level)
                .min(max_tokens.saturating_sub(1_024))
                .max(1);
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget,
                "display": "summarized"
            });
        }
    } else if reasoning_model
        && request
            .model_spec
            .as_ref()
            .is_none_or(|model| model.thinking_level_map.get("off") != Some(&None))
    {
        body["thinking"] = json!({"type": "disabled"});
    }
    if compat.supports_temperature
        && request.thinking_level == pi_core::ThinkingLevel::Off
        && let Some(temperature) = request.sampling_params.get("temperature")
    {
        body["temperature"] = temperature.clone();
    }
    if cache_retention() != CacheRetention::None {
        let cache_control =
            if cache_retention() == CacheRetention::Long && compat.supports_long_cache_retention {
                json!({"type": "ephemeral", "ttl": "1h"})
            } else {
                json!({"type": "ephemeral"})
            };
        apply_cache_control(
            &mut system,
            &mut messages,
            &mut tools,
            &cache_control,
            compat.supports_cache_control_on_tools,
        );
        if !system.is_empty() {
            body["system"] = Value::Array(system);
        }
        body["messages"] = Value::Array(messages);
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
    }
    body
}

fn thinking_budget(level: pi_core::ThinkingLevel) -> u64 {
    match level {
        pi_core::ThinkingLevel::Minimal => 1_024,
        pi_core::ThinkingLevel::Low => 2_048,
        pi_core::ThinkingLevel::Medium => 8_192,
        pi_core::ThinkingLevel::High
        | pi_core::ThinkingLevel::XHigh
        | pi_core::ThinkingLevel::Max => 16_384,
        pi_core::ThinkingLevel::Off => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheRetention {
    None,
    Short,
    Long,
}

fn cache_retention() -> CacheRetention {
    match std::env::var("PI_CACHE_RETENTION").as_deref() {
        Ok("none") => CacheRetention::None,
        Ok("long") => CacheRetention::Long,
        _ => CacheRetention::Short,
    }
}

fn apply_cache_control(
    system: &mut [Value],
    messages: &mut [Value],
    tools: &mut [Value],
    cache_control: &Value,
    supports_cache_control_on_tools: bool,
) {
    for block in system {
        block["cache_control"] = cache_control.clone();
    }
    if supports_cache_control_on_tools && let Some(tool) = tools.last_mut() {
        tool["cache_control"] = cache_control.clone();
    }
    let Some(content) = messages
        .iter_mut()
        .rev()
        .find_map(|message| message.get_mut("content").and_then(Value::as_array_mut))
    else {
        return;
    };
    if let Some(block) = content.iter_mut().rev().find(|block| {
        matches!(
            block.get("type").and_then(Value::as_str),
            Some("text" | "tool_result")
        )
    }) {
        block["cache_control"] = cache_control.clone();
    }
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

fn project_message(
    message: &Message,
    mode: AnthropicMode,
    compat: &ResolvedAnthropicMessagesCompat,
    deferred_names: &HashSet<String>,
    loaded_tools: &mut HashSet<String>,
) -> Vec<Value> {
    match message {
        Message::User(message) => vec![json!({
            "role": "user",
            "content": message.content.iter().filter_map(project_user_block).collect::<Vec<_>>()
        })],
        Message::Assistant(message) => vec![json!({
            "role": "assistant",
            "content": message.content.iter().filter_map(|block| {
                project_assistant_block(block, mode, compat.allow_empty_signature)
            }).collect::<Vec<_>>()
        })],
        Message::ToolResult(message) => {
            let references = message
                .added_tool_names
                .iter()
                .flatten()
                .filter(|name| deferred_names.contains(*name))
                .filter(|name| loaded_tools.insert((*name).clone()))
                .map(|name| {
                    json!({
                        "type": "tool_reference",
                        "tool_name": outbound_tool_name(name, mode)
                    })
                })
                .collect::<Vec<_>>();
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    ContentBlock::Thinking(text) => Some(text.thinking.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let has_references = !references.is_empty();
            let content = if !has_references {
                Value::String(text.clone())
            } else {
                Value::Array(references)
            };
            let mut projected = vec![json!({
                "role": "user",
                "content": [{
                "type": "tool_result",
                "tool_use_id": message.tool_call_id.as_str(),
                "content": content,
                "is_error": message.is_error
            }]
            })];
            if !text.is_empty() && has_references {
                projected.push(json!({
                    "role": "user",
                    "content": [{"type": "text", "text": text}]
                }));
            }
            projected
        }
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

fn project_assistant_block(
    block: &ContentBlock,
    mode: AnthropicMode,
    allow_empty_signature: bool,
) -> Option<Value> {
    match block {
        ContentBlock::Text(text) => Some(json!({"type": "text", "text": text.text})),
        ContentBlock::Thinking(thinking) if thinking.redacted == Some(true) => Some(json!({
            "type": "redacted_thinking",
            "data": thinking.thinking_signature.clone().unwrap_or_default()
        })),
        ContentBlock::Thinking(thinking) => {
            let signature = thinking.thinking_signature.as_deref().unwrap_or_default();
            if signature.trim().is_empty() && !allow_empty_signature {
                (!thinking.thinking.trim().is_empty())
                    .then(|| json!({"type": "text", "text": thinking.thinking}))
            } else {
                Some(json!({
                    "type": "thinking", "thinking": thinking.thinking,
                    "signature": signature
                }))
            }
        }
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
        if let Some(value) = usage
            .pointer("/cache_creation/ephemeral_1h_input_tokens")
            .and_then(Value::as_u64)
        {
            self.usage.cache_write_1h = Some(value);
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
        let mut model = pi_core::ModelSpec::new(
            "anthropic",
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            "anthropic-messages",
        );
        model.reasoning = true;
        model.compat = Some(json!({"forceAdaptiveThinking": true}));
        let request = ProviderRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            model_spec: Some(model),
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
            session_id: None,
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
