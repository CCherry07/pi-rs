#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError,
    ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink,
};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct TestToolsPlugin {
    completions: Arc<Mutex<Vec<String>>>,
}

impl Default for TestToolsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TestToolsPlugin {
    pub fn new() -> Self {
        Self {
            completions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn completions(&self) -> Vec<String> {
        self.completions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl AgentPlugin for TestToolsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("test-tools")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(EchoTool))?;
        context.register_tool(Arc::new(DelayTool {
            completions: Arc::clone(&self.completions),
            mode: ToolExecutionMode::Parallel,
            name: "delay",
        }))?;
        context.register_tool(Arc::new(DelayTool {
            completions: Arc::clone(&self.completions),
            mode: ToolExecutionMode::Sequential,
            name: "sequential_delay",
        }))?;
        context.register_tool(Arc::new(FailTool))?;
        context.register_tool(Arc::new(UpdateTool))?;
        context.register_tool(Arc::new(WaitForAbortTool))?;
        Ok(())
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        spec("echo", ToolExecutionMode::Parallel)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context
            .abort_signal
            .check()
            .map_err(|_| ToolError::Aborted)?;
        Ok(ToolResult::text(
            input
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ))
    }
}

struct DelayTool {
    completions: Arc<Mutex<Vec<String>>>,
    mode: ToolExecutionMode,
    name: &'static str,
}

#[async_trait]
impl Tool for DelayTool {
    fn spec(&self) -> ToolSpec {
        spec(self.name, self.mode)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let delay_ms = input.get("delayMs").and_then(Value::as_u64).unwrap_or(0);
        tokio::select! {
            biased;
            () = context.abort_signal.wait() => return Err(ToolError::Aborted),
            () = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
        }
        let value = input
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.completions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(value.clone());
        Ok(ToolResult::text(value))
    }
}

struct FailTool;

#[async_trait]
impl Tool for FailTool {
    fn spec(&self) -> ToolSpec {
        spec("fail", ToolExecutionMode::Parallel)
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::Execution("intentional failure".to_string()))
    }
}

struct UpdateTool;

#[async_trait]
impl Tool for UpdateTool {
    fn spec(&self) -> ToolSpec {
        spec("update", ToolExecutionMode::Parallel)
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let _ = updates.send(pi_core::ToolUpdate {
            content: vec![pi_core::ContentBlock::Text(pi_core::TextContent::new(
                "partial",
            ))],
            details: None,
        });
        Ok(ToolResult::text("final"))
    }
}

struct WaitForAbortTool;

#[async_trait]
impl Tool for WaitForAbortTool {
    fn spec(&self) -> ToolSpec {
        spec("wait_for_abort", ToolExecutionMode::Parallel)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.abort_signal.wait().await;
        Err(ToolError::Aborted)
    }
}

fn spec(name: &str, execution_mode: ToolExecutionMode) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("Test tool {name}"),
        parameters: json!({
            "type": "object",
            "additionalProperties": true
        }),
        execution_mode,
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
    }
}
