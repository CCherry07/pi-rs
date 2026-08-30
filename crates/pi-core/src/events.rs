use std::sync::Arc;

use crate::{
    AssistantMessage, AssistantStream, Message, StreamEvent, ToolCallId, ToolResult,
    ToolResultMessage,
};

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: Arc<AssistantMessage>,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        stream: AssistantStream,
        update: Arc<StreamEvent>,
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
