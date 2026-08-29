use std::collections::HashMap;

use async_stream::stream;
use base64::Engine;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, ContentMetadata, Message, ModelCost, ModelSpec, ProviderError,
    ProviderId, ProviderRequest, ProviderStream, ResponseMetadata, ResponseMetadataPatch,
    StopReason, StreamEvent, ThinkingLevel, ToolCallId, Usage,
};
use serde_json::{Map, Value, json};

use crate::BEDROCK_CONVERSE_STREAM_API;
use crate::eventstream::{EventFrame, EventStreamDecoder};

const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";
const REDACTED_THINKING_PLACEHOLDER: &str = "[Reasoning redacted]";

pub fn request_body(request: &ProviderRequest) -> Value {
    let model = request.model_spec.as_ref();
    let mut body = Map::new();
    body.insert(
        "messages".to_string(),
        Value::Array(project_messages(request)),
    );
    if !request.system_prompt.is_empty() {
        let mut system = vec![json!({"text": request.system_prompt})];
        if model.is_some_and(supports_prompt_caching) {
            system.push(json!({"cachePoint": {"type": "default"}}));
        }
        body.insert("system".to_string(), Value::Array(system));
    }

    let mut inference = Map::new();
    let max_tokens = request.max_output_tokens.or_else(|| {
        model
            .filter(|model| is_anthropic_claude(model))
            .map(|model| model.max_tokens)
    });
    if let Some(max_tokens) = max_tokens {
        inference.insert("maxTokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = request.sampling_params.get("temperature") {
        inference.insert("temperature".to_string(), temperature.clone());
    }
    if let Some(top_p) = request
        .sampling_params
        .get("topP")
        .or_else(|| request.sampling_params.get("top_p"))
    {
        inference.insert("topP".to_string(), top_p.clone());
    }
    if let Some(stop_sequences) = request
        .sampling_params
        .get("stopSequences")
        .or_else(|| request.sampling_params.get("stop_sequences"))
    {
        inference.insert("stopSequences".to_string(), stop_sequences.clone());
    }
    if !inference.is_empty() {
        body.insert("inferenceConfig".to_string(), Value::Object(inference));
    }

    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "toolSpec": {
                        "name": tool.name,
                        "description": tool.description,
                        "inputSchema": {"json": sanitize_document(tool.parameters.clone())}
                    }
                })
            })
            .collect::<Vec<_>>();
        body.insert("toolConfig".to_string(), json!({"tools": tools}));
    }

    if let Some(fields) = additional_model_fields(request) {
        body.insert("additionalModelRequestFields".to_string(), fields);
    }
    if let Some(metadata) = request
        .sampling_params
        .get("requestMetadata")
        .or_else(|| request.sampling_params.get("request_metadata"))
    {
        body.insert("requestMetadata".to_string(), metadata.clone());
    }
    Value::Object(body)
}

fn project_messages(request: &ProviderRequest) -> Vec<Value> {
    let model = request.model_spec.as_ref();
    let mut messages = Vec::new();
    let mut pending_tool_results = Vec::new();
    for original in &request.messages {
        let message = original.clone().into_provider_message();
        if !matches!(message, Message::ToolResult(_)) && !pending_tool_results.is_empty() {
            push_message(
                &mut messages,
                "user",
                std::mem::take(&mut pending_tool_results),
            );
        }
        match message {
            Message::User(message) => {
                let mut content = project_user_content(&message.content);
                if content.is_empty() {
                    content.push(json!({"text": EMPTY_TEXT_PLACEHOLDER}));
                }
                push_message(&mut messages, "user", content);
            }
            Message::Assistant(message) => {
                let mut content = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text(text) if !text.text.trim().is_empty() => {
                            content.push(json!({"text": text.text}));
                        }
                        ContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                if let Some(signature) = thinking
                                    .thinking_signature
                                    .as_deref()
                                    .filter(|value| !value.trim().is_empty())
                                {
                                    content.push(json!({
                                        "reasoningContent": {"redactedContent": signature}
                                    }));
                                }
                            } else if !thinking.thinking.trim().is_empty() {
                                if model.is_some_and(is_anthropic_claude) {
                                    if let Some(signature) = thinking
                                        .thinking_signature
                                        .as_deref()
                                        .filter(|value| !value.trim().is_empty())
                                    {
                                        content.push(json!({
                                            "reasoningContent": {"reasoningText": {
                                                "text": thinking.thinking,
                                                "signature": signature
                                            }}
                                        }));
                                    } else {
                                        content.push(json!({"text": thinking.thinking}));
                                    }
                                } else {
                                    content.push(json!({
                                        "reasoningContent": {"reasoningText": {
                                            "text": thinking.thinking
                                        }}
                                    }));
                                }
                            }
                        }
                        ContentBlock::ToolCall(call) => {
                            content.push(json!({"toolUse": {
                                "toolUseId": normalize_tool_call_id(call.id.as_str()),
                                "name": call.name,
                                "input": sanitize_document(call.arguments.clone())
                            }}));
                        }
                        ContentBlock::Text(_) | ContentBlock::Image(_) => {}
                    }
                }
                if !content.is_empty() {
                    push_message(&mut messages, "assistant", content);
                }
            }
            Message::ToolResult(message) => {
                let mut content = project_user_content(&message.content);
                if content.is_empty() {
                    content.push(json!({"text": EMPTY_TEXT_PLACEHOLDER}));
                }
                pending_tool_results.push(json!({"toolResult": {
                    "toolUseId": normalize_tool_call_id(message.tool_call_id.as_str()),
                    "content": content,
                    "status": if message.is_error { "error" } else { "success" }
                }}));
            }
            Message::Custom(_) => unreachable!("custom messages project to user messages"),
        }
    }
    if !pending_tool_results.is_empty() {
        push_message(&mut messages, "user", pending_tool_results);
    }
    if model.is_some_and(supports_prompt_caching)
        && let Some(last_user) = messages
            .iter_mut()
            .rev()
            .find(|message| message["role"] == "user")
        && let Some(content) = last_user["content"].as_array_mut()
    {
        content.push(json!({"cachePoint": {"type": "default"}}));
    }
    messages
}

fn project_user_content(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) if !text.text.trim().is_empty() => {
                Some(json!({"text": text.text}))
            }
            ContentBlock::Image(image) => image_format(&image.mime_type).map(|format| {
                json!({"image": {
                    "format": format,
                    "source": {"bytes": image.data}
                }})
            }),
            _ => None,
        })
        .collect()
}

fn push_message(messages: &mut Vec<Value>, role: &str, mut content: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last["role"] == role
        && let Some(existing) = last["content"].as_array_mut()
    {
        existing.append(&mut content);
        return;
    }
    messages.push(json!({"role": role, "content": content}));
}

fn image_format(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/jpeg" | "image/jpg" => Some("jpeg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn sanitize_document(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize_document).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .filter(|(name, _)| !name.is_empty())
                .map(|(name, value)| (name, sanitize_document(value)))
                .collect(),
        ),
        value => value,
    }
}

fn additional_model_fields(request: &ProviderRequest) -> Option<Value> {
    let model = request.model_spec.as_ref()?;
    if !model.reasoning
        || request.thinking_level == ThinkingLevel::Off
        || !is_anthropic_claude(model)
    {
        return None;
    }
    if supports_adaptive_thinking(model) {
        let effort = model
            .thinking_level_map
            .get(request.thinking_level.as_str())
            .and_then(Clone::clone)
            .unwrap_or_else(|| match request.thinking_level {
                ThinkingLevel::Minimal | ThinkingLevel::Low => "low".to_string(),
                ThinkingLevel::Medium => "medium".to_string(),
                ThinkingLevel::High => "high".to_string(),
                ThinkingLevel::XHigh => "xhigh".to_string(),
                ThinkingLevel::Max => "max".to_string(),
                ThinkingLevel::Off => "low".to_string(),
            });
        return Some(json!({
            "thinking": {"type": "adaptive", "display": "summarized"},
            "output_config": {"effort": effort}
        }));
    }
    let budget = request
        .thinking_budgets
        .and_then(|budgets| budgets.for_level(request.thinking_level))
        .unwrap_or(match request.thinking_level {
            ThinkingLevel::Minimal => 1_024,
            ThinkingLevel::Low => 2_048,
            ThinkingLevel::Medium => 8_192,
            ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => 16_384,
            ThinkingLevel::Off => 0,
        });
    Some(json!({
        "thinking": {
            "type": "enabled",
            "budget_tokens": budget,
            "display": "summarized"
        },
        "anthropic_beta": ["interleaved-thinking-2025-05-14"]
    }))
}

fn is_anthropic_claude(model: &ModelSpec) -> bool {
    let id = model.id.as_str().to_ascii_lowercase();
    let name = model.name.to_ascii_lowercase();
    id.contains("anthropic.claude") || id.contains("anthropic/claude") || name.contains("claude")
}

fn supports_prompt_caching(model: &ModelSpec) -> bool {
    let candidate = format!("{} {}", model.id, model.name)
        .to_ascii_lowercase()
        .replace([' ', '_', '.', ':'], "-");
    candidate.contains("claude-3-5-haiku")
        || candidate.contains("claude-3-7-sonnet")
        || candidate.contains("claude-4-")
        || candidate.contains("claude-haiku-4")
        || candidate.contains("claude-opus-4")
        || candidate.contains("claude-sonnet-4")
        || candidate.contains("claude-fable-5")
        || candidate.contains("claude-opus-5")
        || candidate.contains("claude-sonnet-5")
}

fn supports_adaptive_thinking(model: &ModelSpec) -> bool {
    let candidate = format!("{} {}", model.id, model.name)
        .to_ascii_lowercase()
        .replace([' ', '_', '.', ':'], "-");
    [
        "opus-4-6",
        "opus-4-7",
        "opus-4-8",
        "opus-5",
        "sonnet-4-6",
        "sonnet-5",
        "fable-5",
    ]
    .iter()
    .any(|value| candidate.contains(value))
}

pub fn stream_response(
    provider: ProviderId,
    model: pi_core::ModelId,
    cost: ModelCost,
    mut body: pi_provider::HttpBodyStream,
    signal: AbortSignal,
) -> ProviderStream {
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(
                provider,
                model,
                BEDROCK_CONVERSE_STREAM_API,
                now_ms(),
            ),
        });
        let mut decoder = EventStreamDecoder::default();
        let mut state = BedrockStreamState::new(cost);
        loop {
            let next = tokio::select! {
                _ = signal.wait() => {
                    yield Err(ProviderError::Aborted);
                    return;
                }
                next = body.next() => next,
            };
            match next {
                Some(Ok(bytes)) => {
                    let frames = match decoder.push(&bytes) {
                        Ok(frames) => frames,
                        Err(error) => {
                            yield Err(ProviderError::Protocol(error));
                            return;
                        }
                    };
                    for frame in frames {
                        match state.consume(frame) {
                            Ok(events) => {
                                let done = events.iter().any(|event| matches!(event, StreamEvent::Done { .. }));
                                for event in events {
                                    yield Ok(event);
                                }
                                if done {
                                    return;
                                }
                            }
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        }
                    }
                }
                Some(Err(error)) => {
                    yield Err(map_transport_error(error));
                    return;
                }
                None => {
                    if let Err(error) = decoder.finish() {
                        yield Err(ProviderError::Protocol(error));
                        return;
                    }
                    match state.finish() {
                        Ok(events) => for event in events { yield Ok(event); },
                        Err(error) => yield Err(error),
                    }
                    return;
                }
            }
        }
    })
}

struct BedrockStreamState {
    blocks: HashMap<u64, StreamingBlock>,
    next_content_index: usize,
    stop_reason: Option<StopReason>,
    usage: Option<Usage>,
    cost: ModelCost,
    done: bool,
}

enum StreamingBlock {
    Text {
        content_index: usize,
    },
    Thinking {
        content_index: usize,
        signature: String,
        redacted: Vec<u8>,
        marked_redacted: bool,
    },
    Tool {
        content_index: usize,
    },
}

impl BedrockStreamState {
    fn new(cost: ModelCost) -> Self {
        Self {
            blocks: HashMap::new(),
            next_content_index: 0,
            stop_reason: None,
            usage: None,
            cost,
            done: false,
        }
    }

    fn consume(&mut self, frame: EventFrame) -> Result<Vec<StreamEvent>, ProviderError> {
        let message_type = frame
            .headers
            .get(":message-type")
            .map(String::as_str)
            .unwrap_or("event");
        if message_type != "event" {
            return Err(ProviderError::Failure(exception_message(&frame)));
        }
        let event_type = frame
            .headers
            .get(":event-type")
            .ok_or_else(|| ProviderError::Protocol("Bedrock event has no :event-type".to_string()))?
            .as_str();
        let value: Value = serde_json::from_slice(&frame.payload).map_err(|error| {
            ProviderError::Protocol(format!("invalid Bedrock {event_type} payload: {error}"))
        })?;
        let mut events = Vec::new();
        match event_type {
            "messageStart" => {
                if value
                    .get("role")
                    .and_then(Value::as_str)
                    .is_some_and(|role| role != "assistant")
                {
                    return Err(ProviderError::Protocol(
                        "Bedrock response started with a non-assistant role".to_string(),
                    ));
                }
            }
            "contentBlockStart" => self.content_block_start(&value, &mut events)?,
            "contentBlockDelta" => self.content_block_delta(&value, &mut events)?,
            "contentBlockStop" => self.content_block_stop(&value, &mut events)?,
            "messageStop" => {
                let raw = value
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                self.stop_reason = Some(map_stop_reason(raw)?);
                events.push(StreamEvent::Metadata {
                    patch: ResponseMetadataPatch {
                        raw_stop_reason: Some(raw.to_string()),
                        ..ResponseMetadataPatch::default()
                    },
                });
            }
            "metadata" => {
                self.usage = Some(parse_usage(&value));
            }
            exception if exception.ends_with("Exception") => {
                return Err(ProviderError::Failure(payload_error(exception, &value)));
            }
            _ => {}
        }
        if self.stop_reason.is_some() && self.usage.is_some() {
            events.extend(self.finish()?);
        }
        Ok(events)
    }

    fn content_block_start(
        &mut self,
        value: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let block_index = block_index(value)?;
        let Some(tool) = value.pointer("/start/toolUse") else {
            return Ok(());
        };
        let id = tool
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
        let content_index = self.allocate_content_index();
        self.blocks
            .insert(block_index, StreamingBlock::Tool { content_index });
        events.push(StreamEvent::ToolCallStart {
            content_index,
            id: ToolCallId::new(id),
            name: name.to_string(),
        });
        Ok(())
    }

    fn content_block_delta(
        &mut self,
        value: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let block_index = block_index(value)?;
        if let Some(text) = value.pointer("/delta/text").and_then(Value::as_str) {
            let content_index = self.ensure_text(block_index, events);
            events.push(StreamEvent::TextDelta {
                content_index,
                delta: text.to_string(),
            });
        }
        if let Some(input) = value
            .pointer("/delta/toolUse/input")
            .and_then(Value::as_str)
        {
            let content_index = match self.blocks.get(&block_index) {
                Some(StreamingBlock::Tool { content_index }) => *content_index,
                _ => {
                    return Err(ProviderError::Protocol(format!(
                        "Bedrock tool delta for unknown block {block_index}"
                    )));
                }
            };
            events.push(StreamEvent::ToolCallDelta {
                content_index,
                arguments_delta: input.to_string(),
            });
        }
        let reasoning = value.pointer("/delta/reasoningContent");
        if let Some(reasoning) = reasoning {
            let content_index = self.ensure_thinking(block_index, events);
            if let Some(text) = reasoning.get("text").and_then(Value::as_str) {
                events.push(StreamEvent::ThinkingDelta {
                    content_index,
                    delta: text.to_string(),
                });
            }
            if let Some(signature) = reasoning.get("signature").and_then(Value::as_str)
                && let Some(StreamingBlock::Thinking {
                    signature: current, ..
                }) = self.blocks.get_mut(&block_index)
            {
                current.push_str(signature);
            }
            if let Some(redacted) = reasoning.get("redactedContent").and_then(Value::as_str) {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(redacted)
                    .map_err(|error| {
                        ProviderError::Protocol(format!(
                            "invalid Bedrock redacted reasoning: {error}"
                        ))
                    })?;
                if let Some(StreamingBlock::Thinking {
                    redacted: current,
                    marked_redacted,
                    ..
                }) = self.blocks.get_mut(&block_index)
                {
                    if !*marked_redacted {
                        *marked_redacted = true;
                        events.push(StreamEvent::ContentMetadata {
                            content_index,
                            metadata: ContentMetadata::Thinking {
                                redacted: Some(true),
                            },
                        });
                        events.push(StreamEvent::ThinkingDelta {
                            content_index,
                            delta: REDACTED_THINKING_PLACEHOLDER.to_string(),
                        });
                    }
                    current.extend_from_slice(&decoded);
                }
            }
        }
        Ok(())
    }

    fn content_block_stop(
        &mut self,
        value: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let block_index = block_index(value)?;
        let Some(block) = self.blocks.remove(&block_index) else {
            return Ok(());
        };
        events.push(end_event(block));
        Ok(())
    }

    fn ensure_text(&mut self, block_index: u64, events: &mut Vec<StreamEvent>) -> usize {
        if let Some(StreamingBlock::Text { content_index }) = self.blocks.get(&block_index) {
            return *content_index;
        }
        let content_index = self.allocate_content_index();
        self.blocks
            .insert(block_index, StreamingBlock::Text { content_index });
        events.push(StreamEvent::TextStart { content_index });
        content_index
    }

    fn ensure_thinking(&mut self, block_index: u64, events: &mut Vec<StreamEvent>) -> usize {
        if let Some(StreamingBlock::Thinking { content_index, .. }) = self.blocks.get(&block_index)
        {
            return *content_index;
        }
        let content_index = self.allocate_content_index();
        self.blocks.insert(
            block_index,
            StreamingBlock::Thinking {
                content_index,
                signature: String::new(),
                redacted: Vec::new(),
                marked_redacted: false,
            },
        );
        events.push(StreamEvent::ThinkingStart { content_index });
        content_index
    }

    fn allocate_content_index(&mut self) -> usize {
        let index = self.next_content_index;
        self.next_content_index = self.next_content_index.saturating_add(1);
        index
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        if self.done {
            return Ok(Vec::new());
        }
        let reason = self.stop_reason.ok_or_else(|| {
            ProviderError::Protocol("Bedrock stream ended without a stop reason".to_string())
        })?;
        let mut events = self
            .blocks
            .drain()
            .map(|(_, block)| end_event(block))
            .collect::<Vec<_>>();
        let mut usage = self.usage.take().unwrap_or_default();
        usage.cost = self.cost.calculate(&usage);
        events.push(StreamEvent::Done { reason, usage });
        self.done = true;
        Ok(events)
    }
}

fn end_event(block: StreamingBlock) -> StreamEvent {
    match block {
        StreamingBlock::Text { content_index } => StreamEvent::TextEnd {
            content_index,
            text_signature: None,
        },
        StreamingBlock::Thinking {
            content_index,
            signature,
            redacted,
            ..
        } => {
            let signature = if redacted.is_empty() {
                (!signature.is_empty()).then_some(signature)
            } else {
                Some(base64::engine::general_purpose::STANDARD.encode(redacted))
            };
            StreamEvent::ThinkingEnd {
                content_index,
                thinking_signature: signature,
            }
        }
        StreamingBlock::Tool { content_index } => StreamEvent::ToolCallEnd {
            content_index,
            thought_signature: None,
        },
    }
}

fn block_index(value: &Value) -> Result<u64, ProviderError> {
    value
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ProviderError::Protocol("Bedrock content event has no block index".to_string())
        })
}

fn parse_usage(value: &Value) -> Usage {
    let usage = value.get("usage").unwrap_or(&Value::Null);
    let input = number(usage, "inputTokens");
    let output = number(usage, "outputTokens");
    Usage {
        input,
        output,
        cache_read: number(usage, "cacheReadInputTokens"),
        cache_write: number(usage, "cacheWriteInputTokens"),
        total_tokens: number(usage, "totalTokens").max(input.saturating_add(output)),
        ..Usage::default()
    }
}

fn number(value: &Value, name: &str) -> u64 {
    value.get(name).and_then(Value::as_u64).unwrap_or(0)
}

fn map_stop_reason(reason: &str) -> Result<StopReason, ProviderError> {
    match reason {
        "end_turn" | "stop_sequence" => Ok(StopReason::Stop),
        "max_tokens" | "model_context_window_exceeded" => Ok(StopReason::Length),
        "tool_use" => Ok(StopReason::ToolUse),
        other => Err(ProviderError::Failure(format!(
            "Bedrock stopped with unsupported reason {other:?}"
        ))),
    }
}

fn exception_message(frame: &EventFrame) -> String {
    let kind = frame
        .headers
        .get(":exception-type")
        .or_else(|| frame.headers.get(":error-code"))
        .map(String::as_str)
        .unwrap_or("Bedrock stream exception");
    let value = serde_json::from_slice::<Value>(&frame.payload).unwrap_or(Value::Null);
    payload_error(kind, &value)
}

fn payload_error(kind: &str, value: &Value) -> String {
    let message = value
        .get("message")
        .or_else(|| value.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("unknown Bedrock error");
    format!("{kind}: {message}")
}

fn map_transport_error(error: pi_provider::TransportError) -> ProviderError {
    match error {
        pi_provider::TransportError::Aborted => ProviderError::Aborted,
        pi_provider::TransportError::InvalidConfiguration(message)
        | pi_provider::TransportError::InvalidSse(message) => ProviderError::Protocol(message),
        error => ProviderError::Failure(error.to_string()),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pi_core::{ModelId, ThinkingBudgets, ToolSpec};

    use super::*;

    fn request() -> ProviderRequest {
        let mut model = ModelSpec::new(
            "amazon-bedrock",
            "anthropic.claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            BEDROCK_CONVERSE_STREAM_API,
        );
        model.reasoning = true;
        model.max_tokens = 64_000;
        model.compat = Some(json!({"supportsStrictMode": true}));
        ProviderRequest {
            model: ModelId::new("anthropic.claude-sonnet-4-6"),
            model_spec: Some(model),
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: vec![ToolSpec {
                name: "read".to_string(),
                label: "Read".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
                execution_mode: pi_core::ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            }],
            thinking_level: ThinkingLevel::High,
            thinking_budgets: Some(ThinkingBudgets {
                high: Some(12_000),
                ..ThinkingBudgets::default()
            }),
            max_output_tokens: Some(20_000),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::from([("temperature".to_string(), json!(0.2))]),
            session_id: None,
        }
    }

    #[test]
    fn builds_converse_request_with_tools_cache_and_adaptive_thinking() {
        let body = request_body(&request());
        assert_eq!(body["system"][0]["text"], "system");
        assert_eq!(body["system"][1]["cachePoint"]["type"], "default");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 20_000);
        assert_eq!(body["inferenceConfig"]["temperature"], 0.2);
        assert_eq!(body["toolConfig"]["tools"][0]["toolSpec"]["name"], "read");
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"]["type"],
            "adaptive"
        );
        assert_eq!(
            body["additionalModelRequestFields"]["output_config"]["effort"],
            "high"
        );
    }
}
