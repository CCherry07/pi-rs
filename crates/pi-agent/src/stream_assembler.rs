use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use pi_core::{
    AssistantMessage, AssistantStream, AssistantStreamId, AssistantStreamView, ContentBlock,
    ContentMetadata, ResponseMetadata, StopReason, StreamEvent, TextContent, ThinkingContent,
    ToolCall, Usage,
};

static NEXT_ASSISTANT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

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
    pub update: Option<Arc<StreamEvent>>,
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
        redacted: Option<bool>,
        ended: bool,
    },
    ToolCall {
        id: pi_core::ToolCallId,
        name: String,
        raw_arguments: String,
        signature: Option<String>,
        namespace: Option<String>,
        ended: bool,
    },
}

struct StreamAssemblerState {
    metadata: Option<ResponseMetadata>,
    blocks: Vec<Option<PartialBlock>>,
    usage: Usage,
    stop_reason: Option<StopReason>,
    finished: bool,
}

pub struct StreamAssembler {
    state: Arc<RwLock<StreamAssemblerState>>,
    stream: AssistantStream,
}

struct StreamAssemblerView {
    state: Arc<RwLock<StreamAssemblerState>>,
}

impl AssistantStreamView for StreamAssemblerView {
    fn snapshot(&self) -> Option<AssistantMessage> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .build_message(false)
            .ok()
    }
}

impl Default for StreamAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAssembler {
    pub fn new() -> Self {
        let state = Arc::new(RwLock::new(StreamAssemblerState::new()));
        let stream = AssistantStream::new(
            AssistantStreamId::new(format!(
                "assistant-stream-{}",
                NEXT_ASSISTANT_STREAM_ID.fetch_add(1, Ordering::Relaxed)
            )),
            Arc::new(StreamAssemblerView {
                state: Arc::clone(&state),
            }),
        );
        Self { state, stream }
    }

    pub fn push(&mut self, event: StreamEvent) -> Result<StreamUpdate, AssemblerError> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event)
    }

    pub fn stream(&self) -> AssistantStream {
        self.stream.clone()
    }

    pub fn snapshot(&self) -> Result<AssistantMessage, AssemblerError> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .build_message(false)
    }

    pub fn finish(&self) -> Result<AssistantMessage, AssemblerError> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .build_message(true)
    }

    pub fn failure_message(
        &self,
        reason: StopReason,
        message: impl Into<String>,
    ) -> AssistantMessage {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure_message(reason, message.into())
    }
}

impl StreamAssemblerState {
    fn new() -> Self {
        Self {
            metadata: None,
            blocks: Vec::new(),
            usage: Usage::default(),
            stop_reason: None,
            finished: false,
        }
    }

    fn push(&mut self, event: StreamEvent) -> Result<StreamUpdate, AssemblerError> {
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
                    update: None,
                })
            }
            StreamEvent::Metadata { patch } => {
                let metadata = self.metadata.as_mut().ok_or(AssemblerError::MissingStart)?;
                if let Some(response_model) = &patch.response_model {
                    metadata.response_model = Some(response_model.clone());
                }
                if let Some(response_id) = &patch.response_id {
                    metadata.response_id = Some(response_id.clone());
                }
                if let Some(diagnostics) = &patch.diagnostics {
                    metadata.diagnostics = Some(diagnostics.clone());
                }
                if let Some(deferred) = &patch.deferred {
                    metadata.deferred = Some(deferred.clone());
                }
                if let Some(raw_stop_reason) = &patch.raw_stop_reason {
                    metadata.raw_stop_reason = Some(raw_stop_reason.clone());
                }
                if let Some(end_turn) = patch.end_turn {
                    metadata.end_turn = Some(end_turn);
                }
                Ok(StreamUpdate {
                    started: false,
                    update: Some(Arc::new(StreamEvent::Metadata { patch })),
                })
            }
            StreamEvent::ContentMetadata {
                content_index,
                metadata,
            } => {
                match (self.block_mut(content_index)?, &metadata) {
                    (
                        PartialBlock::Thinking { redacted, .. },
                        ContentMetadata::Thinking { redacted: value },
                    ) => *redacted = *value,
                    (
                        PartialBlock::ToolCall { namespace, .. },
                        ContentMetadata::ToolCall { namespace: value },
                    ) => *namespace = value.clone(),
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(StreamUpdate {
                    started: false,
                    update: Some(Arc::new(StreamEvent::ContentMetadata {
                        content_index,
                        metadata,
                    })),
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
                Ok(Self::update(StreamEvent::TextStart { content_index }))
            }
            StreamEvent::TextDelta {
                content_index,
                delta,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::Text { text, ended, .. } if !*ended => text.push_str(&delta),
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(StreamEvent::TextDelta {
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
                        *signature = text_signature.clone();
                        *ended = true;
                    }
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(StreamEvent::TextEnd {
                    content_index,
                    text_signature,
                }))
            }
            StreamEvent::ThinkingStart { content_index } => {
                self.require_started()?;
                self.insert_block(
                    content_index,
                    PartialBlock::Thinking {
                        text: String::new(),
                        signature: None,
                        redacted: None,
                        ended: false,
                    },
                )?;
                Ok(Self::update(StreamEvent::ThinkingStart { content_index }))
            }
            StreamEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                match self.block_mut(content_index)? {
                    PartialBlock::Thinking { text, ended, .. } if !*ended => text.push_str(&delta),
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(StreamEvent::ThinkingDelta {
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
                        *signature = thinking_signature.clone();
                        *ended = true;
                    }
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(StreamEvent::ThinkingEnd {
                    content_index,
                    thinking_signature,
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
                        id: id.clone(),
                        name: name.clone(),
                        raw_arguments: String::new(),
                        signature: None,
                        namespace: None,
                        ended: false,
                    },
                )?;
                Ok(Self::update(StreamEvent::ToolCallStart {
                    content_index,
                    id,
                    name,
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
                Ok(Self::update(StreamEvent::ToolCallDelta {
                    content_index,
                    arguments_delta,
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
                        *signature = thought_signature.clone();
                        *ended = true;
                    }
                    _ => return Err(AssemblerError::BlockTypeMismatch(content_index)),
                }
                Ok(Self::update(StreamEvent::ToolCallEnd {
                    content_index,
                    thought_signature,
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
                self.usage = usage.clone();
                self.stop_reason = Some(reason);
                self.finished = true;
                Ok(StreamUpdate {
                    started: false,
                    update: Some(Arc::new(StreamEvent::Done { reason, usage })),
                })
            }
        }
    }

    fn failure_message(&self, reason: StopReason, error_message: String) -> AssistantMessage {
        if let Ok(mut partial) = self.build_message(false) {
            partial.stop_reason = reason;
            partial.error_message = Some(error_message);
            return partial;
        }
        let metadata = self.metadata.clone().unwrap_or_else(|| {
            ResponseMetadata::new("unknown".into(), "unknown".into(), "unknown", 0)
        });
        AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new(""))],
            api: metadata.api,
            provider: metadata.provider,
            model: metadata.model,
            response_model: metadata.response_model,
            response_id: metadata.response_id,
            diagnostics: metadata.diagnostics,
            usage: Usage::default(),
            stop_reason: reason,
            error_message: Some(error_message),
            deferred: metadata.deferred,
            raw_stop_reason: metadata.raw_stop_reason,
            end_turn: metadata.end_turn,
            timestamp_ms: metadata.timestamp_ms,
        }
    }

    fn update(update: StreamEvent) -> StreamUpdate {
        StreamUpdate {
            started: false,
            update: Some(Arc::new(update)),
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
                    redacted,
                    ended,
                } => {
                    if require_finished && !ended {
                        return Err(AssemblerError::OpenBlock(index));
                    }
                    content.push(ContentBlock::Thinking(ThinkingContent {
                        thinking: text.clone(),
                        thinking_signature: signature.clone(),
                        redacted: *redacted,
                    }));
                }
                PartialBlock::ToolCall {
                    id,
                    name,
                    raw_arguments,
                    signature,
                    namespace,
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
                        namespace: namespace.clone(),
                    }));
                }
            }
        }

        Ok(AssistantMessage {
            content,
            api: metadata.api,
            provider: metadata.provider,
            model: metadata.model,
            response_model: metadata.response_model,
            response_id: metadata.response_id,
            diagnostics: metadata.diagnostics,
            usage: self.usage.clone(),
            stop_reason: self.stop_reason.unwrap_or(if require_finished {
                StopReason::Stop
            } else {
                StopReason::Pending
            }),
            error_message: None,
            deferred: metadata.deferred,
            raw_stop_reason: metadata.raw_stop_reason,
            end_turn: metadata.end_turn,
            timestamp_ms: metadata.timestamp_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{
        ContentMetadata, DeferredHandle, ModelId, ProviderId, ResponseMetadataPatch, ToolCallId,
    };

    fn metadata() -> ResponseMetadata {
        ResponseMetadata::new(
            ProviderId::new("scripted"),
            ModelId::new("test"),
            "scripted",
            1,
        )
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
    fn preserves_thinking_content_and_signature_before_text() {
        let mut assembler = StreamAssembler::new();
        for event in [
            StreamEvent::Start {
                metadata: metadata(),
            },
            StreamEvent::ThinkingStart { content_index: 0 },
            StreamEvent::ThinkingDelta {
                content_index: 0,
                delta: "step by step".to_string(),
            },
            StreamEvent::ThinkingEnd {
                content_index: 0,
                thinking_signature: Some("opaque".to_string()),
            },
            StreamEvent::TextStart { content_index: 1 },
            StreamEvent::TextDelta {
                content_index: 1,
                delta: "answer".to_string(),
            },
            StreamEvent::TextEnd {
                content_index: 1,
                text_signature: Some("text-signature".to_string()),
            },
            StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            },
        ] {
            assembler.push(event).unwrap();
        }

        let message = assembler.finish().unwrap();
        assert!(matches!(
            &message.content[0],
            ContentBlock::Thinking(thinking)
                if thinking.thinking == "step by step"
                    && thinking.thinking_signature.as_deref() == Some("opaque")
        ));
        assert!(matches!(
            &message.content[1],
            ContentBlock::Text(text)
                if text.text == "answer"
                    && text.text_signature.as_deref() == Some("text-signature")
        ));
    }

    #[test]
    fn preserves_response_and_content_metadata() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::Metadata {
                patch: ResponseMetadataPatch {
                    response_model: Some("resolved-model".to_string()),
                    response_id: Some("response-1".to_string()),
                    diagnostics: Some(vec![serde_json::json!({"code":"retry"})]),
                    deferred: Some(DeferredHandle {
                        provider: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        api: "scripted".to_string(),
                        id: "deferred-1".to_string(),
                        expires_at: Some(10),
                        poll_after_ms: Some(20),
                        data: Some(serde_json::json!({"cursor":"next"})),
                    }),
                    raw_stop_reason: Some("completed".to_string()),
                    end_turn: Some(true),
                },
            })
            .unwrap();
        assembler
            .push(StreamEvent::ThinkingStart { content_index: 0 })
            .unwrap();
        assembler
            .push(StreamEvent::ContentMetadata {
                content_index: 0,
                metadata: ContentMetadata::Thinking {
                    redacted: Some(true),
                },
            })
            .unwrap();
        assembler
            .push(StreamEvent::ThinkingDelta {
                content_index: 0,
                delta: "[Reasoning redacted]".to_string(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::ThinkingEnd {
                content_index: 0,
                thinking_signature: Some("opaque".to_string()),
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
            .push(StreamEvent::ContentMetadata {
                content_index: 1,
                metadata: ContentMetadata::ToolCall {
                    namespace: Some("dynamic".to_string()),
                },
            })
            .unwrap();
        assembler
            .push(StreamEvent::ToolCallDelta {
                content_index: 1,
                arguments_delta: "{}".to_string(),
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
                reason: StopReason::Deferred,
                usage: Usage::default(),
            })
            .unwrap();

        let message = assembler.finish().unwrap();
        assert_eq!(message.response_model.as_deref(), Some("resolved-model"));
        assert_eq!(message.response_id.as_deref(), Some("response-1"));
        assert_eq!(
            message.diagnostics,
            Some(vec![serde_json::json!({"code":"retry"})])
        );
        assert_eq!(message.deferred.as_ref().unwrap().id, "deferred-1");
        assert_eq!(message.raw_stop_reason.as_deref(), Some("completed"));
        assert_eq!(message.end_turn, Some(true));
        assert!(matches!(
            &message.content[0],
            ContentBlock::Thinking(thinking) if thinking.redacted == Some(true)
        ));
        assert_eq!(
            message.tool_calls()[0].namespace.as_deref(),
            Some("dynamic")
        );
    }

    #[test]
    fn failure_message_preserves_accumulated_partial_content_and_metadata() {
        let mut assembler = StreamAssembler::new();
        assembler
            .push(StreamEvent::Start {
                metadata: metadata(),
            })
            .unwrap();
        assembler
            .push(StreamEvent::Metadata {
                patch: ResponseMetadataPatch {
                    response_id: Some("response-before-failure".to_string()),
                    raw_stop_reason: Some("failed".to_string()),
                    ..ResponseMetadataPatch::default()
                },
            })
            .unwrap();
        assembler
            .push(StreamEvent::TextStart { content_index: 0 })
            .unwrap();
        assembler
            .push(StreamEvent::TextDelta {
                content_index: 0,
                delta: "kept partial".to_string(),
            })
            .unwrap();

        let message = assembler.failure_message(StopReason::Error, "network failed");
        assert!(matches!(
            &message.content[0],
            ContentBlock::Text(text) if text.text == "kept partial"
        ));
        assert_eq!(
            message.response_id.as_deref(),
            Some("response-before-failure")
        );
        assert_eq!(message.raw_stop_reason.as_deref(), Some("failed"));
        assert_eq!(message.error_message.as_deref(), Some("network failed"));
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
    fn done_is_forwarded_as_a_stream_update() {
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
        assert!(matches!(
            update.update.as_deref(),
            Some(StreamEvent::Done { .. })
        ));
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
