use std::collections::BTreeMap;

use pi_core::{ProviderError, StopReason, StreamEvent, ToolCallId, Usage};
use serde_json::Value;

#[derive(Default)]
pub(crate) struct ChunkState {
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tools: BTreeMap<usize, usize>,
    next_index: usize,
    reason: Option<StopReason>,
    usage: Usage,
    closed: bool,
}

pub(crate) fn consume_json(
    state: &mut ChunkState,
    data: &str,
) -> Result<Vec<StreamEvent>, ProviderError> {
    let value = serde_json::from_str(data).map_err(|error| {
        ProviderError::Protocol(format!(
            "invalid SSE JSON: {error}; data={}",
            truncate(data, 500)
        ))
    })?;
    state.consume(&value)
}

impl ChunkState {
    fn consume(&mut self, chunk: &Value) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            self.usage = parse_usage(usage);
        }
        if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
            return Err(ProviderError::Failure(message.to_string()));
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(events);
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(value) = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
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
                delta: value.to_string(),
            });
        }
        if let Some(value) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
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
                delta: value.to_string(),
            });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let source = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if !self.tools.contains_key(&source) {
                    let index = self.next_index;
                    self.next_index += 1;
                    events.push(StreamEvent::ToolCallStart {
                        content_index: index,
                        id: ToolCallId::new(
                            call.get("id").and_then(Value::as_str).unwrap_or_default(),
                        ),
                        name: call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    });
                    self.tools.insert(source, index);
                }
                if let Some(value) = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                {
                    events.push(StreamEvent::ToolCallDelta {
                        content_index: self.tools[&source],
                        arguments_delta: value.to_string(),
                    });
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.reason = Some(match reason {
                "length" => StopReason::Length,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "content_filter" | "error" => StopReason::Error,
                _ => StopReason::Stop,
            });
            events.extend(self.close());
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
        for index in self.tools.values() {
            events.push(StreamEvent::ToolCallEnd {
                content_index: *index,
                thought_signature: None,
            });
        }
        events
    }

    pub(crate) fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = self.close();
        events.push(StreamEvent::Done {
            reason: self.reason.unwrap_or(StopReason::Stop),
            usage: self.usage.clone(),
        });
        events
    }
}

fn parse_usage(value: &Value) -> Usage {
    let prompt = value
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = value
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = value
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input = value
        .get("prompt_cache_miss_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| prompt.saturating_sub(cache_read));
    Usage {
        input,
        output,
        cache_read,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: value
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(prompt + output),
        cost: pi_core::UsageCost::default(),
    }
}

fn truncate(value: &str, max: usize) -> &str {
    value.get(..max).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_text_tools_and_cached_usage() {
        let mut state = ChunkState::default();
        let text = state
            .consume(&json!({"choices":[{"delta":{"content":"hi"}}]}))
            .unwrap();
        assert!(matches!(
            text[0],
            StreamEvent::TextStart { content_index: 0 }
        ));
        state.consume(&json!({"choices":[{"delta":{"tool_calls":[{"index":4,"id":"c1","function":{"name":"echo","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,"prompt_tokens_details":{"cached_tokens":3}}})).unwrap();
        let done = state.finish();
        assert!(
            matches!(&done[0], StreamEvent::Done { reason:StopReason::ToolUse, usage } if usage.input == 7)
        );
    }
}
