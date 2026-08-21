use crate::{AssistantMessage, Message, ToolCallId, ToolResult, ToolResultMessage};

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantMessageEvent {
    Start,
    TextStart { content_index: usize },
    TextDelta { content_index: usize, delta: String },
    TextEnd { content_index: usize },
    ThinkingStart { content_index: usize },
    ThinkingDelta { content_index: usize, delta: String },
    ThinkingEnd { content_index: usize },
    ToolCallStart { content_index: usize },
    ToolCallDelta { content_index: usize, delta: String },
    ToolCallEnd { content_index: usize },
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: AssistantMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        message: AssistantMessage,
        event: AssistantMessageEvent,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart {
        tool_call_id: ToolCallId,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: ToolCallId,
        tool_name: String,
        args: serde_json::Value,
        partial_result: ToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: ToolCallId,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
}
