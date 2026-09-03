use super::*;
use pi_core::{
    AgentEndEvent, AgentSettledEvent, AgentStartEvent, ContextEvent, ContextPatch, InputContext,
    InputEvent, InputPatch, RunId, ToolCallBlock, ToolCallEvent, ToolCallPatch, ToolResultEvent,
    ToolResultPatch, TurnEndEvent, TurnStartEvent,
};

#[derive(Default)]
struct Trace(Mutex<Vec<(RunId, String)>>);

impl Trace {
    fn record(&self, run: &RunId, event: String) {
        self.0.lock().unwrap().push((run.clone(), event));
    }
}

struct HookTool {
    name: &'static str,
    trace: Arc<Trace>,
}

#[async_trait]
impl Tool for HookTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            label: self.name.into(),
            description: "Hook test".into(),
            parameters: json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn prepare_arguments(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<Value, ToolError> {
        self.trace.record(
            context.run_id().unwrap(),
            format!("prepare:{}:{}", self.name, args["value"]),
        );
        Ok(args)
    }

    fn validate_arguments(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("value").is_some_and(Value::is_string) {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments("value must be a string".into()))
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        args: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        assert!(context.session.id().is_err());
        assert!(context.models.current().is_err());
        self.trace.record(
            context.run_id().unwrap(),
            format!("execute:{}:{}", self.name, args["value"]),
        );
        Ok(ToolResult::text("original result"))
    }
}

struct ParentPlugin(Arc<Trace>);

#[pi_core::agent_plugin]
impl AgentPlugin for ParentPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("parent-tool-hooks")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for name in ["allowed", "forbidden"] {
            context.register_tool(Arc::new(HookTool {
                name,
                trace: self.0.clone(),
            }))?;
        }
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _: AgentPluginContext,
        _: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        panic!("the private Agent must never inherit parent prompt hooks")
    }

    async fn tool_call(
        &self,
        _: AgentPluginContext,
        _: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        panic!("the private Agent must never invoke parent tool hooks")
    }

    async fn tool_result(
        &self,
        _: AgentPluginContext,
        _: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        panic!("the private Agent must never invoke parent tool hooks")
    }
}

struct PrivatePlugin(Arc<Trace>);

#[pi_core::agent_plugin]
impl AgentPlugin for PrivatePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("private-tool-hooks")
    }

    fn register(&self, _: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        panic!("private hook attachments must not mutate generation registries")
    }

    async fn tool_call(
        &self,
        context: AgentPluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        assert!(context.session.id().is_err());
        assert!(context.models.current().is_err());
        self.0.record(
            context.run_id(),
            format!("call:{}", event.validated_args["value"]),
        );
        Ok(match event.validated_args["value"].as_str().unwrap() {
            "patch" => ToolCallPatch {
                arguments: Some(json!({"value":"patched"})),
                ..ToolCallPatch::default()
            },
            "invalid-patch" => ToolCallPatch {
                arguments: Some(json!({"value":42})),
                ..ToolCallPatch::default()
            },
            "block" => ToolCallPatch {
                block: Some(ToolCallBlock {
                    reason: "private guard denied this".into(),
                    terminate: false,
                }),
                ..ToolCallPatch::default()
            },
            "error" => {
                return Err(PluginError::Hook {
                    plugin_id: self.id(),
                    hook: "tool_call",
                    message: "private hook failure".into(),
                });
            }
            _ => ToolCallPatch::default(),
        })
    }

    async fn tool_result(
        &self,
        context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        self.0.record(
            context.run_id(),
            format!("result:{}", event.validated_args["value"]),
        );
        assert_eq!(
            event.result.content,
            ToolResult::text("original result").content
        );
        Ok(ToolResultPatch {
            content: Some(ToolResult::text("private receipt").content),
            ..ToolResultPatch::default()
        })
    }
}

struct LifecyclePlugin {
    name: &'static str,
    trace: Arc<Trace>,
}

impl LifecyclePlugin {
    fn record(&self, context: &AgentPluginContext, hook: &str) {
        assert!(context.session.id().is_err());
        assert!(context.models.current().is_err());
        self.trace
            .record(context.run_id(), format!("{hook}:{}", self.name));
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for LifecyclePlugin {
    fn id(&self) -> PluginId {
        PluginId::new(self.name)
    }

    fn register(&self, _: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        panic!("private hook attachments must not mutate generation registries")
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        self.record(&context, "before");
        tokio::task::yield_now().await;
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!("{}\nprivate:{}", event.system_prompt, self.name)),
            ..BeforeAgentStartPatch::default()
        })
    }

    async fn agent_start(
        &self,
        context: AgentPluginContext,
        _: AgentStartEvent,
    ) -> Result<(), PluginError> {
        self.record(&context, "start");
        Ok(())
    }

    async fn turn_start(
        &self,
        context: AgentPluginContext,
        _: TurnStartEvent,
    ) -> Result<(), PluginError> {
        self.record(&context, "turn_start");
        Ok(())
    }

    async fn context(
        &self,
        context: AgentPluginContext,
        _: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        self.record(&context, "context");
        Ok(ContextPatch::default())
    }

    async fn turn_end(
        &self,
        context: AgentPluginContext,
        _: TurnEndEvent,
    ) -> Result<(), PluginError> {
        self.record(&context, "turn_end");
        Ok(())
    }

    async fn agent_end(
        &self,
        context: AgentPluginContext,
        _: AgentEndEvent,
    ) -> Result<(), PluginError> {
        self.record(&context, "end");
        Ok(())
    }

    async fn input(&self, _: InputContext, _: InputEvent) -> Result<InputPatch, PluginError> {
        panic!("structured ephemeral prompts do not run the product input pipeline")
    }

    async fn agent_settled(
        &self,
        _: AgentPluginContext,
        _: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        panic!("a bare Agent has no session-level settled event")
    }
}

#[tokio::test]
async fn private_plugins_receive_normal_agent_hooks_in_order_without_parent_or_session_hooks() {
    let trace = Arc::new(Trace::default());
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "patch",
            "allowed",
            json!({"value":"patch"}),
        )]),
        ScriptedTurn::Text("done".into()),
    ]);
    let provider = scripted.provider();
    let history = vec![Message::User(UserMessage::text("parent history", 0))];
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_plugin(ParentPlugin(trace.clone()))
        .agent_options(AgentOptions {
            active_tools: vec!["allowed".into(), "forbidden".into()],
            system_prompt: "Parent instructions".into(),
            messages: history.clone(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let mut input = request(&["allowed"]);
    input.system_prompt = None;
    input.inherit_history = true;
    input.plugins = vec![
        Arc::new(LifecyclePlugin {
            name: "a",
            trace: trace.clone(),
        }),
        Arc::new(PrivatePlugin(trace.clone())),
        Arc::new(LifecyclePlugin {
            name: "b",
            trace: trace.clone(),
        }),
    ];
    let outcome = runtime
        .run_ephemeral(input, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
    let trace = trace.0.lock().unwrap();
    assert!(trace.iter().all(|(run_id, _)| run_id == &trace[0].0));
    assert_eq!(
        trace
            .iter()
            .map(|(_, event)| event.as_str())
            .collect::<Vec<_>>(),
        [
            "before:a",
            "before:b",
            "start:a",
            "start:b",
            "turn_start:a",
            "turn_start:b",
            "context:a",
            "context:b",
            "prepare:allowed:\"patch\"",
            "call:\"patch\"",
            "execute:allowed:\"patched\"",
            "result:\"patched\"",
            "turn_end:a",
            "turn_end:b",
            "turn_start:a",
            "turn_start:b",
            "context:a",
            "context:b",
            "turn_end:a",
            "turn_end:b",
            "end:a",
            "end:b",
        ]
    );
    assert_eq!(
        provider.requests()[0].system_prompt,
        "Parent instructions\nprivate:a\nprivate:b"
    );
    assert_eq!(provider.requests()[0].messages[0], history[0]);
    assert_eq!(
        runtime.agent().effective_system_prompt(),
        "Parent instructions"
    );
    assert_eq!(runtime.agent().state().messages, history);
}

#[tokio::test]
async fn explicit_tool_hooks_share_execution_identity_and_preserve_guards_and_initial_validation() {
    let trace = Arc::new(Trace::default());
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![
            ToolCall::new("patch", "allowed", json!({"value":"patch"})),
            ToolCall::new("block", "allowed", json!({"value":"block"})),
            ToolCall::new("invalid-patch", "allowed", json!({"value":"invalid-patch"})),
            ToolCall::new("invalid-input", "allowed", json!({"value":42})),
            ToolCall::new("hook-error", "allowed", json!({"value":"error"})),
            ToolCall::new("scope", "forbidden", json!({"value":"patch"})),
        ]),
        ScriptedTurn::Text("done".into()),
    ]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_plugin(ParentPlugin(trace.clone()))
        .agent_options(AgentOptions {
            active_tools: vec!["allowed".into(), "forbidden".into()],
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let active = runtime.agent().state().active_tools;
    let parent_tools = runtime
        .current_generation()
        .agent
        .registries()
        .tool_specs(&active)
        .unwrap();
    let mut input = request(&["allowed"]);
    input.plugins.push(Arc::new(PrivatePlugin(trace.clone())));
    let result = runtime
        .run_ephemeral(input, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(result.status, EphemeralSessionStatus::Completed);
    let trace = trace.0.lock().unwrap();
    assert!(trace.iter().all(|(run_id, _)| run_id == &trace[0].0));
    assert_eq!(
        trace
            .iter()
            .map(|(_, event)| event.as_str())
            .collect::<Vec<_>>(),
        [
            "prepare:allowed:\"patch\"",
            "call:\"patch\"",
            "execute:allowed:\"patched\"",
            "result:\"patched\"",
            "prepare:allowed:\"block\"",
            "call:\"block\"",
            "prepare:allowed:\"invalid-patch\"",
            "call:\"invalid-patch\"",
            "execute:allowed:42",
            "result:42",
            "prepare:allowed:42",
            "prepare:allowed:\"error\"",
            "call:\"error\"",
        ]
    );
    let receipts = result
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 6);
    assert!(!receipts[0].is_error);
    assert_eq!(
        receipts[0].content,
        ToolResult::text("private receipt").content
    );
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.is_error)
            .collect::<Vec<_>>(),
        [false, true, false, true, true, true]
    );
    assert_eq!(
        receipts[2].content,
        ToolResult::text("private receipt").content
    );
    assert_eq!(provider.requests()[0].tools, parent_tools);
    assert_eq!(
        runtime
            .current_generation()
            .agent
            .registries()
            .tool_specs(&active)
            .unwrap(),
        parent_tools
    );
    assert!(runtime.agent().state().messages.is_empty());
}

#[tokio::test]
async fn duplicate_private_plugin_ids_fail_before_any_provider_request_or_registration() {
    let scripted = ScriptedProviderPlugin::scripted([]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .build()
        .unwrap();
    let trace = Arc::new(Trace::default());
    let mut input = request(&[]);
    input.plugins = vec![
        Arc::new(PrivatePlugin(trace.clone())),
        Arc::new(PrivatePlugin(trace.clone())),
    ];
    assert!(
        runtime
            .run_ephemeral(input, AbortHandle::new().1)
            .await
            .is_err()
    );
    assert!(provider.requests().is_empty());
    assert!(trace.0.lock().unwrap().is_empty());
}

struct LifetimePlugin {
    _lifetime: Arc<()>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for LifetimePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("private-lifetime")
    }
    async fn tool_call(
        &self,
        _: AgentPluginContext,
        _: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        Ok(ToolCallPatch::default())
    }
}

#[tokio::test]
async fn private_plugins_drop_on_all_exit_paths_and_do_not_pin_retired_generations() {
    for mode in ["complete", "error", "timeout", "cancel", "drop", "reload"] {
        let scripted = ScriptedProviderPlugin::scripted([match mode {
            "complete" => ScriptedTurn::Text("done".into()),
            "error" => ScriptedTurn::Error("provider failed".into()),
            _ => ScriptedTurn::WaitForAbort,
        }]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .build()
            .unwrap();
        let old_context = runtime.plugin_context_handle(pi_core::PluginContextScope::Base);
        let lifetime = Arc::new(());
        let weak = Arc::downgrade(&lifetime);
        let mut input = request(&[]);
        input.plugins.push(Arc::new(LifetimePlugin {
            _lifetime: lifetime,
        }));
        if mode == "timeout" {
            input.timeout = Duration::from_millis(30);
        }
        let (abort, signal) = AbortHandle::new();
        let mut run = Box::pin(runtime.run_ephemeral(input, signal));
        if matches!(mode, "complete" | "error") {
            let outcome = run.await.unwrap();
            if mode == "complete" {
                assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
            } else {
                assert!(matches!(outcome.status, EphemeralSessionStatus::Failed(_)));
            }
        } else {
            tokio::select! {
                result = &mut run => panic!("private run ended prematurely: {result:?}"),
                () = async { while provider.requests().is_empty() { tokio::task::yield_now().await; } } => {},
            }
            assert!(weak.upgrade().is_some());
            if mode == "reload" {
                runtime.reload().await.unwrap();
                assert!(old_context.access_for_adapter().is_ok());
            }
            if mode == "drop" {
                drop(run);
            } else {
                if mode != "timeout" {
                    abort.abort();
                }
                assert_eq!(
                    run.await.unwrap().status,
                    if mode == "timeout" {
                        EphemeralSessionStatus::TimedOut
                    } else {
                        EphemeralSessionStatus::Aborted
                    }
                );
            }
        }
        assert!(
            weak.upgrade().is_none(),
            "{mode} retained an invocation-private plugin"
        );
        if mode == "reload" {
            assert!(matches!(
                old_context.access_for_adapter(),
                Err(pi_core::PluginContextError::Retired)
            ));
        }
    }
}
