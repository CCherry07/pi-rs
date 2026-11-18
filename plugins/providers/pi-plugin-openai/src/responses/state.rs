use pi_core::{
    ContentMetadata, ModelId, ResponseMetadataPatch, StopReason, StreamEvent, ToolCallId,
};
use pi_plugin::ProviderError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Thinking,
    Tool,
}
struct Slot {
    kind: Kind,
    text: String,
    pending_summary_separators: usize,
    signature: Option<Value>,
    ended: bool,
    done: bool,
    grammar: Option<(String, pi_provider::GrammarInputBuffer)>,
}
pub(super) struct StreamState {
    slots: Vec<Slot>,
    outputs: HashMap<u64, usize>,
    ids: HashMap<String, usize>,
    requested_model: String,
    terminal: bool,
    pending_error: Option<ProviderError>,
    grammar_properties: BTreeMap<String, String>,
}
impl StreamState {
    pub(super) fn new(model: &ModelId) -> Self {
        Self {
            slots: vec![],
            outputs: HashMap::new(),
            ids: HashMap::new(),
            requested_model: model.to_string(),
            terminal: false,
            pending_error: None,
            grammar_properties: BTreeMap::new(),
        }
    }
    pub(super) fn with_grammar(mut self, properties: BTreeMap<String, String>) -> Self {
        self.grammar_properties = properties;
        self
    }
    pub(super) fn take_pending_error(&mut self) -> Option<ProviderError> {
        self.pending_error.take()
    }
    fn slot(
        &mut self,
        value: &Value,
        kind: Kind,
        events: &mut Vec<StreamEvent>,
    ) -> Result<usize, ProviderError> {
        let item = value.get("item").unwrap_or(&Value::Null);
        let id = item["id"]
            .as_str()
            .or_else(|| value["item_id"].as_str())
            .unwrap_or_default();
        let output = value["output_index"].as_u64();
        let found = output
            .and_then(|i| self.outputs.get(&i).copied())
            .or_else(|| self.ids.get(id).copied())
            .or_else(|| {
                (output.is_none() && id.is_empty())
                    .then(|| self.slots.iter().rposition(|s| s.kind == kind && !s.done))
                    .flatten()
            });
        if let Some(index) = found {
            if self.slots[index].kind != kind {
                return Err(ProviderError::Protocol(
                    "Responses output index changed item type".into(),
                ));
            }
            if let Some(output) = output {
                self.outputs.insert(output, index);
            }
            if !id.is_empty() {
                self.ids.insert(id.into(), index);
            }
            return Ok(index);
        }
        if kind == Kind::Tool
            && (item["call_id"].as_str().is_none_or(str::is_empty)
                || item["name"].as_str().is_none_or(str::is_empty))
        {
            return Err(ProviderError::Protocol(
                "Responses tool item is missing call_id or name".into(),
            ));
        }
        let index = self.slots.len();
        if let Some(output) = output {
            self.outputs.insert(output, index);
        }
        if !id.is_empty() {
            self.ids.insert(id.into(), index);
        }
        self.slots.push(Slot {
            kind,
            text: String::new(),
            pending_summary_separators: 0,
            signature: None,
            ended: false,
            done: false,
            grammar: if item["type"] == "custom_tool_call" {
                let name = item["name"].as_str().unwrap_or_default();
                let property = self.grammar_properties.get(name).ok_or_else(|| {
                    ProviderError::Protocol(format!(
                        "Received custom tool call for unconfigured grammar tool {name}"
                    ))
                })?;
                Some((property.clone(), pi_provider::GrammarInputBuffer::default()))
            } else {
                None
            },
        });
        events.push(match kind {
            Kind::Text => StreamEvent::TextStart {
                content_index: index,
            },
            Kind::Thinking => StreamEvent::ThinkingStart {
                content_index: index,
            },
            Kind::Tool => {
                let call = item["call_id"].as_str().unwrap_or(id);
                let compound = if id.is_empty() {
                    call.into()
                } else {
                    format!("{call}|{id}")
                };
                StreamEvent::ToolCallStart {
                    content_index: index,
                    id: ToolCallId::new(compound),
                    name: item["name"].as_str().unwrap_or_default().into(),
                }
            }
        });
        Ok(index)
    }
    fn append(&mut self, index: usize, text: &str, events: &mut Vec<StreamEvent>) {
        if text.is_empty() || self.slots[index].ended {
            return;
        }
        let slot = &mut self.slots[index];
        slot.text.push_str(text);
        events.push(match slot.kind {
            Kind::Text => StreamEvent::TextDelta {
                content_index: index,
                delta: text.into(),
            },
            Kind::Thinking => StreamEvent::ThinkingDelta {
                content_index: index,
                delta: text.into(),
            },
            Kind::Tool => StreamEvent::ToolCallDelta {
                content_index: index,
                arguments_delta: text.into(),
            },
        });
    }
    fn final_text(
        &mut self,
        index: usize,
        text: &str,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        if text.is_empty() {
            return Ok(());
        }
        let previous = &self.slots[index].text;
        if let Some(tail) = text.strip_prefix(previous) {
            let tail = tail.to_owned();
            self.append(index, &tail, events);
            Ok(())
        } else {
            Err(ProviderError::Protocol(
                "Responses final content contradicts streamed deltas".into(),
            ))
        }
    }
    fn grammar_input(
        &mut self,
        index: usize,
        input: Option<&str>,
        done: bool,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let (property, buffer) = self.slots[index].grammar.as_mut().ok_or_else(|| {
            ProviderError::Protocol("Responses custom input references a non-grammar tool".into())
        })?;
        let delta = if done {
            buffer.close(property, input)
        } else {
            buffer.append(property, input.unwrap_or_default())
        }
        .map_err(ProviderError::Protocol)?;
        if let Some(delta) = delta {
            self.append(index, &delta, events);
        }
        Ok(())
    }
    fn end(&mut self, index: usize, events: &mut Vec<StreamEvent>) {
        let slot = &mut self.slots[index];
        if slot.ended {
            return;
        }
        slot.ended = true;
        events.push(match slot.kind {
            Kind::Text => StreamEvent::TextEnd {
                content_index: index,
                text_signature: slot.signature.as_ref().map(Value::to_string),
            },
            Kind::Thinking => StreamEvent::ThinkingEnd {
                content_index: index,
                thinking_signature: slot.signature.as_ref().map(Value::to_string),
            },
            Kind::Tool => StreamEvent::ToolCallEnd {
                content_index: index,
                thought_signature: None,
            },
        });
    }
    pub(super) fn consume(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::Protocol(format!("invalid OpenAI Responses SSE JSON: {e}"))
        })?;
        let kind = value["type"].as_str().unwrap_or_default();
        let mut events = Vec::new();
        if matches!(
            kind,
            "response.created"
                | "response.completed"
                | "response.done"
                | "response.incomplete"
                | "response.failed"
        ) {
            let response = &value["response"];
            let status = response["status"].as_str();
            let reason = response["incomplete_details"]["reason"].as_str();
            let patch = ResponseMetadataPatch {
                response_model: response["model"]
                    .as_str()
                    .filter(|m| *m != self.requested_model)
                    .map(str::to_owned),
                response_id: response["id"].as_str().map(str::to_owned),
                raw_stop_reason: if kind == "response.created" {
                    None
                } else {
                    match (status, reason) {
                        (Some(s), Some(r)) => Some(format!("{s}.{r}")),
                        (Some(s), None) => Some(s.into()),
                        (None, Some(r)) => Some(r.into()),
                        _ => None,
                    }
                },
                end_turn: response["end_turn"].as_bool(),
                ..Default::default()
            };
            if patch != ResponseMetadataPatch::default() {
                events.push(StreamEvent::Metadata { patch });
            }
        }
        match kind {
            "response.output_item.added" | "response.output_item.done" => {
                let item = &value["item"];
                let kind = match item["type"].as_str() {
                    Some("message") => Kind::Text,
                    Some("reasoning") => Kind::Thinking,
                    Some("function_call" | "custom_tool_call") => Kind::Tool,
                    _ => return Ok(events),
                };
                let index = self.slot(&value, kind, &mut events)?;
                if kind == Kind::Tool {
                    if let Some(namespace) = item["namespace"].as_str() {
                        events.push(StreamEvent::ContentMetadata {
                            content_index: index,
                            metadata: ContentMetadata::ToolCall {
                                namespace: Some(namespace.into()),
                            },
                        });
                    }
                    if item["type"] == "custom_tool_call" {
                        self.grammar_input(
                            index,
                            item["input"].as_str(),
                            value["type"] == "response.output_item.done",
                            &mut events,
                        )?;
                    } else if let Some(args) = item["arguments"].as_str() {
                        self.final_text(index, args, &mut events)?;
                    }
                }
                if value["type"] == "response.output_item.done" {
                    self.slots[index].pending_summary_separators = 0;
                    match kind {
                        Kind::Thinking => {
                            let text = joined(item, "summary", "text", "\n\n");
                            let text = if text.is_empty() {
                                joined(item, "content", "text", "\n\n")
                            } else {
                                text
                            };
                            self.final_text(index, &text, &mut events)?;
                            self.slots[index].signature = Some(item.clone());
                        }
                        Kind::Text => {
                            let text = item["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|c| {
                                    c["text"].as_str().or_else(|| c["refusal"].as_str())
                                })
                                .collect::<String>();
                            self.final_text(index, &text, &mut events)?;
                            if let Some(id) = item["id"].as_str() {
                                let mut signature = json!({"v":1,"id":id});
                                if let Some(phase) = item["phase"]
                                    .as_str()
                                    .filter(|p| matches!(*p, "commentary" | "final_answer"))
                                {
                                    signature["phase"] = json!(phase);
                                }
                                self.slots[index].signature = Some(signature);
                            }
                        }
                        Kind::Tool => {}
                    }
                    self.slots[index].done = true;
                    // Keep reasoning open until the terminal response can backfill encrypted_content.
                    if kind != Kind::Thinking {
                        self.end(index, &mut events);
                    }
                }
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta" => {
                let slot_kind = if kind.contains("reasoning") {
                    Kind::Thinking
                } else {
                    Kind::Text
                };
                let index = self.slot(&value, slot_kind, &mut events)?;
                let delta = value["delta"].as_str().unwrap_or_default();
                if !delta.is_empty() && slot_kind == Kind::Thinking {
                    let separators =
                        std::mem::take(&mut self.slots[index].pending_summary_separators);
                    self.append(index, &"\n\n".repeat(separators), &mut events);
                }
                self.append(index, delta, &mut events);
            }
            "response.reasoning_summary_part.done" => {
                let index = self.slot(&value, Kind::Thinking, &mut events)?;
                // Final summaries join parts without a trailing separator. Unlike Pi's
                // mutable partial message, semantic deltas cannot retract that suffix.
                // Defer separators until more reasoning arrives; final_text can also
                // supply them from the completed item when later deltas are absent.
                self.slots[index].pending_summary_separators += 1;
            }
            "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.delta"
            | "response.custom_tool_call_input.done" => {
                let index = value["output_index"]
                    .as_u64()
                    .and_then(|i| self.outputs.get(&i).copied())
                    .or_else(|| {
                        value["item_id"]
                            .as_str()
                            .and_then(|id| self.ids.get(id).copied())
                    });
                if let Some(index) = index {
                    if self.slots[index].kind != Kind::Tool {
                        return Err(ProviderError::Protocol(
                            "Responses tool delta references a non-tool item".into(),
                        ));
                    }
                    if kind.contains("custom_tool_call_input") {
                        self.grammar_input(
                            index,
                            value[if kind.ends_with(".done") {
                                "input"
                            } else {
                                "delta"
                            }]
                            .as_str(),
                            kind.ends_with(".done"),
                            &mut events,
                        )?;
                    } else if kind.ends_with(".done") {
                        self.final_text(
                            index,
                            value["arguments"].as_str().unwrap_or_default(),
                            &mut events,
                        )?;
                    } else {
                        self.append(
                            index,
                            value["delta"].as_str().unwrap_or_default(),
                            &mut events,
                        );
                    }
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                self.terminal = true;
                for item in value["response"]["output"].as_array().into_iter().flatten() {
                    if item["type"] == "reasoning"
                        && item["encrypted_content"].is_string()
                        && let Some(index) =
                            item["id"].as_str().and_then(|id| self.ids.get(id).copied())
                        && let Some(signature) = &mut self.slots[index].signature
                        && !signature["encrypted_content"].is_string()
                    {
                        signature["encrypted_content"] = item["encrypted_content"].clone();
                    }
                }
                if self
                    .slots
                    .iter()
                    .any(|slot| slot.kind == Kind::Tool && !slot.done)
                {
                    self.pending_error = Some(ProviderError::Protocol(
                        "Responses stream completed with an unfinished tool call".into(),
                    ));
                    return Ok(events);
                }
                let mut reason = if self.slots.iter().any(|s| s.kind == Kind::Tool) {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                };
                if kind == "response.incomplete" {
                    if value["response"]["incomplete_details"]["reason"] == "max_output_tokens" {
                        reason = StopReason::Length;
                    } else {
                        self.pending_error = Some(ProviderError::Failure(
                            "Responses incomplete without max_output_tokens reason".into(),
                        ));
                        return Ok(events);
                    }
                }
                for index in 0..self.slots.len() {
                    self.end(index, &mut events);
                }
                events.push(StreamEvent::Done {
                    reason,
                    usage: super::response_usage(&value["response"]),
                });
            }
            "response.failed" | "error" => {
                self.terminal = true;
                let message = value
                    .pointer("/response/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Responses request failed");
                self.pending_error = Some(ProviderError::Failure(message.into()));
            }
            _ => {}
        }
        Ok(events)
    }
    pub(super) fn finish(&mut self) -> Vec<StreamEvent> {
        if !self.terminal {
            self.pending_error = Some(ProviderError::Protocol(
                "OpenAI Responses stream ended before a terminal response event".into(),
            ));
        }
        Vec::new()
    }
}
fn joined(item: &Value, key: &str, field: &str, separator: &str) -> String {
    item[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v[field].as_str())
        .collect::<Vec<_>>()
        .join(separator)
}
