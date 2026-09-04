use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AbortSignal, AgentPlugin, AgentPluginContext, BeforeAgentStartEvent,
    BeforeAgentStartPatch, BeforeProviderHeadersEvent, ContentBlock, EphemeralSessionRequest,
    EphemeralSessionStatus, Message, ModelId, ModelSelection, PluginError, PluginId, Provider,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderPluginContext,
    ProviderRegisterContext, ProviderRequest, ProviderStream, RegisterContext, ThinkingBudgets,
    ThinkingLevel, Tool, ToolCall, ToolCallId, ToolContext, ToolError, ToolExecutionMode,
    ToolResult, ToolSpec, ToolUpdateSink, UserMessage,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

use crate::{AgentOptions, PiRuntime};

mod tool_plugins;

fn request(tools: &[&str]) -> EphemeralSessionRequest {
    EphemeralSessionRequest {
        system_prompt: Some("Only maintain memory".to_string()),
        origin: "test maintenance".to_string(),
        inherit_history: false,
        history_tail: None,
        messages: vec![Message::User(UserMessage::text("maintain", 0))],
        tools: tools.iter().map(|name| name.to_string()).collect(),
        plugins: Vec::new(),
        model: None,
        thinking_level: None,
        max_tool_iterations: 3,
        max_input_tokens: None,
        compaction: None,
        timeout: Duration::from_secs(5),
    }
}

struct ProbeTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ProbeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_string(),
            label: self.name.to_string(),
            description: self.name.to_string(),
            parameters: json!({"type":"object"}),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }
    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        assert!(
            context.session.id().is_err(),
            "ephemeral tools must not reach the parent"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::text("saved"))
    }
}

struct ProbePlugin {
    allowed: Arc<AtomicUsize>,
    forbidden: Arc<AtomicUsize>,
    hooks: Arc<AtomicUsize>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for ProbePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("probe")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(ProbeTool {
            name: "allowed",
            calls: Arc::clone(&self.allowed),
        }))?;
        context.register_tool(Arc::new(ProbeTool {
            name: "forbidden",
            calls: Arc::clone(&self.forbidden),
        }))
    }
    async fn before_agent_start(
        &self,
        _: AgentPluginContext,
        _: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        self.hooks.fetch_add(1, Ordering::SeqCst);
        Ok(BeforeAgentStartPatch::default())
    }
}

#[tokio::test]
async fn tool_loop_is_scoped_and_does_not_emit_parent_events_or_hooks() {
    let allowed = Arc::new(AtomicUsize::new(0));
    let forbidden = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(AtomicUsize::new(0));
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![
            ToolCall::new("save", "allowed", json!({})),
            ToolCall::new("escape", "forbidden", json!({})),
        ]),
        ScriptedTurn::Text("done".to_string()),
    ]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_plugin(ProbePlugin {
            allowed: allowed.clone(),
            forbidden: forbidden.clone(),
            hooks: hooks.clone(),
        })
        .agent_options(AgentOptions {
            active_tools: vec!["allowed".to_string(), "forbidden".to_string()],
            messages: vec![Message::User(UserMessage::text("parent history", 0))],
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let events = Arc::new(AtomicUsize::new(0));
    runtime.agent().subscribe(Arc::new({
        let events = events.clone();
        move |_, _| {
            events.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        }
    }));
    let before = runtime.agent().state().messages;
    let outcome = runtime
        .run_ephemeral(request(&["allowed"]), AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
    assert_eq!(allowed.load(Ordering::SeqCst), 1);
    assert_eq!(forbidden.load(Ordering::SeqCst), 0);
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.agent().state().messages, before);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["allowed", "forbidden"]
    );
    assert_eq!(requests[0].system_prompt, "Only maintain memory");
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[0].session_id, runtime.agent().session_id());
    assert_eq!(requests[0].session_id, requests[1].session_id);
    assert!(outcome.messages.iter().any(|message| matches!(message, Message::ToolResult(result) if result.tool_name == "forbidden" && result.is_error)));
}

#[tokio::test]
async fn requested_tools_cannot_exceed_the_parent_ceiling() {
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .build()
        .unwrap();
    assert!(
        runtime
            .run_ephemeral(request(&["forbidden"]), AbortHandle::new().1)
            .await
            .unwrap_err()
            .to_string()
            .contains("outside the calling session")
    );
}

#[tokio::test]
async fn ephemeral_session_pins_its_generation_without_blocking_reload() {
    let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::WaitForAbort]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .build()
        .unwrap();
    let old_context = runtime.plugin_context_handle(pi_core::PluginContextScope::Base);
    let (abort, signal) = AbortHandle::new();
    let mut run = Box::pin(runtime.run_ephemeral(request(&[]), signal));
    tokio::select! {
        result = &mut run => panic!("session ended before cancellation: {result:?}"),
        () = async { while provider.requests().is_empty() { tokio::task::yield_now().await; } } => {},
    }
    tokio::time::timeout(Duration::from_secs(1), runtime.reload())
        .await
        .expect("ephemeral sessions must not hold the parent reload gate")
        .unwrap();
    assert!(old_context.access_for_adapter().is_ok());
    abort.abort();
    assert_eq!(run.await.unwrap().status, EphemeralSessionStatus::Aborted);
    assert!(matches!(
        old_context.access_for_adapter(),
        Err(pi_core::PluginContextError::Retired)
    ));
}

#[tokio::test]
async fn timeout_cancel_and_dropped_future_abort_the_provider_without_detached_work() {
    for mode in ["timeout", "cancel", "drop"] {
        let provider = Arc::new(AuthProvider {
            inner: ScriptedProvider::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                [ScriptedTurn::WaitForAbort],
            ),
            observed: Mutex::new(Vec::new()),
            signals: Mutex::new(Vec::new()),
        });
        let runtime = PiRuntime::builder()
            .provider_plugin(AuthPlugin {
                provider: provider.clone(),
                credential: Arc::new(Mutex::new("fixture".to_string())),
            })
            .build()
            .unwrap();
        let (abort, signal) = AbortHandle::new();
        let mut input = request(&[]);
        if mode == "timeout" {
            input.timeout = Duration::from_millis(20);
        }
        let mut run = Box::pin(runtime.run_ephemeral(input, signal));
        tokio::select! {
            outcome = &mut run => panic!("run ended prematurely: {outcome:?}"),
            () = async { while provider.inner.requests().is_empty() { tokio::task::yield_now().await; } } => {}
        }
        if mode == "drop" {
            drop(run);
        } else {
            if mode == "cancel" {
                abort.abort();
            }
            let result = run.await.unwrap();
            assert_eq!(
                result.status,
                if mode == "cancel" {
                    EphemeralSessionStatus::Aborted
                } else {
                    EphemeralSessionStatus::TimedOut
                }
            );
        }
        assert!(runtime.agent().state().messages.is_empty());
        assert!(!runtime.agent().is_running());
        let signals = provider.signals.lock().unwrap();
        assert_eq!(signals.len(), 1);
        assert!(
            signals[0].is_aborted(),
            "{mode} must cancel the child's provider signal"
        );
    }
}

#[tokio::test]
async fn cancellation_and_timeout_return_completed_balanced_tool_receipts() {
    for mode in ["cancel", "timeout"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let scripted = ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![ToolCall::new("saved", "allowed", json!({}))]),
            ScriptedTurn::WaitForAbort,
        ]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .agent_plugin(ProbePlugin {
                allowed: calls.clone(),
                forbidden: Arc::new(AtomicUsize::new(0)),
                hooks: Arc::new(AtomicUsize::new(0)),
            })
            .agent_options(AgentOptions {
                active_tools: vec!["allowed".into()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let (abort, signal) = AbortHandle::new();
        let mut input = request(&["allowed"]);
        if mode == "timeout" {
            input.timeout = Duration::from_secs(1);
        }
        let mut run = Box::pin(runtime.run_ephemeral(input, signal));
        tokio::select! {
            result = &mut run => panic!("run ended before its second provider call: {result:?}"),
            () = async { while provider.requests().len() < 2 { tokio::task::yield_now().await; } } => {},
        }
        if mode == "cancel" {
            abort.abort();
        }
        let outcome = run.await.unwrap();
        assert_eq!(
            outcome.status,
            if mode == "cancel" {
                EphemeralSessionStatus::Aborted
            } else {
                EphemeralSessionStatus::TimedOut
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(outcome.messages.iter().any(
            |message| matches!(message, Message::ToolResult(result) if result.tool_name == "allowed" && !result.is_error)
        ));
        assert_balanced(&outcome.messages);
        assert!(runtime.agent().state().messages.is_empty());
    }
}

#[tokio::test]
async fn only_a_different_explicit_model_gets_independent_thinking_settings() {
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("alternate".into()),
        ScriptedTurn::Text("same".into()),
    ]);
    let provider = scripted.provider();
    let budgets = ThinkingBudgets {
        minimal: Some(100),
        low: Some(200),
        medium: Some(300),
        high: Some(400),
    };
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            thinking_level: ThinkingLevel::High,
            thinking_budgets: Some(budgets),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();

    let mut alternate = request(&[]);
    alternate.model = Some(ModelSelection::new("scripted", "review"));
    runtime
        .run_ephemeral(alternate, AbortHandle::new().1)
        .await
        .unwrap();
    let mut same = request(&[]);
    same.model = Some(ModelSelection::new("scripted", "test"));
    runtime
        .run_ephemeral(same, AbortHandle::new().1)
        .await
        .unwrap();

    let requests = provider.requests();
    assert_eq!(requests[0].model.as_str(), "review");
    assert_eq!(requests[0].thinking_level, ThinkingLevel::Off);
    assert_eq!(requests[0].thinking_budgets, None);
    assert_eq!(requests[1].thinking_level, ThinkingLevel::High);
    assert_eq!(requests[1].thinking_budgets, Some(budgets));
}

#[tokio::test]
async fn repeated_tool_calls_stop_at_the_iteration_budget() {
    let turns = (0..5).map(|index| {
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            format!("{index}"),
            "missing",
            json!({}),
        )])
    });
    let scripted = ScriptedProviderPlugin::scripted(turns);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .build()
        .unwrap();
    let mut input = request(&[]);
    input.max_tool_iterations = 2;
    let result = runtime
        .run_ephemeral(input, AbortHandle::new().1)
        .await
        .unwrap();
    assert!(matches!(result.status, EphemeralSessionStatus::Failed(_)));
    assert_eq!(provider.requests().len(), 2);
}

struct AuthProvider {
    inner: ScriptedProvider,
    observed: Mutex<Vec<BTreeMap<String, String>>>,
    signals: Mutex<Vec<AbortSignal>>,
}

fn long_history() -> Vec<Message> {
    let mut messages = vec![
        Message::User(UserMessage::text("Original task.", 0)),
        Message::User(UserMessage::text("Keep the user's constraints.", 0)),
    ];
    messages.extend((0..12).map(|index| {
        Message::User(UserMessage::text(
            format!(
                "Historical evidence {index}: {}",
                "verified result ".repeat(550)
            ),
            0,
        ))
    }));
    messages
}

fn compaction_request() -> EphemeralSessionRequest {
    let mut input = request(&[]);
    input.inherit_history = true;
    input.system_prompt = None;
    input.compaction = Some(pi_core::EphemeralCompactionOptions {
        threshold_tokens: 1_000,
        retained_head_messages: 2,
        retained_tail_messages: 1,
        retained_tail_tokens: 200,
        max_summary_tokens: 2_000,
    });
    input
}

fn review_tool_call() -> ScriptedTurn {
    ScriptedTurn::ToolCalls(vec![
        ToolCall::new("one", "missing", json!({})),
        ToolCall::new("two", "missing", json!({})),
    ])
}

fn assert_balanced(messages: &[Message]) {
    let mut pending = std::collections::HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(message) => {
                assert!(
                    pending.is_empty(),
                    "provider history has a dangling tool call"
                );
                for call in message.tool_calls() {
                    pending.insert(call.id);
                }
            }
            Message::ToolResult(message) => {
                assert!(pending.remove(&message.tool_call_id), "orphan tool result")
            }
            _ => assert!(pending.is_empty(), "a summary/request split a tool group"),
        }
    }
    assert!(pending.is_empty());
}

#[tokio::test]
async fn detached_compaction_preserves_first_replay_and_only_rewrites_private_context() {
    let scripted = ScriptedProviderPlugin::scripted([
        review_tool_call(),
        ScriptedTurn::Text("Verified procedure and constraints retained.".into()),
        ScriptedTurn::Text("Review completed.".into()),
    ]);
    let provider = scripted.provider();
    let history = long_history();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            messages: history.clone(),
            system_prompt: "Effective parent instructions".into(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    runtime.agent().set_session_id(Some("live-parent".into()));
    let events = Arc::new(AtomicUsize::new(0));
    runtime.agent().subscribe(Arc::new({
        let events = events.clone();
        move |_, _| {
            events.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        }
    }));
    let request = compaction_request();
    let current_prompt = request.messages[0].clone();
    let outcome = runtime
        .run_ephemeral(request, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].messages[..history.len()], history);
    assert_eq!(requests[0].system_prompt, "Effective parent instructions");
    assert!(
        requests[1].tools.is_empty(),
        "summary is a tool-free provider request"
    );
    assert!(
        requests[1]
            .system_prompt
            .contains("Summarize historical conversation")
    );
    assert_eq!(requests[0].tools, requests[2].tools);
    assert_eq!(requests[0].system_prompt, requests[2].system_prompt);
    assert_eq!(requests[2].session_id.as_deref(), Some("live-parent"));
    assert!(
        requests[2].messages.contains(&current_prompt),
        "the active request stays verbatim"
    );
    assert!(requests[2].messages.len() < requests[0].messages.len());
    assert_balanced(&requests[2].messages);
    assert_eq!(runtime.agent().state().messages, history);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    assert!(!outcome.messages.iter().any(|message| matches!(message, Message::User(m) if m.content.iter().any(|b| matches!(b, ContentBlock::Text(t) if t.text.contains("Private context summary"))))));
}

#[tokio::test]
async fn incomplete_or_failed_compaction_never_commits_a_partial_summary() {
    let truncated = ScriptedTurn::Events(vec![
        pi_core::StreamEvent::Start {
            metadata: pi_core::ResponseMetadata::new(
                "scripted".into(),
                "test".into(),
                "scripted",
                0,
            ),
        },
        pi_core::StreamEvent::TextStart { content_index: 0 },
        pi_core::StreamEvent::TextDelta {
            content_index: 0,
            delta: "This summary was cut off mid-procedure".into(),
        },
        pi_core::StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        pi_core::StreamEvent::Done {
            reason: pi_core::StopReason::Length,
            usage: pi_core::Usage::default(),
        },
    ]);
    for summary in [
        ScriptedTurn::Text(String::new()),
        ScriptedTurn::Error("summary unavailable".into()),
        review_tool_call(),
        truncated,
    ] {
        let scripted = ScriptedProviderPlugin::scripted([
            review_tool_call(),
            summary,
            ScriptedTurn::Text("must not run".into()),
        ]);
        let provider = scripted.provider();
        let history = long_history();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .agent_options(AgentOptions {
                messages: history.clone(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let outcome = runtime
            .run_ephemeral(compaction_request(), AbortHandle::new().1)
            .await
            .unwrap();
        assert!(
            matches!(outcome.status, EphemeralSessionStatus::Failed(reason) if reason.contains("compaction"))
        );
        assert_eq!(provider.requests().len(), 2);
        assert_balanced(&outcome.messages);
        assert_eq!(runtime.agent().state().messages, history);
    }
}

#[tokio::test]
async fn final_response_and_exhausted_iteration_budget_do_not_pay_for_compaction() {
    for turn in [
        ScriptedTurn::Text("Nothing to save.".into()),
        review_tool_call(),
    ] {
        let scripted = ScriptedProviderPlugin::scripted([turn]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .agent_options(AgentOptions {
                messages: long_history(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let mut request = compaction_request();
        request.max_tool_iterations = 1;
        runtime
            .run_ephemeral(request, AbortHandle::new().1)
            .await
            .unwrap();
        assert_eq!(provider.requests().len(), 1);
    }
}

#[tokio::test]
async fn compaction_summary_usage_shares_the_aggregate_input_budget() {
    use pi_core::{ResponseMetadata, StopReason, StreamEvent, Usage};
    let summary = ScriptedTurn::Events(vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new("scripted".into(), "test".into(), "scripted", 0),
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "Verified historical facts.".into(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Stop,
            usage: Usage {
                cache_read: 25_000,
                ..Usage::default()
            },
        },
    ]);
    let scripted = ScriptedProviderPlugin::scripted([
        review_tool_call(),
        summary,
        ScriptedTurn::Text("must not run".into()),
    ]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            messages: long_history(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let mut request = compaction_request();
    request.max_input_tokens = Some(20_000);
    let outcome = runtime
        .run_ephemeral(request, AbortHandle::new().1)
        .await
        .unwrap();
    assert!(
        matches!(outcome.status, EphemeralSessionStatus::Failed(reason) if reason.contains("input-token"))
    );
    assert_eq!(provider.requests().len(), 2);
    assert_balanced(&outcome.messages);
    assert_eq!(
        outcome.usage.cache_read, 25_000,
        "detached summary usage must be returned even though its message is private"
    );
    assert_eq!(
        outcome.api_calls, 2,
        "the review response and detached summary are both provider calls"
    );
}

#[tokio::test]
async fn cancellation_and_reload_during_summary_keep_parent_generation_and_history_safe() {
    for mode in ["cancel", "timeout", "drop"] {
        let provider = Arc::new(AuthProvider {
            inner: ScriptedProvider::new(
                "scripted".into(),
                "test".into(),
                [review_tool_call(), ScriptedTurn::WaitForAbort],
            ),
            observed: Mutex::new(Vec::new()),
            signals: Mutex::new(Vec::new()),
        });
        let history = long_history();
        let runtime = PiRuntime::builder()
            .provider_plugin(AuthPlugin {
                provider: provider.clone(),
                credential: Arc::new(Mutex::new("fixture".into())),
            })
            .agent_options(AgentOptions {
                messages: history.clone(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let old_context = runtime.plugin_context_handle(pi_core::PluginContextScope::Base);
        let (abort, signal) = AbortHandle::new();
        let mut request = compaction_request();
        if mode == "timeout" {
            request.timeout = Duration::from_secs(1);
        }
        let mut run = Box::pin(runtime.run_ephemeral(request, signal));
        tokio::select! {
            result = &mut run => panic!("review ended before summary: {result:?}"),
            () = async { while provider.inner.requests().len() < 2 { tokio::task::yield_now().await; } } => {},
        }
        runtime.reload().await.unwrap();
        assert!(old_context.access_for_adapter().is_ok());
        match mode {
            "cancel" => {
                abort.abort();
                assert_eq!(run.await.unwrap().status, EphemeralSessionStatus::Aborted);
            }
            "timeout" => assert_eq!(run.await.unwrap().status, EphemeralSessionStatus::TimedOut),
            _ => drop(run),
        }
        assert!(provider.signals.lock().unwrap()[1].is_aborted());
        assert_eq!(provider.inner.requests().len(), 2);
        assert_eq!(runtime.agent().state().messages, history);
        assert!(matches!(
            old_context.access_for_adapter(),
            Err(pi_core::PluginContextError::Retired)
        ));
        assert_eq!(
            provider.observed.lock().unwrap().len(),
            2,
            "summary reuses the provider/auth Adapter"
        );
    }
}

#[tokio::test]
async fn aggregate_input_budget_counts_cached_tokens_and_stops_after_balanced_tools() {
    use pi_core::{ResponseMetadata, StopReason, StreamEvent, Usage};
    let turn = || {
        ScriptedTurn::Events(vec![
            StreamEvent::Start {
                metadata: ResponseMetadata::new(
                    ProviderId::new("scripted"),
                    ModelId::new("test"),
                    "scripted",
                    0,
                ),
            },
            StreamEvent::ToolCallStart {
                content_index: 0,
                id: ToolCallId::new("probe"),
                name: "missing".into(),
            },
            StreamEvent::ToolCallDelta {
                content_index: 0,
                arguments_delta: "{}".into(),
            },
            StreamEvent::ToolCallEnd {
                content_index: 0,
                thought_signature: None,
            },
            StreamEvent::Done {
                reason: StopReason::ToolUse,
                usage: Usage {
                    input: 10,
                    cache_read: 30,
                    cache_write: 20,
                    ..Usage::default()
                },
            },
        ])
    };
    let scripted = ScriptedProviderPlugin::scripted([turn(), turn(), turn()]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .build()
        .unwrap();
    let mut request = request(&[]);
    request.max_input_tokens = Some(100);
    let outcome = runtime
        .run_ephemeral(request, AbortHandle::new().1)
        .await
        .unwrap();
    assert!(
        matches!(outcome.status, EphemeralSessionStatus::Failed(reason) if reason.contains("input-token"))
    );
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(outcome.usage.input, 20);
    assert_eq!(outcome.usage.cache_read, 60);
    assert_eq!(outcome.usage.cache_write, 40);
    assert_eq!(outcome.api_calls, 2);
    assert_eq!(
        outcome
            .messages
            .iter()
            .filter(|m| matches!(m, Message::ToolResult(_)))
            .count(),
        2
    );
}

#[tokio::test]
async fn inherited_history_uses_the_parents_message_converter() {
    let custom = Message::custom(pi_core::CustomMessage {
        custom_type: "context".to_string(),
        content: pi_core::CustomMessageContent::Text("retained context".to_string()),
        display: false,
        details: None,
        timestamp_ms: 1,
    });
    let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
    let provider = plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(plugin)
        .agent_options(AgentOptions {
            messages: vec![custom.clone()],
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    runtime
        .agent()
        .configure(pi_agent::AgentConfigurationPatch {
            convert_to_llm: Some(pi_agent::ConvertToLlm::new(|messages| async move {
                messages
                    .into_iter()
                    .map(Message::into_provider_message)
                    .collect()
            })),
            ..pi_agent::AgentConfigurationPatch::default()
        })
        .unwrap();
    let mut input = request(&[]);
    input.inherit_history = true;
    let outcome = runtime
        .run_ephemeral(input, AbortHandle::new().1)
        .await
        .unwrap();

    assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
    assert_eq!(
        provider.requests()[0].messages,
        [
            Message::User(UserMessage::text("retained context", 1)),
            Message::User(UserMessage::text("maintain", 0)),
        ]
    );
    assert_eq!(runtime.agent().state().messages, [custom]);
}

#[tokio::test]
async fn cold_history_digest_preserves_the_tool_group_at_the_cut() {
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new("call", "missing", json!({}))]),
        ScriptedTurn::Text("foreground done".into()),
        ScriptedTurn::Text("review done".into()),
    ]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .build()
        .unwrap();
    runtime
        .agent()
        .prompt(vec![Message::User(UserMessage::text("original task", 0))])
        .await
        .unwrap();
    let original = runtime.agent().state().messages;
    let mut request = request(&[]);
    request.inherit_history = true;
    request.history_tail = Some(2);
    runtime
        .run_ephemeral(request, AbortHandle::new().1)
        .await
        .unwrap();
    let replay = &provider.requests()[2].messages;
    assert!(
        matches!(&replay[0], Message::User(u) if matches!(&u.content[0], ContentBlock::Text(t) if t.text.contains("USER: original task")))
    );
    assert!(matches!(&replay[1], Message::Assistant(a) if a.tool_calls().len() == 1));
    assert!(matches!(&replay[2], Message::ToolResult(_)));
    assert_eq!(runtime.agent().state().messages, original);
}

#[async_trait]
impl Provider for AuthProvider {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let headers = context
            .before_provider_headers(&signal, BTreeMap::new())
            .await;
        self.observed.lock().unwrap().push(headers);
        self.signals.lock().unwrap().push(signal.clone());
        self.inner.stream(request, context, signal).await
    }
}

struct AuthPlugin {
    provider: Arc<AuthProvider>,
    credential: Arc<Mutex<String>>,
}

#[pi_core::provider_plugin]
impl ProviderPlugin for AuthPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("in-memory-auth-adapter")
    }
    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())
    }
    async fn before_provider_headers(
        &self,
        _: ProviderPluginContext,
        event: BeforeProviderHeadersEvent,
    ) -> Result<Option<BTreeMap<String, Option<String>>>, PluginError> {
        let mut headers = event.headers;
        headers.insert(
            "authorization".to_string(),
            Some(self.credential.lock().unwrap().clone()),
        );
        headers.insert(
            "x-subscription-route".to_string(),
            Some("fixture-account".to_string()),
        );
        Ok(Some(headers))
    }
}

#[tokio::test]
async fn registered_auth_adapter_and_request_time_credentials_are_reused() {
    let provider = Arc::new(AuthProvider {
        inner: ScriptedProvider::new(
            ProviderId::new("custom"),
            ModelId::new("test"),
            [
                ScriptedTurn::Text("one".to_string()),
                ScriptedTurn::Text("two".to_string()),
            ],
        ),
        observed: Mutex::new(Vec::new()),
        signals: Mutex::new(Vec::new()),
    });
    let credential = Arc::new(Mutex::new("fixture-token-one".to_string()));
    let runtime = PiRuntime::builder()
        .provider_plugin(AuthPlugin {
            provider: provider.clone(),
            credential: credential.clone(),
        })
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("custom"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    for expected in ["fixture-token-one", "fixture-token-two"] {
        *credential.lock().unwrap() = expected.to_string();
        let outcome = runtime
            .run_ephemeral(request(&[]), AbortHandle::new().1)
            .await
            .unwrap();
        assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
        let observed = provider.observed.lock().unwrap();
        assert_eq!(observed.last().unwrap()["authorization"], expected);
        assert_eq!(
            observed.last().unwrap()["x-subscription-route"],
            "fixture-account"
        );
        assert!(
            matches!(&outcome.messages.last().unwrap(), Message::Assistant(message) if matches!(&message.content[0], ContentBlock::Text(_)))
        );
    }
    assert_eq!(provider.inner.requests().len(), 2);
}
