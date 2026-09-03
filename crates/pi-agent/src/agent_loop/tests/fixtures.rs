use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AbortSignal, AgentContext, AgentEvent, AgentPlugin, AgentPluginContext,
    ContentBlock, ContextEvent, ContextPatch, CustomMessage, CustomMessageContent, Message,
    ModelId, PluginError, PluginId, ProviderId, RegisterContext, RegistriesBuilder, StopReason,
    TextContent, ThinkingLevel, Tool, ToolCall, ToolCallEvent, ToolCallId, ToolCallPatch,
    ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolResultEvent, ToolResultPatch,
    ToolSpec, ToolUpdateSink, Usage, UserMessage,
};
use pi_telemetry::TelemetryContext;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::super::*;
use crate::EventError;

pub fn user(text: &str) -> Message {
    Message::User(UserMessage::text(text, 1))
}

pub fn custom(kind: &str, text: &str) -> Message {
    Message::custom(CustomMessage {
        custom_type: kind.to_string(),
        content: CustomMessageContent::Text(text.to_string()),
        display: false,
        details: None,
        timestamp_ms: 1,
    })
}

pub fn assistant(text: &str) -> Message {
    Message::assistant(text_response(text))
}

pub fn text_response(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text(TextContent::new(text))],
        api: "scripted".to_string(),
        provider: ProviderId::new("scripted"),
        model: ModelId::new("test"),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        deferred: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp_ms: 1,
    }
}

pub fn roles(messages: &[Message]) -> Vec<&'static str> {
    messages
        .iter()
        .map(|message| match message {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
            Message::Custom(_) => "custom",
        })
        .collect()
}

pub fn event_types(events: &[AgentEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::AgentEnd { .. } => "agent_end",
            AgentEvent::TurnStart => "turn_start",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        })
        .collect()
}

pub fn text(content: &[ContentBlock]) -> &str {
    let [ContentBlock::Text(text)] = content else {
        panic!("expected one text block, got {content:?}");
    };
    &text.text
}

pub fn tool_results(messages: &[Message]) -> Vec<&ToolResultMessage> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.as_ref()),
            _ => None,
        })
        .collect()
}

pub fn calls(values: &[&str]) -> AssistantMessage {
    tool_response(
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                ToolCall::new(
                    format!("tool-{}", index + 1),
                    "echo",
                    json!({"value": value}),
                )
            })
            .collect(),
    )
}

pub fn tool_response(calls: Vec<ToolCall>) -> AssistantMessage {
    AssistantMessage {
        content: calls.into_iter().map(ContentBlock::ToolCall).collect(),
        stop_reason: StopReason::ToolUse,
        ..text_response("")
    }
}

pub fn done() -> AssistantMessage {
    text_response("done")
}

pub fn truncated_call() -> AssistantMessage {
    AssistantMessage {
        stop_reason: StopReason::Length,
        ..calls(&["hel"])
    }
}

/// Pi's MockAssistantStream sends only a terminal event with the complete message.
/// Keep that input shape here so lifecycle assertions need no event normalization.
pub struct MockStream {
    turns: Mutex<VecDeque<AssistantMessage>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl MockStream {
    pub fn new(turns: impl IntoIterator<Item = AssistantMessage>) -> Arc<Self> {
        Arc::new(Self {
            turns: Mutex::new(turns.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> Vec<ProviderRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn stream_fn(self: &Arc<Self>) -> StreamFn {
        StreamFn::new({
            let stream = Arc::clone(self);
            move |request, _context, _signal| {
                let stream = Arc::clone(&stream);
                async move {
                    stream.requests.lock().unwrap().push(request);
                    let message = stream
                        .turns
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected provider request");
                    Ok(message.into())
                }
            }
        })
    }
}

#[derive(Default)]
pub struct RecordingEvents(Mutex<Vec<AgentEvent>>);

impl RecordingEvents {
    pub fn snapshot(&self) -> Vec<AgentEvent> {
        self.0.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentEventSink for RecordingEvents {
    async fn emit(
        &self,
        event: AgentEvent,
        _signal: AbortSignal,
    ) -> Result<AgentEvent, EventError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(event)
    }
}

pub struct TestLoop {
    pub context: AgentContext,
    pub config: AgentLoopConfig,
    pub services: AgentLoopServices,
    pub provider: Arc<MockStream>,
    pub events: Arc<RecordingEvents>,
}

impl TestLoop {
    pub fn new(
        turns: impl IntoIterator<Item = AssistantMessage>,
        tools: Vec<Arc<dyn Tool>>,
        mut plugins: Vec<Arc<dyn AgentPlugin>>,
    ) -> Self {
        let active_tools = tools.iter().map(|tool| tool.spec().name).collect();
        plugins.insert(0, Arc::new(ToolsPlugin(tools)));
        let provider = MockStream::new(turns);
        let (plugins, provider_plugins, registries) = RegistriesBuilder::new()
            .register_plugin_sets(plugins, vec![])
            .unwrap();
        let events = Arc::new(RecordingEvents::default());
        Self {
            context: AgentContext {
                system_prompt: "You are helpful.".to_string(),
                messages: Vec::new(),
                active_tools,
            },
            config: AgentLoopConfig {
                stream_fn: Some(provider.stream_fn()),
                convert_to_llm: ConvertToLlm::new(|messages| async move {
                    messages
                        .into_iter()
                        .filter(|message| !matches!(message, Message::Custom(_)))
                        .collect()
                }),
                transform_context: None,
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                thinking_level: ThinkingLevel::Off,
                thinking_budgets: None,
                block_images: false,
                tool_execution: ToolExecutionMode::default(),
                max_tool_iterations: 10,
                max_parallel_tools: 8,
                cwd: std::env::current_dir().unwrap(),
                session_id: None,
            },
            services: AgentLoopServices {
                default_stream_fn: None,
                generation: 1,
                registries: Arc::new(registries),
                plugins: Arc::new(plugins),
                provider_plugins: Arc::new(provider_plugins),
                queues: Arc::new(NoopMessageQueues),
                turn_control: Arc::new(NoopAgentTurnControl),
                telemetry: TelemetryContext::noop(),
                events: events.clone(),
            },
            provider,
            events,
        }
    }

    pub async fn run(&self, prompt: &str) -> AgentLoopOutcome {
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            run_agent_loop(
                RunId::new("agent-loop-test"),
                vec![user(prompt)],
                self.context.clone(),
                self.config.clone(),
                self.services.clone(),
                AbortHandle::new().1,
            ),
        )
        .await
        .expect("agent loop did not settle")
        .expect("agent loop failed");
        assert!(
            matches!(
                outcome.stop,
                AgentLoopStop::Completed | AgentLoopStop::TerminatedByTools
            ),
            "unexpected stop: {:?}",
            outcome.stop
        );
        assert!(matches!(
            self.events.snapshot().last(),
            Some(AgentEvent::AgentEnd { messages }) if messages == &outcome.new_messages
        ));
        outcome
    }

    pub async fn continue_run(&self) -> Result<AgentLoopOutcome, AgentLoopError> {
        tokio::time::timeout(
            Duration::from_secs(5),
            run_agent_loop_continue(
                RunId::new("agent-loop-continue-test"),
                self.context.clone(),
                self.config.clone(),
                self.services.clone(),
                AbortHandle::new().1,
            ),
        )
        .await
        .expect("agent loop continuation did not settle")
    }
}

struct ToolsPlugin(Vec<Arc<dyn Tool>>);

#[pi_core::agent_plugin]
impl AgentPlugin for ToolsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("agent-loop-test-tools")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for tool in &self.0 {
            context.register_tool(tool.clone())?;
        }
        Ok(())
    }
}

pub type ContextHook = dyn Fn(Vec<Message>) -> Vec<Message> + Send + Sync;
pub type BeforeToolHook = dyn Fn(ToolCallEvent) -> ToolCallPatch + Send + Sync;
pub type AfterToolHook = dyn Fn(ToolResultEvent) -> ToolResultPatch + Send + Sync;

#[derive(Default)]
pub struct Hooks {
    pub context: Option<Box<ContextHook>>,
    pub before: Option<Box<BeforeToolHook>>,
    pub after: Option<Box<AfterToolHook>>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for Hooks {
    fn id(&self) -> PluginId {
        PluginId::new("agent-loop-test-hooks")
    }

    async fn context(
        &self,
        _context: AgentPluginContext,
        event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        Ok(ContextPatch {
            messages: self.context.as_ref().map(|hook| hook(event.messages)),
        })
    }

    async fn tool_call(
        &self,
        _context: AgentPluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        Ok(self
            .before
            .as_ref()
            .map_or_else(ToolCallPatch::default, |hook| hook(event)))
    }

    async fn tool_result(
        &self,
        _context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        Ok(self
            .after
            .as_ref()
            .map_or_else(ToolResultPatch::default, |hook| hook(event)))
    }
}

#[derive(Default)]
pub struct ToolProbe {
    pub executions: Mutex<Vec<Value>>,
    pub validations: Mutex<Vec<Value>>,
    pub trace: Mutex<Vec<String>>,
    pub release_first: Notify,
}

pub struct EchoTool {
    pub name: &'static str,
    pub mode: ToolExecutionMode,
    pub probe: Arc<ToolProbe>,
    pub gated: bool,
    pub usage: Option<Usage>,
    pub terminate_values: Vec<&'static str>,
}

impl EchoTool {
    pub fn new(probe: &Arc<ToolProbe>) -> Self {
        Self {
            name: "echo",
            mode: ToolExecutionMode::default(),
            probe: probe.clone(),
            gated: false,
            usage: None,
            terminate_values: Vec::new(),
        }
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_string(),
            label: self.name.to_string(),
            description: "Echo tool".to_string(),
            parameters: json!({"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}),
            execution_mode: self.mode,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        self.probe.validations.lock().unwrap().push(input.clone());
        if input.get("value").is_some_and(Value::is_string) {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments(
                "value must be a string".to_string(),
            ))
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.probe.executions.lock().unwrap().push(input.clone());
        let value = input["value"]
            .as_str()
            .map_or_else(|| input["value"].to_string(), str::to_string);
        self.probe
            .trace
            .lock()
            .unwrap()
            .push(format!("start:{}:{value}", self.name));
        if self.gated && matches!(value.as_str(), "first" | "a") {
            self.probe.release_first.notified().await;
        }
        self.probe
            .trace
            .lock()
            .unwrap()
            .push(format!("end:{}:{value}", self.name));
        let mut result = ToolResult::text(format!("echoed: {value}"));
        result.details = Some(input);
        result.usage = self.usage.clone();
        result.terminate = self.terminate_values.contains(&value.as_str());
        Ok(result)
    }
}
