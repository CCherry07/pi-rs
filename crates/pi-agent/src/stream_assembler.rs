use pi_core::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, ResponseMetadata, StopReason,
    StreamEvent, TextContent, ThinkingContent, ToolCall, Usage,
};

#[derive(Debug, thiserror::Error)]
pub enum AssemblerError {
    #[error("stream must start before content events")]
    MissingStart,
    #[error("stream already started")]
    DuplicateStart,
    #[error("stream already finished")]
    AlreadyFinished,
    #[error("duplicate content block at index {0}")]
    DuplicateBlock(usize),
    #[error("unknown content block at index {0}")]
    UnknownBlock(usize),
    #[error("content block type mismatch at index {0}")]
    BlockTypeMismatch(usize),
    #[error("content block {0} is still open")]
    OpenBlock(usize),
    #[error("stream ended without Done")]
    MissingDone,
    #[error("content block index {0} is missing")]
    MissingBlock(usize),
    #[error("invalid tool arguments for {tool}: {message}")]
    InvalidToolArguments { tool: String, message: String },
}

#[derive(Debug, Clone)]
pub struct StreamUpdate {
    pub started: bool,
    pub message_event: Option<AssistantMessageEvent>,
}

#[derive(Debug, Clone)]
enum PartialBlock {
    Text {
        text: String,
        signature: Option<String>,
        ended: bool,
    },
    Thinking {
        text: String,
        signature: Option<String>,
        ended: bool,
    },
    ToolCall {
        id: pi_core::ToolCallId,
        name: String,
        raw_arguments: String,
        signature: Option<String>,
        ended: bool,
    },
}

pub struct StreamAssembler {
    metadata: Option<ResponseMetadata>,
    blocks: Vec<Option<PartialBlock>>,
    usage: Usage,
    stop_reason: Option<StopReason>,
    finished: bool,
}

impl Default for StreamAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAssembler {
    pub fn new() -> Self {
        Self {
            metadata: None,
            blocks: Vec::new(),
            usage: Usage::default(),
            stop_reason: None,
            finished: false,
        }
    }

    pub fn push(&mut self, event: StreamEvent) -> Result<StreamUpdate, AssemblerError> {
        if self.finished {
            return Err(AssemblerError::AlreadyFinished);
        }

        match event {
            StreamEvent::Start { metadata } => {
                if self.metadata.is_some() {
                    return Err(AssemblerError::DuplicateStart);
                }
                self.metadata = Some(metadata);
                Ok(StreamUpdate {
                    started: true,
                    message_event: Some(AssistantMessageEvent::Start),
                })
            }
            StreamEvent::TextStart { content_index } => {
                self.require_started()?;
                self.insert_block(
                    content_index,
                    PartialBlock::Text {
                        text: String::new(),
                        signature: None,
                        ended: false,
                    },
                )?;
                Ok(Self::update(AssistantMessageEvent::TextStart {
                    content_index,
                }))
            }
            StreamEvent::TextDelta {
                content_index,
                delta,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::Text { text, ended, .. } if !*ended => text.push_str(&delta),
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                }))
            }
            StreamEvent::TextEnd {
                content_index,
                text_signature,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::Text {
                        signature, ended, ..
                    } if !*ended => {
                        *signature = text_signature;
                        *ended = true;
                    }
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(AssistantMessageEvent::TextEnd {
                    content_index,
                }))
            }
            StreamEvent::ThinkingStart { content_index } => {
                self.require_started()?;
                self.insert_block(
                    content_index,
                    PartialBlock::Thinking {
                        text: String::new(),
                        signature: None,
                        ended: false,
                    },
                )?;
                Ok(Self::update(AssistantMessageEvent::ThinkingStart {
                    content_index,
                }))
            }
            StreamEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::Thinking { text, ended, .. } if !*ended => text.push_str(&delta),
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                }))
            }
            StreamEvent::ThinkingEnd {
                content_index,
                thinking_signature,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::Thinking {
                        signature, ended, ..
                    } if !*ended => {
                        *signature = thinking_signature;
                        *ended = true;
                    }
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                }))
            }
            StreamEvent::ToolCallStart {
                content_index,
                id,
                name,
            } => {
                self.require_started()?;
                self.insert_block(
                    content_index,
                    PartialBlock::ToolCall {
                        id,
                        name,
                        raw_arguments: String::new(),
                        signature: None,
                        ended: false,
                    },
                )?;
                Ok(Self::update(AssistantMessageEvent::ToolCallStart {
                    content_index,
                }))
            }
            StreamEvent::ToolCallDelta {
                content_index,
                arguments_delta,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::ToolCall {
                        raw_arguments,
                        ended,
                        ..
                    } if !*ended => raw_arguments.push_str(&arguments_delta),
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta: arguments_delta,
                }))
            }
            StreamEvent::ToolCallEnd {
                content_index,
                thought_signature,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::ToolCall {
                        signature, ended, ..
                    } if !*ended => {
                        *signature = thought_signature;
                        *ended = true;
                    }
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(AssistantMessageEvent::ToolCallEnd {
                    content_index,
                }))
            }
            StreamEvent::Done { reason, usage } => {
                self.require_started()?;
                for (index, block) in self.blocks.iter().enumerate() {
                    let Some(block) = block else {
                        return Err(AssemblerError::MissingBlock(index));
                    };
                    let ended = match block {
                        PartialBlock::Text { ended, .. }
                        | PartialBlock::Thinking { ended, .. }
                        | PartialBlock::ToolCall { ended, .. } => *ended,
                    };
                    if !ended {
                        return Err(AssemblerError::OpenBlock(index));
                    }
                }
                self.usage = usage;
                self.stop_reason = Some(reason);
                self.finished = true;
                Ok(StreamUpdate {
                    started: false,
                    message_event: None,
                })
            }
        }
    }

    pub fn snapshot(&self) -> Result<AssistantMessage, AssemblerError> {
        self.build_message(false)
    }

    pub fn finish(self) -> Result<AssistantMessage, AssemblerError> {
        self.build_message(true)
    }

    pub fn failure_message(
        &self,
        reason: StopReason,
        message: impl Into<String>,
    ) -> AssistantMessage {
        let metadata = self.metadata.clone().unwrap_or_else(|| {
            ResponseMetadata::new("unknown".into(), "unknown".into(), "unknown", 0)
        });
        AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new(""))],
            api: metadata.api,
            provider: metadata.provider,
            model: metadata.model,
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: reason,
            error_message: Some(message.into()),
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: metadata.timestamp_ms,
        }
    }

    fn update(event: AssistantMessageEvent) -> StreamUpdate {
        StreamUpdate {
            started: false,
            message_event: Some(event),
        }
    }

    fn require_started(&self) -> Result<(), AssemblerError> {
        if self.metadata.is_some() {
            Ok(())
        } else {
            Err(AssemblerError::MissingStart)
        }
    }

    fn insert_block(&mut self, index: usize, block: PartialBlock) -> Result<(), AssemblerError> {
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, || None);
        }
        if self.blocks[index].is_some() {
            return Err(AssemblerError::DuplicateBlock(index));
        }
        self.blocks[index] = Some(block);
        Ok(())
    }

    fn block_mut(&mut self, index: usize) -> Result<&mut PartialBlock, AssemblerError> {
        self.require_started()?;
        self.blocks
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(AssemblerError::UnknownBlock(index))
    }

    fn build_message(&self, require_finished: bool) -> Result<AssistantMessage, AssemblerError> {
        if require_finished && !self.finished {
            return Err(AssemblerError::MissingDone);
        }
        let metadata = self.metadata.clone().ok_or(AssemblerError::MissingStart)?;
        let mut content = Vec::with_capacity(self.blocks.len());
        for (index, block) in self.blocks.iter().enumerate() {
            let Some(block) = block else {
                return Err(AssemblerError::MissingBlock(index));
            };
            match block {
                PartialBlock::Text {
                    text,
                    signature,
                    ended,
                } => {
                    if require_finished && !ended {
                        return Err(AssemblerError::OpenBlock(index));
                    }
                    content.push(ContentBlock::Text(TextContent {
                        text: text.clone(),
                        text_signature: signature.clone(),
                    }));
                }
                PartialBlock::Thinking {
                    text,
                    signature,
                    ended,
                } => {
                    if require_finished && !ended {
                        return Err(AssemblerError::OpenBlock(index));
                    }
                    content.push(ContentBlock::Thinking(ThinkingContent {
                        thinking: text.clone(),
                        thinking_signature: signature.clone(),
                        redacted: None,
                    }));
                }
                PartialBlock::ToolCall {
                    id,
                    name,
                    raw_arguments,
                    signature,
                    ended,
                } => {
                    if require_finished && !ended {
                        return Err(AssemblerError::OpenBlock(index));
                    }
                    let arguments = if !ended && !require_finished {
                        // Tool arguments are commonly split in the middle of a
                        // JSON token. Partial snapshots remain observable without
                        // pretending incomplete bytes are valid arguments.
                        serde_json::Value::Null
                    } else if raw_arguments.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(raw_arguments).map_err(|error| {
                            AssemblerError::InvalidToolArguments {
                                tool: name.clone(),
                                message: error.to_string(),
                            }
                        })?
                    };
                    content.push(ContentBlock::ToolCall(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments,
                        thought_signature: signature.clone(),
                        namespace: None,
                    }));
                }
            }
        }

        Ok(AssistantMessage {
            content,
            api: metadata.api,
            provider: metadata.provider,
            model: metadata.model,
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: self.usage.clone(),
            stop_reason: self.stop_reason.unwrap_or(if require_finished {
                StopReason::Stop
            } else {
                StopReason::Pending
            }),
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: metadata.timestamp_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ModelId, ProviderId, ToolCallId};

    fn metadata() -> ResponseMetadata {
        ResponseMetadata::new(ProviderId::new("faux"), ModelId::new("test"), "faux", 1)
    }

    #[test]
    fn assembles_text_and_tool_call() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::TextStart { content_index: 0 })
            .unwrap();
        assembler
            .push(StreamEvent::TextDelta {
                content_index: 0,
                delta: "hello".to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::TextEnd {
                content_index: 0,
                text_signature: None,
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallStart {
                content_index: 1,
                id: ToolCallId::new("call-1"),
                name: "echo".to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallDelta {
                content_index: 1,
                arguments_delta: r#"{"text":"ok"}"#.to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallEnd {
                content_index: 1,
                thought_signature: None,
            })
            .unwrap();
        assembler
            .push(StreamEvent::Done {
                reason: StopReason::ToolUse,
                usage: Usage::default(),
            })
            .unwrap();

        let message = assembler.finish().unwrap();
        assert_eq!(message.content.len(), 2);
        assert_eq!(message.tool_calls()[0].arguments["text"], "ok");
    }

    #[test]
    fn rejects_delta_without_start() {
        let mut assembler = StreamAssembler::new();
        let error = assembler
            .push(StreamEvent::TextDelta {
                content_index: 0,
                delta: "bad".to_string(),
            })
            .unwrap_err();
        assert!(matches!(error, AssemblerError::MissingStart));
    }

    #[test]
    fn rejects_duplicate_start_and_events_after_done() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assert!(matches!(
            assembler
                .push(StreamEvent::Start {
                    metadata: metadata(),
                })
                .unwrap_err(),
            AssemblerError::DuplicateStart
        ));
        assembler
            .push(StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            })
            .unwrap();
        assert!(matches!(
            assembler
                .push(StreamEvent::TextStart { content_index: 0 })
                .unwrap_err(),
            AssemblerError::AlreadyFinished
        ));
    }

    #[test]
    fn partial_tool_argument_snapshot_does_not_parse_incomplete_json() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallStart {
                content_index: 0,
                id: ToolCallId::new("call-1"),
                name: "echo".to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallDelta {
                content_index: 0,
                arguments_delta: r#"{"te"#.to_string(),
            })
            .unwrap();

        let snapshot = assembler.snapshot().unwrap();
        assert!(snapshot.tool_calls()[0].arguments.is_null());
        assert_eq!(snapshot.stop_reason, StopReason::Pending);
    }

    #[test]
    fn rejects_invalid_tool_arguments_at_finish() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallStart {
                content_index: 0,
                id: ToolCallId::new("call-1"),
                name: "echo".to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallDelta {
                content_index: 0,
                arguments_delta: "{".to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallEnd {
                content_index: 0,
                thought_signature: None,
            })
            .unwrap();
        assembler
            .push(StreamEvent::Done {
                reason: StopReason::ToolUse,
                usage: Usage::default(),
            })
            .unwrap();
        assert!(matches!(
            assembler.finish().unwrap_err(),
            AssemblerError::InvalidToolArguments { .. }
        ));
    }

    #[test]
    fn done_does_not_emit_a_message_update() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        let update = assembler
            .push(StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            })
            .unwrap();
        assert!(update.message_event.is_none());
    }

    #[test]
    fn finish_requires_done() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assert!(matches!(
            assembler.finish().unwrap_err(),
            AssemblerError::MissingDone
        ));
    }
}
