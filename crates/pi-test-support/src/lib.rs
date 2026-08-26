#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use pi_core::{
    AbortSignal, AgentPlugin, ContentMetadata, ModelId, PluginId, Provider, ProviderCallContext,
    ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext, ProviderRequest,
    ProviderStream, RegisterContext, ResponseMetadata, StopReason, StreamEvent, Tool, ToolCall,
    ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink,
    Usage,
};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub enum ScriptedTurn {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Events(Vec<StreamEvent>),
    Error(String),
    WaitForAbort,
}

pub struct ScriptedProvider {
    id: ProviderId,
    model: ModelId,
    turns: tokio::sync::Mutex<VecDeque<ScriptedTurn>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    pub fn new(
        id: ProviderId,
        model: ModelId,
        turns: impl IntoIterator<Item = ScriptedTurn>,
    ) -> Self {
        Self {
            id,
            model,
            turns: tokio::sync::Mutex::new(turns.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ProviderRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn metadata(&self) -> ResponseMetadata {
        ResponseMetadata::new(self.id.clone(), self.model.clone(), "scripted", now_ms())
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        let turn = self
            .turns
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| ProviderError::Protocol("no scripted turn remains".to_string()))?;
        let events = match turn {
            ScriptedTurn::Text(text) => vec![
                StreamEvent::Start {
                    metadata: self.metadata(),
                },
                StreamEvent::TextStart { content_index: 0 },
                StreamEvent::TextDelta {
                    content_index: 0,
                    delta: text,
                },
                StreamEvent::TextEnd {
                    content_index: 0,
                    text_signature: None,
                },
                StreamEvent::Done {
                    reason: StopReason::Stop,
                    usage: Usage::default(),
                },
            ],
            ScriptedTurn::ToolCalls(calls) => {
                let mut events = vec![StreamEvent::Start {
                    metadata: self.metadata(),
                }];
                for (content_index, call) in calls.into_iter().enumerate() {
                    let namespace = call.namespace.clone();
                    events.push(StreamEvent::ToolCallStart {
                        content_index,
                        id: call.id,
                        name: call.name,
                    });
                    if namespace.is_some() {
                        events.push(StreamEvent::ContentMetadata {
                            content_index,
                            metadata: ContentMetadata::ToolCall { namespace },
                        });
                    }
                    events.push(StreamEvent::ToolCallDelta {
                        content_index,
                        arguments_delta: call.arguments.to_string(),
                    });
                    events.push(StreamEvent::ToolCallEnd {
                        content_index,
                        thought_signature: call.thought_signature,
                    });
                }
                events.push(StreamEvent::Done {
                    reason: StopReason::ToolUse,
                    usage: Usage::default(),
                });
                events
            }
            ScriptedTurn::Events(events) => events,
            ScriptedTurn::Error(message) => return Err(ProviderError::Failure(message)),
            ScriptedTurn::WaitForAbort => {
                return Ok(Box::pin(stream::once(async move {
                    signal.wait().await;
                    Err(ProviderError::Aborted)
                })));
            }
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

pub struct ScriptedProviderPlugin {
    provider: Arc<ScriptedProvider>,
}

impl ScriptedProviderPlugin {
    pub fn scripted(turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
        Self {
            provider: Arc::new(ScriptedProvider::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                turns,
            )),
        }
    }

    pub fn provider(&self) -> Arc<ScriptedProvider> {
        Arc::clone(&self.provider)
    }
}

impl ProviderPlugin for ScriptedProviderPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("scripted-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

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
        test_tool_spec("echo", ToolExecutionMode::Parallel)
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
        test_tool_spec(self.name, self.mode)
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
        test_tool_spec("fail", ToolExecutionMode::Parallel)
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
        test_tool_spec("update", ToolExecutionMode::Parallel)
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
        test_tool_spec("wait_for_abort", ToolExecutionMode::Parallel)
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

fn test_tool_spec(name: &str, execution_mode: ToolExecutionMode) -> ToolSpec {
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
