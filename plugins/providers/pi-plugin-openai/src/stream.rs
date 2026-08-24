use std::collections::BTreeMap;

use pi_core::{ProviderError, StopReason, StreamEvent, ToolCallId, Usage};
use serde_json::Value;

pub(crate) struct ChunkState {
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tools: BTreeMap<usize, usize>,
    next_index: usize,
    reason: Option<StopReason>,
    thinking_signature: Option<String>,
    usage: Usage,
    closed: bool,
    supports_finish_reason: bool,
    saw_finish_reason: bool,
}

impl Default for ChunkState {
    fn default() -> Self {
        Self::new(true)
    }
}

impl ChunkState {
    pub(crate) fn new(supports_finish_reason: bool) -> Self {
        Self {
            text_index: None,
            thinking_index: None,
            tools: BTreeMap::new(),
            next_index: 0,
            reason: None,
            thinking_signature: None,
            usage: Usage::default(),
            closed: false,
            supports_finish_reason,
            saw_finish_reason: false,
        }
    }
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
        let reasoning = ["reasoning_content", "reasoning", "reasoning_text"]
            .into_iter()
            .find_map(|field| {
                delta
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(|value| (field, value))
            });
        if let Some((field, value)) = reasoning {
            self.thinking_signature
                .get_or_insert_with(|| field.to_string());
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
            self.saw_finish_reason = true;
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
                thinking_signature: self.thinking_signature.take(),
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

    pub(crate) fn finish(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        if self.supports_finish_reason && !self.saw_finish_reason {
            return Err(ProviderError::Protocol(
                "stream ended without finish_reason".to_string(),
            ));
        }
        if self.reason.is_none() {
            self.reason = Some(if self.tools.is_empty() {
                StopReason::Stop
            } else {
                StopReason::ToolUse
            });
        }
        let mut events = self.close();
        events.push(StreamEvent::Done {
            reason: self.reason.unwrap_or(StopReason::Stop),
            usage: self.usage.clone(),
        });
        Ok(events)
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
        .or_else(|| value.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .or_else(|| value.get("cached_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let cache_write = value
        .pointer("/prompt_tokens_details/cache_write_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input = value
        .get("prompt_cache_miss_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            prompt
                .saturating_sub(cache_read)
                .saturating_sub(cache_write)
        });
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: value
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        total_tokens: input + output + cache_read + cache_write,
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
        let done = state.finish().unwrap();
        assert!(
            matches!(&done[0], StreamEvent::Done { reason:StopReason::ToolUse, usage } if usage.input == 7)
        );
    }

    #[test]
    fn missing_finish_reason_is_configurable_for_compatible_servers() {
        let mut strict = ChunkState::new(true);
        strict
            .consume(&json!({"choices":[{"delta":{"content":"hi"}}]}))
            .unwrap();
        assert!(strict.finish().is_err());

        let mut inferred = ChunkState::new(false);
        inferred
            .consume(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"echo","arguments":"{}"}}]}}]}))
            .unwrap();
        assert!(matches!(
            inferred.finish().unwrap().last(),
            Some(StreamEvent::Done {
                reason: StopReason::ToolUse,
                ..
            })
        ));
    }
}
