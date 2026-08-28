use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{AbortSignal, ContentBlock, ToolCallId, Usage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    pub execution_mode: ToolExecutionMode,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    pub details: Option<Value>,
    pub usage: Option<Usage>,
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    pub terminate: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(crate::TextContent::new(text))],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            terminate: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolUpdate {
    pub content: Vec<ContentBlock>,
    pub details: Option<Value>,
}

#[derive(Clone)]
pub struct ToolUpdateSink {
    sender: mpsc::UnboundedSender<ToolUpdate>,
}

impl ToolUpdateSink {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<ToolUpdate>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    pub fn send(&self, update: ToolUpdate) -> bool {
        self.sender.send(update).is_ok()
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool execution aborted")]
    Aborted,
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),
    #[error("tool failed: {0}")]
    Execution(String),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn prepare_arguments(&self, input: Value) -> Result<Value, ToolError> {
        Ok(input)
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        if input.is_object() {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments(
                "arguments must be a JSON object".to_string(),
            ))
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError>;
}
