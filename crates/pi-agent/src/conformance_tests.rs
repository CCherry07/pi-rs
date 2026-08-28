use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, AgentEvent, AgentPlugin, AssistantMessage, ContentBlock, Message, ModelId,
    PluginContext, PluginError, PluginId, ProviderId, ProviderPlugin, RegisterContext,
    RegistriesBuilder, ResponseMetadata, ResponseMetadataPatch, StopReason, StreamEvent,
    TextContent, ThinkingLevel, Tool, ToolCall, ToolCallBlock, ToolCallEvent, ToolCallId,
    ToolCallPatch, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolResultEvent,
    ToolResultMessage, ToolResultPatch, ToolSpec, ToolUpdate, ToolUpdateSink, Usage, UserMessage,
};
use pi_telemetry::{InMemoryTelemetrySink, SpanStatus, TelemetryContext, TelemetryRecord};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::{
    Agent, AgentConfigurationPatch, AgentContext, AgentLoopStop, AgentLoopTurnUpdate, AgentOptions,
    AgentRestoreState, AgentRuntime, AgentTurnContext, AgentTurnControl, AgentTurnControlError,
    EventError, FnTurnControl,
};

fn build_agent(
    turns: impl IntoIterator<Item = ScriptedTurn>,
    plugins: Vec<Arc<dyn AgentPlugin>>,
    options: AgentOptions,
) -> (Agent, Arc<ScriptedProvider>) {
    let provider_plugin = Arc::new(ScriptedProviderPlugin::scripted(turns));
    let provider = provider_plugin.provider();
    let provider_plugins: Vec<Arc<dyn ProviderPlugin>> = vec![provider_plugin];
    let (plugins, provider_plugins, registries) = RegistriesBuilder::new()
        .register_plugin_sets(plugins, provider_plugins)
        .unwrap();
    let runtime = Arc::new(AgentRuntime::new(
        1,
        options.system_prompt.clone(),
        Arc::new(registries),
        Arc::new(plugins),
        Arc::new(provider_plugins),
    ));
    (Agent::with_runtime(options, runtime), provider)
}

fn assistant_message(content: Vec<ContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "scripted".to_string(),
        provider: ProviderId::new("scripted"),
        model: ModelId::new("test"),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        error_message: None,
        deferred: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp_ms: 1,
    }
}

fn tool_result_message(id: &str, name: &str, text: &str) -> Message {
    Message::tool_result(ToolResultMessage {
        tool_call_id: ToolCallId::new(id),
        tool_name: name.to_string(),
        content: vec![ContentBlock::Text(TextContent::new(text))],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp_ms: 1,
    })
}

fn tool_result_text(message: &ToolResultMessage) -> Option<&str> {
    message.content.iter().find_map(|block| match block {
        ContentBlock::Text(text) => Some(text.text.as_str()),
        _ => None,
    })
}

async fn wait_until_running(agent: &Agent) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !agent.state().is_running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("agent did not start");
}

struct ToolPlugin {
    id: &'static str,
    tools: Vec<Arc<dyn Tool>>,
}

struct RecordingTurnControl {
    stop: bool,
    saw_active_signal: AtomicBool,
    observed_roles: Mutex<Vec<Vec<&'static str>>>,
}

impl RecordingTurnControl {
    fn stopping() -> Self {
        Self {
            stop: true,
            saw_active_signal: AtomicBool::new(false),
            observed_roles: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl AgentTurnControl for RecordingTurnControl {
    async fn should_stop_after_turn(
        &self,
        context: AgentTurnContext,
        signal: AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        self.saw_active_signal
            .store(!signal.is_aborted(), Ordering::SeqCst);
        self.observed_roles
            .lock()
            .unwrap()
            .push(context.context.messages.iter().map(message_role).collect());
        Ok(self.stop)
    }
}

fn message_role(message: &Message) -> &'static str {
    match message {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolResult(_) => "toolResult",
        Message::Custom(_) => "custom",
    }
}

impl ToolPlugin {
    fn one(id: &'static str, tool: Arc<dyn Tool>) -> Self {
        Self {
            id,
            tools: vec![tool],
        }
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for ToolPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(self.id)
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for tool in &self.tools {
            context.register_tool(Arc::clone(tool))?;
        }
        Ok(())
    }
}

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} test tool"),
        parameters: json!({"type": "object", "additionalProperties": true}),
        execution_mode: ToolExecutionMode::Parallel,
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
    }
}

struct RecordingTool {
    name: &'static str,
    executions: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn spec(&self) -> ToolSpec {
        tool_spec(self.name)
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.executions.lock().unwrap().push(input.clone());
        let mut result =
            ToolResult::text(input.get("value").and_then(Value::as_str).unwrap_or("done"));
        result.terminate = input
            .get("terminate")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(result)
    }
}

struct PreparingTool {
    executions: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for PreparingTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("prepare")
    }

    async fn prepare_arguments(&self, input: Value) -> Result<Value, ToolError> {
        let Some(old_text) = input.get("oldText").and_then(Value::as_str) else {
            return Ok(input);
        };
        let Some(new_text) = input.get("newText").and_then(Value::as_str) else {
            return Ok(input);
        };
        Ok(json!({"edits": [{"oldText": old_text, "newText": new_text}]}))
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        if input.get("edits").is_some_and(Value::is_array) {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments(
                "`edits` must be an array".to_string(),
            ))
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.executions.lock().unwrap().push(input);
        Ok(ToolResult::text("prepared"))
    }
}

struct StrictTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for StrictTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("strict")
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        if input.get("value").is_some_and(Value::is_string) {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments(
                "`value` must be a string".to_string(),
            ))
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::text("strict"))
    }
}

struct PatchStrictArguments;

#[pi_core::agent_plugin]
impl AgentPlugin for PatchStrictArguments {
    fn id(&self) -> PluginId {
        PluginId::new("patch-strict-arguments")
    }

    async fn tool_call(
        &self,
        _context: PluginContext,
        _event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        Ok(ToolCallPatch {
            arguments: Some(json!({"value": 123})),
            block: None,
        })
    }
}

struct BlockAllTools;

#[pi_core::agent_plugin]
impl AgentPlugin for BlockAllTools {
    fn id(&self) -> PluginId {
        PluginId::new("block-all-tools")
    }

    async fn tool_call(
        &self,
        _context: PluginContext,
        _event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        Ok(ToolCallPatch {
            arguments: None,
            block: Some(ToolCallBlock {
                reason: "blocked by policy".to_string(),
                terminate: true,
            }),
        })
    }
}

struct TerminateToolResults;

#[pi_core::agent_plugin]
impl AgentPlugin for TerminateToolResults {
    fn id(&self) -> PluginId {
        PluginId::new("terminate-tool-results")
    }

    async fn tool_result(
        &self,
        _context: PluginContext,
        _event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        Ok(ToolResultPatch {
            terminate: Some(true),
            ..ToolResultPatch::default()
        })
    }
}

struct ObserveToolHookContext {
    observed: Arc<Mutex<Vec<Arc<AgentContext>>>>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for ObserveToolHookContext {
    fn id(&self) -> PluginId {
        PluginId::new("observe-tool-hook-context")
    }

    async fn tool_call(
        &self,
        _context: PluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        self.observed.lock().unwrap().push(event.context);
        Ok(ToolCallPatch::default())
    }

    async fn tool_result(
        &self,
        _context: PluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        self.observed.lock().unwrap().push(event.context);
        Ok(ToolResultPatch::default())
    }
}

struct LateUpdateTool {
    retained_sink: Arc<Mutex<Option<ToolUpdateSink>>>,
}

struct SettledParallelTool {
    retained_sink: Arc<Mutex<Option<ToolUpdateSink>>>,
}

#[async_trait]
impl Tool for SettledParallelTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("settled-parallel")
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        *self.retained_sink.lock().unwrap() = Some(updates);
        let mut result = ToolResult::text("settled");
        result.terminate = true;
        Ok(result)
    }
}

struct SlowParallelTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Tool for SlowParallelTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("slow-parallel")
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.started.notify_one();
        self.release.notified().await;
        let mut result = ToolResult::text("slow settled");
        result.terminate = true;
        Ok(result)
    }
}

#[async_trait]
impl Tool for LateUpdateTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("late-update")
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        assert!(updates.send(ToolUpdate {
            content: vec![ContentBlock::Text(TextContent::new("running"))],
            details: None,
        }));
        *self.retained_sink.lock().unwrap() = Some(updates);
        let mut result = ToolResult::text("done");
        result.terminate = true;
        Ok(result)
    }
}

#[test]
fn state_defaults_configuration_restore_and_subscriptions_are_explicit() {
    let options = AgentOptions::default();
    let (agent, _) = build_agent(
        [
            ScriptedTurn::Text("one".to_string()),
            ScriptedTurn::Text("two".to_string()),
        ],
        Vec::new(),
        options,
    );
    let state = agent.state();
    assert_eq!(state.system_prompt, "");
    assert_eq!(state.provider_id, ProviderId::new("scripted"));
    assert_eq!(state.model_id, ModelId::new("test"));
    assert_eq!(state.thinking_level, ThinkingLevel::Off);
    assert!(state.active_tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_running);
    assert!(state.streaming_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error_message.is_none());

    agent
        .configure(AgentConfigurationPatch {
            system_prompt: Some("custom".to_string()),
            thinking_level: Some(ThinkingLevel::High),
            ..AgentConfigurationPatch::default()
        })
        .unwrap();
    assert_eq!(agent.state().system_prompt, "custom");
    assert_eq!(agent.state().thinking_level, ThinkingLevel::High);

    let observed = Arc::new(AtomicUsize::new(0));
    let observed_by_listener = Arc::clone(&observed);
    let subscription = agent.subscribe(Arc::new(move |_event, _signal| {
        let observed = Arc::clone(&observed_by_listener);
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    assert!(agent.unsubscribe(subscription));
    assert!(!agent.unsubscribe(subscription));

    let restored = Message::User(UserMessage::text("restored", 1));
    agent
        .restore(AgentRestoreState {
            system_prompt: Some("restored prompt".to_string()),
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Low,
            active_tools: Vec::new(),
            messages: vec![restored.clone()],
        })
        .unwrap();
    assert_eq!(agent.state().system_prompt, "restored prompt");
    assert_eq!(agent.state().messages, vec![restored]);
    assert_eq!(observed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn continue_validates_tails_and_returns_only_new_messages() {
    let (agent, provider) = build_agent(
        [
            ScriptedTurn::Text("from user".to_string()),
            ScriptedTurn::Text("from tool result".to_string()),
        ],
        Vec::new(),
        AgentOptions::default(),
    );

    let empty_error = agent.continue_run().await.unwrap_err();
    assert!(empty_error.to_string().contains("context is empty"));
    assert!(!agent.state().is_running);

    agent
        .restore(AgentRestoreState {
            system_prompt: None,
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: vec![Message::assistant(assistant_message(
                vec![ContentBlock::Text(TextContent::new("tail"))],
                StopReason::Stop,
            ))],
        })
        .unwrap();
    let assistant_error = agent.continue_run().await.unwrap_err();
    assert!(
        assistant_error
            .to_string()
            .contains("cannot continue from an assistant message")
    );

    agent
        .restore(AgentRestoreState {
            system_prompt: None,
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: vec![Message::User(UserMessage::text("user tail", 1))],
        })
        .unwrap();
    let from_user = agent.continue_run().await.unwrap();
    assert_eq!(from_user.new_messages.len(), 1);
    assert!(matches!(from_user.new_messages[0], Message::Assistant(_)));

    agent
        .restore(AgentRestoreState {
            system_prompt: None,
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: vec![
                Message::User(UserMessage::text("tool request", 1)),
                Message::assistant(assistant_message(
                    vec![ContentBlock::ToolCall(ToolCall::new(
                        "call-1",
                        "fixture",
                        json!({}),
                    ))],
                    StopReason::ToolUse,
                )),
                tool_result_message("call-1", "fixture", "done"),
            ],
        })
        .unwrap();
    let from_tool = agent.continue_run().await.unwrap();
    assert_eq!(from_tool.new_messages.len(), 1);
    assert!(matches!(from_tool.new_messages[0], Message::Assistant(_)));
    assert_eq!(provider.requests().len(), 2);
}

#[tokio::test]
async fn continue_from_assistant_tail_consumes_queued_messages_like_pi() {
    let (agent, provider) = build_agent(
        [
            ScriptedTurn::Text("after first steering".to_string()),
            ScriptedTurn::Text("after second steering".to_string()),
        ],
        Vec::new(),
        AgentOptions::default(),
    );
    agent
        .restore(AgentRestoreState {
            system_prompt: None,
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: vec![Message::assistant(assistant_message(
                vec![ContentBlock::Text(TextContent::new("assistant tail"))],
                StopReason::Stop,
            ))],
        })
        .unwrap();
    agent.steer(Message::User(UserMessage::text("steer one", 2)));
    agent.steer(Message::User(UserMessage::text("steer two", 3)));

    let outcome = agent.continue_run().await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages.len(), 2);
    assert!(matches!(
        &requests[0].messages[1],
        Message::User(message)
            if matches!(&message.content[..], [ContentBlock::Text(text)] if text.text == "steer one")
    ));
    assert_eq!(requests[1].messages.len(), 4);
    assert!(matches!(
        &requests[1].messages[3],
        Message::User(message)
            if matches!(&message.content[..], [ContentBlock::Text(text)] if text.text == "steer two")
    ));
    assert_eq!(
        outcome
            .new_messages
            .iter()
            .map(message_role)
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "user", "assistant"]
    );
    assert!(!agent.has_queued_messages());

    let (follow_up_agent, follow_up_provider) = build_agent(
        [ScriptedTurn::Text("after follow-up".to_string())],
        Vec::new(),
        AgentOptions::default(),
    );
    follow_up_agent
        .restore(AgentRestoreState {
            system_prompt: None,
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: vec![Message::assistant(assistant_message(
                vec![ContentBlock::Text(TextContent::new("assistant tail"))],
                StopReason::Stop,
            ))],
        })
        .unwrap();
    follow_up_agent.follow_up(Message::User(UserMessage::text("follow-up", 4)));

    let follow_up_outcome = follow_up_agent.continue_run().await.unwrap();

    assert_eq!(follow_up_provider.requests().len(), 1);
    assert_eq!(
        follow_up_outcome
            .new_messages
            .iter()
            .map(message_role)
            .collect::<Vec<_>>(),
        vec!["user", "assistant"]
    );
    assert!(!follow_up_agent.has_queued_messages());
}

#[tokio::test]
async fn running_agent_rejects_mutation_and_parallel_runs_without_corrupting_state() {
    let (agent, _) = build_agent(
        [ScriptedTurn::WaitForAbort],
        Vec::new(),
        AgentOptions::default(),
    );
    let runner = agent.clone();
    let active = tokio::spawn(async move { runner.prompt("first").await });
    wait_until_running(&agent).await;

    assert!(matches!(
        agent.prompt("second").await.unwrap_err(),
        crate::agent::AgentError::AlreadyRunning
    ));
    assert!(matches!(
        agent.continue_run().await.unwrap_err(),
        crate::agent::AgentError::AlreadyRunning
    ));
    assert!(matches!(
        agent.reset().unwrap_err(),
        crate::agent::AgentError::ResetWhileRunning
    ));
    assert!(matches!(
        agent
            .configure(AgentConfigurationPatch {
                thinking_level: Some(ThinkingLevel::Low),
                ..AgentConfigurationPatch::default()
            })
            .unwrap_err(),
        crate::agent::AgentError::ConfigureWhileRunning
    ));
    assert_eq!(
        agent
            .state()
            .messages
            .iter()
            .filter(|message| matches!(message, Message::User(_)))
            .count(),
        1
    );

    agent.abort();
    let outcome = active.await.unwrap().unwrap();
    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    assert!(!agent.state().is_running);
    agent.reset().unwrap();
    assert!(agent.state().messages.is_empty());
}

#[tokio::test]
async fn wait_for_idle_includes_async_agent_end_listeners() {
    let (agent, _) = build_agent(
        [ScriptedTurn::Text("done".to_string())],
        Vec::new(),
        AgentOptions::default(),
    );
    let listener_entered = Arc::new(Notify::new());
    let listener_release = Arc::new(Notify::new());
    let entered_for_listener = Arc::clone(&listener_entered);
    let release_for_listener = Arc::clone(&listener_release);
    agent.subscribe(Arc::new(move |event, _signal| {
        let entered = Arc::clone(&entered_for_listener);
        let release = Arc::clone(&release_for_listener);
        async move {
            if matches!(event, AgentEvent::AgentEnd { .. }) {
                entered.notify_one();
                release.notified().await;
            }
            Ok(())
        }
    }));

    let runner = agent.clone();
    let prompt = tokio::spawn(async move { runner.prompt("settle").await.unwrap() });
    tokio::time::timeout(Duration::from_secs(1), listener_entered.notified())
        .await
        .unwrap();
    assert!(agent.state().is_running);

    let idle_finished = Arc::new(AtomicBool::new(false));
    let finished_by_waiter = Arc::clone(&idle_finished);
    let waiting_agent = agent.clone();
    let idle = tokio::spawn(async move {
        waiting_agent.wait_for_idle().await;
        finished_by_waiter.store(true, Ordering::SeqCst);
    });
    tokio::task::yield_now().await;
    assert!(!idle_finished.load(Ordering::SeqCst));

    listener_release.notify_one();
    prompt.await.unwrap();
    idle.await.unwrap();
    assert!(idle_finished.load(Ordering::SeqCst));
    assert!(!agent.state().is_running);
}

#[tokio::test]
async fn listeners_receive_the_active_abort_signal() {
    let (agent, _) = build_agent(
        [ScriptedTurn::WaitForAbort],
        Vec::new(),
        AgentOptions::default(),
    );
    let captured = Arc::new(Mutex::new(None::<AbortSignal>));
    let captured_by_listener = Arc::clone(&captured);
    let received = Arc::new(Notify::new());
    let received_by_listener = Arc::clone(&received);
    agent.subscribe(Arc::new(move |event, signal: AbortSignal| {
        let captured = Arc::clone(&captured_by_listener);
        let received = Arc::clone(&received_by_listener);
        async move {
            if matches!(event, AgentEvent::AgentStart) {
                *captured.lock().unwrap() = Some(signal);
                received.notify_one();
            }
            Ok(())
        }
    }));

    let runner = agent.clone();
    let prompt = tokio::spawn(async move { runner.prompt("abort").await.unwrap() });
    tokio::time::timeout(Duration::from_secs(1), received.notified())
        .await
        .unwrap();
    let signal = captured.lock().unwrap().clone().unwrap();
    assert!(!signal.is_aborted());
    agent.abort();
    let outcome = prompt.await.unwrap();
    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    assert!(signal.is_aborted());
}

#[tokio::test]
async fn abort_before_the_first_provider_call_commits_pending_messages_and_terminal_events() {
    let (agent, provider) = build_agent(
        [ScriptedTurn::Text("must remain unused".to_string())],
        Vec::new(),
        AgentOptions::default(),
    );
    agent.steer(Message::User(UserMessage::text(
        "already drained steering",
        2,
    )));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let entered_by_listener = Arc::clone(&entered);
    let release_by_listener = Arc::clone(&release);
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_listener = Arc::clone(&events);
    agent.subscribe(Arc::new(move |event, _signal| {
        let entered = Arc::clone(&entered_by_listener);
        let release = Arc::clone(&release_by_listener);
        let events = Arc::clone(&events_by_listener);
        async move {
            let is_start = matches!(event, AgentEvent::AgentStart);
            events.lock().unwrap().push(event);
            if is_start {
                entered.notify_one();
                release.notified().await;
            }
            Ok(())
        }
    }));

    let runner = agent.clone();
    let active = tokio::spawn(async move { runner.prompt("prompt before abort").await.unwrap() });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    agent.abort();
    release.notify_one();

    let outcome = active.await.unwrap();

    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    assert!(provider.requests().is_empty());
    assert_eq!(
        outcome
            .new_messages
            .iter()
            .map(message_role)
            .collect::<Vec<_>>(),
        vec!["user", "user", "assistant"]
    );
    let aborted = match outcome.new_messages.last().unwrap() {
        Message::Assistant(message) => message,
        message => panic!("expected aborted assistant, got {message:?}"),
    };
    assert_eq!(aborted.stop_reason, StopReason::Aborted);
    assert_eq!(aborted.error_message.as_deref(), Some("operation aborted"));
    let events = events.lock().unwrap();
    assert!(matches!(
        &events[events.len() - 4..],
        [
            AgentEvent::MessageStart { message: Message::Assistant(start) },
            AgentEvent::MessageEnd { message: Message::Assistant(end) },
            AgentEvent::TurnEnd { message: turn, tool_results },
            AgentEvent::AgentEnd { messages },
        ] if start.stop_reason == StopReason::Aborted
            && end.stop_reason == StopReason::Aborted
            && turn.stop_reason == StopReason::Aborted
            && tool_results.is_empty()
            && messages.len() == 3
    ));
}

#[tokio::test]
async fn mutable_session_id_is_forwarded_to_every_provider_request() {
    let (agent, provider) = build_agent(
        [
            ScriptedTurn::Text("one".to_string()),
            ScriptedTurn::Text("two".to_string()),
        ],
        Vec::new(),
        AgentOptions::default(),
    );
    agent.set_session_id(Some("session-one".to_string()));
    agent.prompt("first").await.unwrap();
    agent.set_session_id(Some("session-two".to_string()));
    agent.prompt("second").await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests[0].session_id.as_deref(), Some("session-one"));
    assert_eq!(requests[1].session_id.as_deref(), Some("session-two"));
    assert_eq!(agent.session_id().as_deref(), Some("session-two"));
}

#[tokio::test]
async fn incomplete_stream_eof_preserves_partial_assistant_and_response_metadata() {
    let events = vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                1,
            ),
        },
        StreamEvent::Metadata {
            patch: ResponseMetadataPatch {
                response_id: Some("partial-response".to_string()),
                raw_stop_reason: Some("upstream-eof".to_string()),
                ..ResponseMetadataPatch::default()
            },
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "kept partial".to_string(),
        },
    ];
    let (agent, _) = build_agent(
        [ScriptedTurn::Events(events)],
        Vec::new(),
        AgentOptions::default(),
    );

    let outcome = agent.prompt("run").await.unwrap();
    assert_eq!(outcome.stop, AgentLoopStop::ProviderError);
    let assistant = outcome
        .new_messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message) => Some(message),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        &assistant.content[0],
        ContentBlock::Text(text) if text.text == "kept partial"
    ));
    assert_eq!(assistant.stop_reason, StopReason::Error);
    assert_eq!(assistant.response_id.as_deref(), Some("partial-response"));
    assert_eq!(assistant.raw_stop_reason.as_deref(), Some("upstream-eof"));
    assert_eq!(
        assistant.error_message.as_deref(),
        Some("stream ended without Done")
    );
}

#[tokio::test]
async fn length_truncated_tool_calls_fail_without_execution_and_can_be_reissued() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "record",
        executions: Arc::clone(&executions),
    });
    let truncated_events = vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                1,
            ),
        },
        StreamEvent::ToolCallStart {
            content_index: 0,
            id: ToolCallId::new("truncated-1"),
            name: "record".to_string(),
        },
        StreamEvent::ToolCallDelta {
            content_index: 0,
            arguments_delta: json!({"value": "partial"}).to_string(),
        },
        StreamEvent::ToolCallEnd {
            content_index: 0,
            thought_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Length,
            usage: Usage::default(),
        },
    ];
    let (agent, provider) = build_agent(
        [
            ScriptedTurn::Events(truncated_events),
            ScriptedTurn::Text("retry complete".to_string()),
        ],
        vec![Arc::new(ToolPlugin::one("record-tool", tool))],
        AgentOptions {
            active_tools: vec!["record".to_string()],
            ..AgentOptions::default()
        },
    );

    let outcome = agent.prompt("run").await.unwrap();
    assert!(executions.lock().unwrap().is_empty());
    let result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .unwrap();
    assert!(result.is_error);
    assert!(
        tool_result_text(result)
            .unwrap()
            .contains("output token limit")
    );
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(outcome.stop, AgentLoopStop::Completed);
}

#[tokio::test]
async fn tool_argument_preparation_runs_before_validation_and_execution() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let (agent, _) = build_agent(
        [
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "prepare-1",
                "prepare",
                json!({"oldText": "before", "newText": "after"}),
            )]),
            ScriptedTurn::Text("done".to_string()),
        ],
        vec![Arc::new(ToolPlugin::one(
            "prepare-tool",
            Arc::new(PreparingTool {
                executions: Arc::clone(&executions),
            }),
        ))],
        AgentOptions {
            active_tools: vec!["prepare".to_string()],
            ..AgentOptions::default()
        },
    );

    agent.prompt("prepare").await.unwrap();
    assert_eq!(
        *executions.lock().unwrap(),
        vec![json!({"edits": [{"oldText": "before", "newText": "after"}]})]
    );
}

#[tokio::test]
async fn agent_loop_passes_the_current_batch_context_to_both_tool_hooks() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (agent, _) = build_agent(
        [ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "context-1",
            "context-tool",
            json!({"value": "done", "terminate": true}),
        )])],
        vec![
            Arc::new(ToolPlugin::one(
                "context-tool-plugin",
                Arc::new(RecordingTool {
                    name: "context-tool",
                    executions,
                }),
            )),
            Arc::new(ObserveToolHookContext {
                observed: Arc::clone(&observed),
            }),
        ],
        AgentOptions {
            system_prompt: "batch system prompt".to_string(),
            active_tools: vec!["context-tool".to_string()],
            ..AgentOptions::default()
        },
    );

    agent.prompt("use the tool").await.unwrap();

    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert!(Arc::ptr_eq(&observed[0], &observed[1]));
    assert_eq!(observed[0].system_prompt, "batch system prompt");
    assert_eq!(observed[0].active_tools, ["context-tool"]);
    assert_eq!(
        observed[0]
            .messages
            .iter()
            .map(message_role)
            .collect::<Vec<_>>(),
        ["user", "assistant"]
    );
}

#[tokio::test]
async fn hook_patched_arguments_are_revalidated_before_execution() {
    let executions = Arc::new(AtomicUsize::new(0));
    let (agent, _) = build_agent(
        [
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "strict-1",
                "strict",
                json!({"value": "valid"}),
            )]),
            ScriptedTurn::Text("recovered".to_string()),
        ],
        vec![
            Arc::new(ToolPlugin::one(
                "strict-tool",
                Arc::new(StrictTool {
                    executions: Arc::clone(&executions),
                }),
            )),
            Arc::new(PatchStrictArguments),
        ],
        AgentOptions {
            active_tools: vec!["strict".to_string()],
            ..AgentOptions::default()
        },
    );

    let outcome = agent.prompt("strict").await.unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(outcome.new_messages.iter().any(|message| matches!(
        message,
        Message::ToolResult(result)
            if result.is_error
                && tool_result_text(result).is_some_and(|text| text.contains("must be a string"))
    )));
}

#[tokio::test]
async fn a_tool_batch_terminates_only_when_every_result_terminates() {
    let all_executions = Arc::new(Mutex::new(Vec::new()));
    let (all_agent, all_provider) = build_agent(
        [ScriptedTurn::ToolCalls(vec![
            ToolCall::new(
                "all-1",
                "toggle",
                json!({"value": "one", "terminate": true}),
            ),
            ToolCall::new(
                "all-2",
                "toggle",
                json!({"value": "two", "terminate": true}),
            ),
        ])],
        vec![Arc::new(ToolPlugin::one(
            "all-toggle-tool",
            Arc::new(RecordingTool {
                name: "toggle",
                executions: Arc::clone(&all_executions),
            }),
        ))],
        AgentOptions {
            active_tools: vec!["toggle".to_string()],
            ..AgentOptions::default()
        },
    );
    let all_outcome = all_agent.prompt("all").await.unwrap();
    assert_eq!(all_outcome.stop, AgentLoopStop::TerminatedByTools);
    assert_eq!(all_provider.requests().len(), 1);
    assert_eq!(all_executions.lock().unwrap().len(), 2);

    let mixed_executions = Arc::new(Mutex::new(Vec::new()));
    let (mixed_agent, mixed_provider) = build_agent(
        [
            ScriptedTurn::ToolCalls(vec![
                ToolCall::new(
                    "mixed-1",
                    "toggle",
                    json!({"value": "one", "terminate": true}),
                ),
                ToolCall::new(
                    "mixed-2",
                    "toggle",
                    json!({"value": "two", "terminate": false}),
                ),
            ]),
            ScriptedTurn::Text("continued".to_string()),
        ],
        vec![Arc::new(ToolPlugin::one(
            "mixed-toggle-tool",
            Arc::new(RecordingTool {
                name: "toggle",
                executions: Arc::clone(&mixed_executions),
            }),
        ))],
        AgentOptions {
            active_tools: vec!["toggle".to_string()],
            ..AgentOptions::default()
        },
    );
    let mixed_outcome = mixed_agent.prompt("mixed").await.unwrap();
    assert_eq!(mixed_outcome.stop, AgentLoopStop::Completed);
    assert_eq!(mixed_provider.requests().len(), 2);
    assert_eq!(mixed_executions.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn blocking_and_after_result_hooks_can_terminate_a_batch() {
    let blocked_executions = Arc::new(Mutex::new(Vec::new()));
    let (blocked_agent, blocked_provider) = build_agent(
        [ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "blocked-1",
            "blocked",
            json!({"value": "never"}),
        )])],
        vec![
            Arc::new(ToolPlugin::one(
                "blocked-tool",
                Arc::new(RecordingTool {
                    name: "blocked",
                    executions: Arc::clone(&blocked_executions),
                }),
            )),
            Arc::new(BlockAllTools),
        ],
        AgentOptions {
            active_tools: vec!["blocked".to_string()],
            ..AgentOptions::default()
        },
    );
    let blocked = blocked_agent.prompt("blocked").await.unwrap();
    assert_eq!(blocked.stop, AgentLoopStop::TerminatedByTools);
    assert!(blocked_executions.lock().unwrap().is_empty());
    assert_eq!(blocked_provider.requests().len(), 1);
    assert!(blocked.new_messages.iter().any(|message| matches!(
        message,
        Message::ToolResult(result)
            if result.is_error && tool_result_text(result) == Some("blocked by policy")
    )));

    let after_executions = Arc::new(Mutex::new(Vec::new()));
    let (after_agent, after_provider) = build_agent(
        [ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "after-1",
            "after",
            json!({"value": "executed"}),
        )])],
        vec![
            Arc::new(ToolPlugin::one(
                "after-tool",
                Arc::new(RecordingTool {
                    name: "after",
                    executions: Arc::clone(&after_executions),
                }),
            )),
            Arc::new(TerminateToolResults),
        ],
        AgentOptions {
            active_tools: vec!["after".to_string()],
            ..AgentOptions::default()
        },
    );
    let after = after_agent.prompt("after").await.unwrap();
    assert_eq!(after.stop, AgentLoopStop::TerminatedByTools);
    assert_eq!(after_executions.lock().unwrap().len(), 1);
    assert_eq!(after_provider.requests().len(), 1);
}

#[tokio::test]
async fn tool_updates_are_closed_and_ignored_after_execution_settles() {
    let retained_sink = Arc::new(Mutex::new(None));
    let (agent, _) = build_agent(
        [ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "late-1",
            "late-update",
            json!({}),
        )])],
        vec![Arc::new(ToolPlugin::one(
            "late-update-tool",
            Arc::new(LateUpdateTool {
                retained_sink: Arc::clone(&retained_sink),
            }),
        ))],
        AgentOptions {
            active_tools: vec!["late-update".to_string()],
            ..AgentOptions::default()
        },
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_listener = Arc::clone(&events);
    agent.subscribe(Arc::new(move |event, _signal| {
        let events = Arc::clone(&events_by_listener);
        async move {
            events.lock().unwrap().push(event);
            Ok::<(), EventError>(())
        }
    }));

    agent.prompt("late update").await.unwrap();
    let event_count = events.lock().unwrap().len();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
            .count(),
        1
    );
    let accepted = retained_sink
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .send(ToolUpdate {
            content: vec![ContentBlock::Text(TextContent::new("too late"))],
            details: None,
        });
    assert!(!accepted);
    tokio::task::yield_now().await;
    assert_eq!(events.lock().unwrap().len(), event_count);
}

#[tokio::test]
async fn a_settled_parallel_tool_cannot_emit_while_another_tool_is_running() {
    let retained_sink = Arc::new(Mutex::new(None));
    let slow_started = Arc::new(Notify::new());
    let release_slow = Arc::new(Notify::new());
    let tools = ToolPlugin {
        id: "parallel-update-tools",
        tools: vec![
            Arc::new(SettledParallelTool {
                retained_sink: Arc::clone(&retained_sink),
            }),
            Arc::new(SlowParallelTool {
                started: Arc::clone(&slow_started),
                release: Arc::clone(&release_slow),
            }),
        ],
    };
    let (agent, _) = build_agent(
        [ScriptedTurn::ToolCalls(vec![
            ToolCall::new("settled-1", "settled-parallel", json!({})),
            ToolCall::new("slow-1", "slow-parallel", json!({})),
        ])],
        vec![Arc::new(tools)],
        AgentOptions {
            active_tools: vec!["settled-parallel".to_string(), "slow-parallel".to_string()],
            ..AgentOptions::default()
        },
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_listener = Arc::clone(&events);
    let settled_ended = Arc::new(Notify::new());
    let ended_by_listener = Arc::clone(&settled_ended);
    agent.subscribe(Arc::new(move |event, _signal| {
        let events = Arc::clone(&events_by_listener);
        let ended = Arc::clone(&ended_by_listener);
        async move {
            if matches!(
                &event,
                AgentEvent::ToolExecutionEnd { tool_call_id, .. }
                    if tool_call_id.as_str() == "settled-1"
            ) {
                ended.notify_one();
            }
            events.lock().unwrap().push(event);
            Ok(())
        }
    }));

    let runner = agent.clone();
    let prompt = tokio::spawn(async move { runner.prompt("parallel").await.unwrap() });
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(slow_started.notified(), settled_ended.notified());
    })
    .await
    .unwrap();
    let event_count = events.lock().unwrap().len();
    assert!(
        !retained_sink
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(ToolUpdate {
                content: vec![ContentBlock::Text(TextContent::new("late parallel"))],
                details: None,
            })
    );
    tokio::task::yield_now().await;
    assert_eq!(events.lock().unwrap().len(), event_count);

    release_slow.notify_one();
    let outcome = prompt.await.unwrap();
    assert_eq!(outcome.stop, AgentLoopStop::TerminatedByTools);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
            .count(),
        0
    );
}

#[tokio::test]
async fn prepare_next_turn_replaces_run_local_context_model_and_thinking() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let prepare_once = Arc::new(AtomicBool::new(false));
    let saw_active_signal = Arc::new(AtomicBool::new(false));
    let observed_prompts = Arc::new(Mutex::new(Vec::new()));
    let controller = Arc::new(
        FnTurnControl::new()
            .with_prepare_next_turn({
                let prepare_once = Arc::clone(&prepare_once);
                let saw_active_signal = Arc::clone(&saw_active_signal);
                move |context, signal| {
                    let prepare_once = Arc::clone(&prepare_once);
                    let saw_active_signal = Arc::clone(&saw_active_signal);
                    async move {
                        tokio::task::yield_now().await;
                        saw_active_signal.store(!signal.is_aborted(), Ordering::SeqCst);
                        if prepare_once.swap(true, Ordering::SeqCst) {
                            return Ok(None);
                        }
                        let mut next_context = Arc::unwrap_or_clone(context.context);
                        next_context.system_prompt = "prepared prompt".to_string();
                        Ok(Some(AgentLoopTurnUpdate {
                            context: Some(next_context),
                            model_id: Some(ModelId::new("prepared-model")),
                            thinking_level: Some(ThinkingLevel::High),
                            ..AgentLoopTurnUpdate::default()
                        }))
                    }
                }
            })
            .with_should_stop_after_turn({
                let observed_prompts = Arc::clone(&observed_prompts);
                move |context, _signal| {
                    let observed_prompts = Arc::clone(&observed_prompts);
                    async move {
                        observed_prompts
                            .lock()
                            .unwrap()
                            .push(context.context.system_prompt.clone());
                        Ok(false)
                    }
                }
            }),
    );
    let (agent, provider) = build_agent(
        [
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "prepare-turn-1",
                "record",
                json!({"value": "first"}),
            )]),
            ScriptedTurn::Text("done".to_string()),
        ],
        vec![Arc::new(ToolPlugin::one(
            "prepare-turn-tool",
            Arc::new(RecordingTool {
                name: "record",
                executions: Arc::clone(&executions),
            }),
        ))],
        AgentOptions {
            active_tools: vec!["record".to_string()],
            turn_control: controller.clone(),
            ..AgentOptions::default()
        },
    );

    agent.prompt("start").await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].system_prompt, "prepared prompt");
    assert_eq!(requests[1].model.as_str(), "prepared-model");
    assert_eq!(requests[1].thinking_level, ThinkingLevel::High);
    assert!(saw_active_signal.load(Ordering::SeqCst));
    assert_eq!(
        *observed_prompts.lock().unwrap(),
        vec!["prepared prompt", "prepared prompt"]
    );
    assert_eq!(agent.state().model_id.as_str(), "test");
    assert_eq!(agent.state().thinking_level, ThinkingLevel::Off);
}

#[tokio::test]
async fn turn_control_closures_share_snapshots_and_isolate_copy_on_write_changes() {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let shared_snapshots = Arc::new(Mutex::new(None::<(Weak<AgentContext>, Weak<Vec<Message>>)>));
    let prepare_observations = Arc::clone(&observations);
    let stop_observations = Arc::clone(&observations);
    let prepare_snapshots = Arc::clone(&shared_snapshots);
    let stop_snapshots = Arc::clone(&shared_snapshots);
    let controller = Arc::new(
        FnTurnControl::new()
            .with_prepare_next_turn(move |mut context, signal| {
                let observations = Arc::clone(&prepare_observations);
                let shared_snapshots = Arc::clone(&prepare_snapshots);
                async move {
                    tokio::task::yield_now().await;
                    assert!(!signal.is_aborted());
                    observations.lock().unwrap().push("prepare");
                    *shared_snapshots.lock().unwrap() = Some((
                        Arc::downgrade(&context.context),
                        Arc::downgrade(&context.new_messages),
                    ));
                    Arc::make_mut(&mut context.context).system_prompt = "detached".to_string();
                    Arc::make_mut(&mut context.new_messages).clear();
                    Ok(None)
                }
            })
            .with_should_stop_after_turn(move |context, signal| {
                let observations = Arc::clone(&stop_observations);
                let shared_snapshots = Arc::clone(&stop_snapshots);
                async move {
                    assert!(!signal.is_aborted());
                    assert_eq!(context.context.system_prompt, "base prompt");
                    assert_eq!(context.new_messages.len(), 2);
                    observations.lock().unwrap().push("stop");
                    let (prepared_context, prepared_messages) =
                        shared_snapshots.lock().unwrap().take().unwrap();
                    let prepared_context = prepared_context.upgrade().unwrap();
                    let prepared_messages = prepared_messages.upgrade().unwrap();
                    assert!(Arc::ptr_eq(&prepared_context, &context.context));
                    assert!(Arc::ptr_eq(&prepared_messages, &context.new_messages));
                    Ok(false)
                }
            }),
    );
    let (agent, _) = build_agent(
        [ScriptedTurn::Text("done".to_string())],
        Vec::new(),
        AgentOptions {
            system_prompt: "base prompt".to_string(),
            turn_control: controller,
            ..AgentOptions::default()
        },
    );

    let outcome = agent.prompt("start").await.unwrap();

    assert_eq!(outcome.final_context.system_prompt, "base prompt");
    assert_eq!(outcome.new_messages.len(), 2);
    let observations = observations.lock().unwrap();
    assert_eq!(*observations, vec!["prepare", "stop"]);
}

#[tokio::test]
async fn should_stop_after_turn_runs_after_tools_and_before_queue_polling() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let controller = Arc::new(RecordingTurnControl::stopping());
    let (agent, provider) = build_agent(
        [
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "stop-turn-1",
                "record",
                json!({"value": "complete first"}),
            )]),
            ScriptedTurn::Text("must remain unused".to_string()),
        ],
        vec![Arc::new(ToolPlugin::one(
            "stop-turn-tool",
            Arc::new(RecordingTool {
                name: "record",
                executions: Arc::clone(&executions),
            }),
        ))],
        AgentOptions {
            active_tools: vec!["record".to_string()],
            turn_control: controller.clone(),
            ..AgentOptions::default()
        },
    );
    agent.follow_up(Message::User(UserMessage::text("stay queued", 2)));

    let outcome = agent.prompt("start").await.unwrap();

    assert_eq!(provider.requests().len(), 1);
    assert_eq!(executions.lock().unwrap().len(), 1);
    assert_eq!(outcome.stop, AgentLoopStop::Completed);
    assert!(agent.has_queued_messages());
    assert!(controller.saw_active_signal.load(Ordering::SeqCst));
    assert_eq!(
        *controller.observed_roles.lock().unwrap(),
        vec![vec!["user", "assistant", "toolResult"]]
    );
}

#[tokio::test]
async fn turn_control_failure_returns_an_error_after_emitting_pi_terminal_lifecycle() {
    let controller = Arc::new(FnTurnControl::new().with_prepare_next_turn(
        |_context, _signal| async move {
            Err(AgentTurnControlError(
                "prepare callback exploded".to_string(),
            ))
        },
    ));
    let (agent, _) = build_agent(
        [ScriptedTurn::Text("completed response".to_string())],
        Vec::new(),
        AgentOptions {
            turn_control: controller,
            ..AgentOptions::default()
        },
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_listener = Arc::clone(&events);
    agent.subscribe(Arc::new(move |event, _signal| {
        let events = Arc::clone(&events_by_listener);
        async move {
            events.lock().unwrap().push(event);
            Ok(())
        }
    }));

    let error = agent.prompt("start").await.unwrap_err();

    assert!(error.to_string().contains("prepare callback exploded"));
    assert!(!agent.state().is_running);
    let state = agent.state();
    assert!(matches!(
        state.messages.last(),
        Some(Message::Assistant(message))
            if message.stop_reason == StopReason::Error
                && message.error_message.as_deref().is_some_and(|error| error.contains("prepare callback exploded"))
    ));
    let events = events.lock().unwrap();
    assert!(matches!(
        &events[events.len() - 4..],
        [
            AgentEvent::MessageStart { message: Message::Assistant(start) },
            AgentEvent::MessageEnd { message: Message::Assistant(end) },
            AgentEvent::TurnEnd { message: turn, tool_results },
            AgentEvent::AgentEnd { messages },
        ] if start.stop_reason == StopReason::Error
            && end.stop_reason == StopReason::Error
            && turn.stop_reason == StopReason::Error
            && tool_results.is_empty()
            && matches!(&messages[..], [Message::Assistant(message)] if message.stop_reason == StopReason::Error)
    ));
}

#[tokio::test]
async fn one_shot_event_failure_still_reduces_a_terminal_failure_sequence() {
    let (agent, _) = build_agent(
        [ScriptedTurn::Text("completed response".to_string())],
        Vec::new(),
        AgentOptions::default(),
    );
    let failed = Arc::new(AtomicBool::new(false));
    let failed_by_listener = Arc::clone(&failed);
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_listener = Arc::clone(&events);
    agent.subscribe(Arc::new(move |event, _signal| {
        let failed = Arc::clone(&failed_by_listener);
        let events = Arc::clone(&events_by_listener);
        async move {
            let fail_now =
                matches!(event, AgentEvent::TurnEnd { .. }) && !failed.swap(true, Ordering::SeqCst);
            events.lock().unwrap().push(event);
            if fail_now {
                return Err(EventError("turn listener exploded".to_string()));
            }
            Ok(())
        }
    }));

    let error = agent.prompt("start").await.unwrap_err();

    assert!(error.to_string().contains("turn listener exploded"));
    assert!(matches!(
        agent.state().messages.last(),
        Some(Message::Assistant(message))
            if message.stop_reason == StopReason::Error
                && message.error_message.as_deref().is_some_and(|error| error.contains("turn listener exploded"))
    ));
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(AgentEvent::AgentEnd { messages })
            if matches!(&messages[..], [Message::Assistant(message)] if message.stop_reason == StopReason::Error)
    ));
}

#[tokio::test]
async fn provider_requests_emit_typed_ai_telemetry_lifecycle() {
    let sink = Arc::new(InMemoryTelemetrySink::default());
    let (agent, _) = build_agent(
        [ScriptedTurn::Text("observed".to_string())],
        Vec::new(),
        AgentOptions {
            telemetry: TelemetryContext::new(sink.clone()),
            ..AgentOptions::default()
        },
    );

    agent.prompt("telemetry").await.unwrap();

    let records = sink.records();
    assert_eq!(records.len(), 2);
    assert!(matches!(
        &records[0],
        TelemetryRecord::Start { name, attributes, .. }
            if name == "pi.ai.request"
                && attributes["pi.ai.operation"] == "stream"
                && attributes["pi.ai.provider"] == "scripted"
    ));
    assert!(matches!(
        &records[1],
        TelemetryRecord::End { name, attributes, status: SpanStatus::Ok, .. }
            if name == "pi.ai.request"
                && attributes["pi.ai.response.stop_reason"] == "stop"
                && attributes["pi.ai.stream.chunk_count"].as_u64().is_some_and(|count| count > 0)
    ));
}
