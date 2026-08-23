#![forbid(unsafe_code)]

//! Shared OpenAI Responses wire projection and SSE stream adaptation.

use std::collections::HashMap;

use async_stream::stream;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, Message, ProviderError, ProviderId, ProviderStream,
    ResponseMetadata, StopReason, StreamEvent, ToolCallId, ToolSpec, Usage,
};
use pi_provider::{HttpBodyStream, SseDecoder, TransportError};
use serde_json::{Value, json};

/// Projects semantic messages into OpenAI Responses input items.
pub fn input_items(messages: &[Message]) -> Vec<Value> {
    messages.iter().flat_map(message_input_items).collect()
}

/// Projects semantic tools into OpenAI Responses function definitions.
pub fn tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": false
            })
        })
        .collect()
}

/// Adapts an accepted OpenAI Responses SSE body into the semantic provider stream.
pub fn stream(
    provider: ProviderId,
    model: pi_core::ModelId,
    api: impl Into<String>,
    mut body: HttpBodyStream,
    signal: AbortSignal,
) -> ProviderStream {
    let api = api.into();
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(provider, model, api, now_ms()),
        });
        let mut decoder = SseDecoder::new();
        let mut state = StreamState::default();
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
                        if event.data == "[DONE]" {
                            for event in state.finish() { yield Ok(event); }
                            return;
                        }
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
                        Ok(Some(event)) if event.data != "[DONE]" => match state.consume(&event.data) {
                            Ok(events) => for event in events { yield Ok(event); },
                            Err(error) => { yield Err(error); return; }
                        },
                        Ok(_) => {}
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    }
                    for event in state.finish() { yield Ok(event); }
                    return;
                }
            }
        }
    })
}

fn message_input_items(message: &Message) -> Vec<Value> {
    match message {
        Message::User(message) => vec![json!({
            "type": "message",
            "role": "user",
            "content": message.content.iter().filter_map(input_content).collect::<Vec<_>>()
        })],
        Message::Assistant(message) => {
            let mut items = Vec::new();
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                items.push(json!({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": text}]
                }));
            }
            items.extend(message.content.iter().filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(json!({
                    "type": "function_call",
                    "call_id": call.id.as_str(),
                    "name": call.name,
                    "arguments": call.arguments.to_string()
                })),
                _ => None,
            }));
            items
        }
        Message::ToolResult(message) => vec![json!({
            "type": "function_call_output",
            "call_id": message.tool_call_id.as_str(),
            "output": message.content.iter().filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                ContentBlock::Thinking(text) => Some(text.thinking.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n")
        })],
    }
}

fn input_content(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text(text) => Some(json!({"type": "input_text", "text": text.text})),
        ContentBlock::Image(image) => Some(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
        })),
        ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => None,
    }
}

#[derive(Default)]
struct StreamState {
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tools: HashMap<String, usize>,
    had_tool_call: bool,
    next_index: usize,
    closed: bool,
}

impl StreamState {
    fn consume(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|error| {
            ProviderError::Protocol(format!("invalid OpenAI Responses SSE JSON: {error}"))
        })?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut events = Vec::new();
        match kind {
            "response.output_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !delta.is_empty() {
                    let index = *self.text_index.get_or_insert_with(|| {
                        let index = self.next_index;
                        self.next_index += 1;
                        events.push(StreamEvent::TextStart {
                            content_index: index,
                        });
                        index
                    });
                    events.push(StreamEvent::TextDelta {
                        content_index: index,
                        delta: delta.to_string(),
                    });
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !delta.is_empty() {
                    let index = *self.thinking_index.get_or_insert_with(|| {
                        let index = self.next_index;
                        self.next_index += 1;
                        events.push(StreamEvent::ThinkingStart {
                            content_index: index,
                        });
                        index
                    });
                    events.push(StreamEvent::ThinkingDelta {
                        content_index: index,
                        delta: delta.to_string(),
                    });
                }
            }
            "response.output_item.added" => {
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let item_id = value
                        .pointer("/item/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let call_id = value
                        .pointer("/item/call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(item_id);
                    let name = value
                        .pointer("/item/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let index = self.next_index;
                    self.next_index += 1;
                    self.had_tool_call = true;
                    self.tools.insert(item_id.to_string(), index);
                    events.push(StreamEvent::ToolCallStart {
                        content_index: index,
                        id: ToolCallId::new(call_id),
                        name: name.to_string(),
                    });
                    if let Some(arguments) = value
                        .pointer("/item/arguments")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        events.push(StreamEvent::ToolCallDelta {
                            content_index: index,
                            arguments_delta: arguments.to_string(),
                        });
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(index) = self.tools.get(item_id).copied() {
                    let delta = value
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !delta.is_empty() {
                        events.push(StreamEvent::ToolCallDelta {
                            content_index: index,
                            arguments_delta: delta.to_string(),
                        });
                    }
                }
            }
            "response.output_item.done" => {
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let item_id = value
                        .pointer("/item/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if let Some(index) = self.tools.remove(item_id) {
                        events.push(StreamEvent::ToolCallEnd {
                            content_index: index,
                            thought_signature: None,
                        });
                    }
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                let reason = if kind == "response.incomplete" {
                    StopReason::Length
                } else if self.had_tool_call {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                };
                let usage = response_usage(value.get("response").unwrap_or(&Value::Null));
                events.extend(self.close());
                events.push(StreamEvent::Done { reason, usage });
            }
            "response.failed" | "error" => {
                let message = value
                    .pointer("/response/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Responses request failed");
                return Err(ProviderError::Failure(message.to_string()));
            }
            _ => {}
        }
        Ok(events)
    }

    fn close(&mut self) -> Vec<StreamEvent> {
        if self.closed {
            return Vec::new();
        }
        self.closed = true;
        let mut events = Vec::new();
        if let Some(index) = self.thinking_index {
            events.push(StreamEvent::ThinkingEnd {
                content_index: index,
                thinking_signature: None,
            });
        }
        if let Some(index) = self.text_index {
            events.push(StreamEvent::TextEnd {
                content_index: index,
                text_signature: None,
            });
        }
        for (_, index) in self.tools.drain() {
            events.push(StreamEvent::ToolCallEnd {
                content_index: index,
                thought_signature: None,
            });
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = self.close();
        if !events.is_empty() {
            events.push(StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            });
        }
        events
    }
}

fn response_usage(response: &Value) -> Usage {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let input_total = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Usage {
        input: input_total.saturating_sub(cache_read),
        output,
        cache_read,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(input_total + output),
        cost: Default::default(),
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
    use futures::stream;
    use pi_core::{AbortHandle, ModelId, UserMessage};

    #[test]
    fn projects_messages_and_tools_through_the_public_interface() {
        let input = input_items(&[Message::User(UserMessage::text("hello", 0))]);
        assert_eq!(input[0]["content"][0]["type"], "input_text");
    }

    #[tokio::test]
    async fn adapts_completed_sse_into_semantic_events() {
        let body: HttpBodyStream = Box::pin(stream::iter([Ok(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                .to_vec(),
        )]));
        let (_, signal) = AbortHandle::new();
        let events = stream(
            ProviderId::new("test"),
            ModelId::new("model"),
            "openai-responses",
            body,
            signal,
        )
        .collect::<Vec<_>>()
        .await;
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::TextDelta { delta, .. }) if delta == "hi")
        ));
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::Done { usage, .. }) if usage.total_tokens == 2)
        ));
    }
}
