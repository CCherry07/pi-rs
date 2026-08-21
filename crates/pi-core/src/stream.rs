use crate::{ResponseMetadata, StopReason, ToolCallId, Usage};

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Start {
        metadata: ResponseMetadata,
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
