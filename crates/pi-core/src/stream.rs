use serde_json::Value;

use crate::{DeferredHandle, ResponseMetadata, StopReason, ToolCallId, Usage};

/// Fields that a provider can discover after the stream has started.
///
/// Each `Some` value replaces the corresponding response field; omitted
/// values leave prior state unchanged. Providers should emit a patch as soon
/// as upstream supplies the value so failures retain all metadata observed
/// before the terminal event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseMetadataPatch {
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub diagnostics: Option<Vec<Value>>,
    pub deferred: Option<DeferredHandle>,
    pub raw_stop_reason: Option<String>,
    pub end_turn: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentMetadata {
    Thinking { redacted: Option<bool> },
    ToolCall { namespace: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Start {
        metadata: ResponseMetadata,
    },
    Metadata {
        patch: ResponseMetadataPatch,
    },
    ContentMetadata {
        content_index: usize,
        metadata: ContentMetadata,
    },
    TextStart {
        content_index: usize,
    },
    TextDelta {
        content_index: usize,
        delta: String,
    },
    TextEnd {
        content_index: usize,
        text_signature: Option<String>,
    },
    ThinkingStart {
        content_index: usize,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        content_index: usize,
        thinking_signature: Option<String>,
    },
    ToolCallStart {
        content_index: usize,
        id: ToolCallId,
        name: String,
    },
    ToolCallDelta {
        content_index: usize,
        arguments_delta: String,
    },
    ToolCallEnd {
        content_index: usize,
        thought_signature: Option<String>,
    },
    Done {
        reason: StopReason,
        usage: Usage,
    },
}
