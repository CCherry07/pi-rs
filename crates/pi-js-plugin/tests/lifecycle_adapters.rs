use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AgentSettledEvent, AssistantMessage, BeforeAgentStartEvent,
    BeforeProviderRequestEvent, CommandContext, CommandOutcome, ContentBlock, CustomMessageContent,
    ImageContent, InputEvent, InputPatch, InputSource, InputStreamingBehavior, Message,
    MessageEndEvent, ModelId, PluginId, ProviderId, ProviderPluginContext, RegistriesBuilder,
    RunId, StopReason, TextContent, ToolResultMessage, TurnEndEvent, TurnStartEvent, Usage,
    UserMessage,
};
use pi_js_plugin::{
    ExtensionContextScope, JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError,
    JsCommandManifest, JsGenerationManifest, JsHookManifest, JsInvocation, JsPluginGeneration,
    JsProviderPluginManifest, JsSessionPluginManifest,
};
use pi_session::{
    SessionBeforeSwitchEvent, SessionIdentity, SessionPluginContext, SessionStartEvent,
    SessionStartReason, SessionSwitchReason,
};
use serde_json::{Value, json};

#[derive(Default)]
struct LifecycleDispatcher {
    invocations: Mutex<Vec<JsInvocation>>,
    scopes: Mutex<Vec<ExtensionContextScope>>,
}

#[async_trait]
impl JsCallbackDispatcher for LifecycleDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        context: pi_js_plugin::ExtensionContextHandle,
    ) -> Result<Value, JsCallbackError> {
        let response = match invocation.callback_id.as_str() {
            "command" => json!({ "action": "transform", "text": "from command" }),
            "input-fail" | "before-agent-start-fail" | "agent-settled-fail" | "provider-fail" => {
                return Err(JsCallbackError::new("intentional callback failure"));
            }
            "input" => json!({
                "action": "transform",
                "text": "from input",
                "images": [{"type": "image", "data": "cmVwbGFjZWQ=", "mimeType": "image/jpeg"}]
            }),
            "before-agent-start" => json!({
                "message": {
                    "customType": "fixture-context",
                    "content": "injected context",
                    "display": false
                },
                "systemPrompt": "rewritten prompt"
            }),
            "before-agent-start-second" => json!({
                "message": {
                    "customType": "fixture-context-second",
                    "content": [{"type": "text", "text": "second context"}],
                    "display": true,
                    "details": {"order": 2}
                },
                "systemPrompt": format!(
                    "{}|second",
                    invocation.payload["event"]["systemPrompt"]
                        .as_str()
                        .expect("second hook receives the chained system prompt")
                )
            }),
            "agent-settled" | "turn-start" | "turn-end" => Value::Null,
            "message-end" => {
                let mut message = invocation.payload["event"]["message"].clone();
                message["content"] = json!([{"type": "text", "text": "replaced at message_end"}]);
                json!({"message": message})
            }
            "provider" => json!({ "model": "rewritten" }),
            "session" => Value::Null,
            "session-before-switch" => json!({ "cancel": true }),
            callback => {
                return Err(JsCallbackError::new(format!(
                    "unexpected callback {callback}"
                )));
            }
        };
        self.scopes.lock().unwrap().push(context.scope());
        self.invocations.lock().unwrap().push(invocation);
        Ok(response)
    }
}

fn generation(dispatcher: Arc<LifecycleDispatcher>) -> JsPluginGeneration {
    JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-lifecycle".to_string(),
            agent_plugins: vec![JsAgentPluginManifest {
                id: "extension".to_string(),
                tools: Vec::new(),
                commands: vec![JsCommandManifest {
                    callback_id: "command".to_string(),
                    name: "rewrite".to_string(),
                    description: "Rewrite input".to_string(),
                    argument_hint: None,
                }],
                hooks: vec![
                    JsHookManifest {
                        name: "input".to_string(),
                        callback_id: "input-fail".to_string(),
                    },
                    JsHookManifest {
                        name: "input".to_string(),
                        callback_id: "input".to_string(),
                    },
                    JsHookManifest {
                        name: "before_agent_start".to_string(),
                        callback_id: "before-agent-start-fail".to_string(),
                    },
                    JsHookManifest {
                        name: "before_agent_start".to_string(),
                        callback_id: "before-agent-start".to_string(),
                    },
                    JsHookManifest {
                        name: "before_agent_start".to_string(),
                        callback_id: "before-agent-start-second".to_string(),
                    },
                    JsHookManifest {
                        name: "agent_settled".to_string(),
                        callback_id: "agent-settled-fail".to_string(),
                    },
                    JsHookManifest {
                        name: "agent_settled".to_string(),
                        callback_id: "agent-settled".to_string(),
                    },
                    JsHookManifest {
                        name: "turn_start".to_string(),
                        callback_id: "turn-start".to_string(),
                    },
                    JsHookManifest {
                        name: "turn_end".to_string(),
                        callback_id: "turn-end".to_string(),
                    },
                    JsHookManifest {
                        name: "message_end".to_string(),
                        callback_id: "message-end".to_string(),
                    },
                ],
            }],
            provider_plugins: vec![JsProviderPluginManifest {
                id: "extension".to_string(),
                hooks: vec![
                    JsHookManifest {
                        name: "before_provider_request".to_string(),
                        callback_id: "provider-fail".to_string(),
                    },
                    JsHookManifest {
                        name: "before_provider_request".to_string(),
                        callback_id: "provider".to_string(),
                    },
                ],
            }],
            session_plugins: vec![JsSessionPluginManifest {
                id: "extension".to_string(),
                hooks: vec![
                    JsHookManifest {
                        name: "session_start".to_string(),
                        callback_id: "session".to_string(),
                    },
                    JsHookManifest {
                        name: "session_before_switch".to_string(),
                        callback_id: "session-before-switch".to_string(),
                    },
                ],
            }],
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap()
}

fn assistant(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text(TextContent::new(text))],
        api: "test".to_string(),
        provider: ProviderId::new("provider"),
        model: ModelId::new("model"),
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

#[tokio::test]
async fn before_agent_start_exposes_pi_prompt_and_images_to_javascript() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.agent_plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();

    let patch = driver
        .before_agent_start(
            &RunId::new("run"),
            std::path::Path::new("/workspace"),
            &signal,
            BeforeAgentStartEvent {
                system_prompt: "base prompt".to_string(),
                input_messages: vec![Message::User(UserMessage {
                    content: vec![
                        ContentBlock::Text(TextContent::new("flux: review")),
                        ContentBlock::Image(ImageContent {
                            data: "YWJj".to_string(),
                            mime_type: "image/png".to_string(),
                        }),
                    ],
                    timestamp_ms: 1,
                })],
                active_tools: vec!["read".to_string()],
                provider_id: ProviderId::new("provider"),
                model_id: ModelId::new("model"),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        patch.system_prompt.as_deref(),
        Some("rewritten prompt|second")
    );
    assert!(matches!(
        patch.messages.as_slice(),
        [Message::Custom(first), Message::Custom(second)]
            if first.custom_type == "fixture-context"
                && first.content == CustomMessageContent::Text("injected context".to_string())
                && !first.display
                && second.custom_type == "fixture-context-second"
                && second.content == CustomMessageContent::Blocks(vec![ContentBlock::Text(
                    TextContent::new("second context")
                )])
                && second.display
                && second.details == Some(json!({"order": 2}))
    ));

    let invocations = dispatcher.invocations.lock().unwrap();
    assert_eq!(invocations.len(), 2);
    assert_eq!(invocations[0].payload["event"]["prompt"], "flux: review");
    assert_eq!(
        invocations[0].payload["event"]["systemPromptOptions"]["cwd"],
        "/workspace"
    );
    assert_eq!(
        invocations[0].payload["event"]["images"],
        json!([{"type": "image", "data": "YWJj", "mimeType": "image/png"}])
    );
    assert_eq!(
        invocations[1].payload["event"]["systemPrompt"],
        "rewritten prompt"
    );
    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook == "before_agent_start"
            && diagnostic.message.contains("intentional callback failure")
    }));
}

#[tokio::test]
async fn one_javascript_source_materializes_as_three_narrow_plugin_lifecycles() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    let (driver, registries) = RegistriesBuilder::new()
        .register_plugins(generation.agent_plugins())
        .unwrap();
    let (abort_handle, signal) = AbortHandle::new();

    let command = registries.command("rewrite").unwrap();
    assert_eq!(
        command
            .execute(
                CommandContext {
                    cwd: "/workspace".into(),
                    abort_signal: signal.clone(),
                },
                "original".to_string(),
            )
            .await
            .unwrap(),
        CommandOutcome::TransformInput("from command".to_string())
    );
    assert_eq!(
        driver
            .input(
                std::path::Path::new("/workspace"),
                &signal,
                InputEvent {
                    text: "original".to_string(),
                    images: Some(vec![ImageContent {
                        data: "b3JpZ2luYWw=".to_string(),
                        mime_type: "image/png".to_string(),
                    }]),
                    source: InputSource::Rpc,
                    streaming_behavior: Some(InputStreamingBehavior::FollowUp),
                },
            )
            .await
            .unwrap(),
        InputPatch::Transform {
            text: "from input".to_string(),
            images: Some(vec![ImageContent {
                data: "cmVwbGFjZWQ=".to_string(),
                mime_type: "image/jpeg".to_string(),
            }]),
        }
    );

    let provider = &generation.provider_plugins()[0];
    assert_eq!(
        provider
            .before_provider_request(
                ProviderPluginContext::new(
                    PluginId::new("extension"),
                    3,
                    ProviderId::new("provider"),
                    ModelId::new("model"),
                    "/workspace".into(),
                    signal,
                ),
                BeforeProviderRequestEvent {
                    payload: json!({ "model": "original" }),
                },
            )
            .await
            .unwrap(),
        Some(json!({ "model": "rewritten" }))
    );

    generation.session_plugins()[0]
        .session_start(
            &SessionPluginContext {
                plugin_id: PluginId::new("extension"),
                generation: 2,
                session: SessionIdentity {
                    id: "session".to_string(),
                    path: "/sessions/session.jsonl".into(),
                    cwd: "/workspace".into(),
                    parent_session_id: None,
                },
            },
            &SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            },
        )
        .await
        .unwrap();

    let switch_result = generation.session_plugins()[0]
        .session_before_switch(
            &SessionPluginContext {
                plugin_id: PluginId::new("extension"),
                generation: 2,
                session: SessionIdentity {
                    id: "session".to_string(),
                    path: "/sessions/session.jsonl".into(),
                    cwd: "/workspace".into(),
                    parent_session_id: None,
                },
            },
            &SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::Resume,
                target_session_file: Some("/sessions/next.jsonl".into()),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(switch_result.cancel);

    abort_handle.abort();
    let invocations = dispatcher.invocations.lock().unwrap();
    assert_eq!(invocations.len(), 5);
    drop(invocations);
    assert_eq!(
        *dispatcher.scopes.lock().unwrap(),
        [
            ExtensionContextScope::Command,
            ExtensionContextScope::Base,
            ExtensionContextScope::Base,
            ExtensionContextScope::Base,
            ExtensionContextScope::Base,
        ]
    );

    let invocations = dispatcher.invocations.lock().unwrap();
    let input = invocations
        .iter()
        .find(|invocation| invocation.callback_id == "input")
        .unwrap();
    assert_eq!(input.payload["event"]["source"], "rpc");
    assert_eq!(input.payload["event"]["streamingBehavior"], "followUp");
    assert_eq!(
        input.payload["event"]["images"],
        json!([{"type": "image", "data": "b3JpZ2luYWw=", "mimeType": "image/png"}])
    );
    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook == "input" && diagnostic.message.contains("intentional callback failure")
    }));
}

#[tokio::test]
async fn turn_metadata_and_message_end_replacement_match_pi() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.agent_plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();
    let run_id = RunId::new("run");
    let cwd = std::path::Path::new("/workspace");

    driver
        .turn_start(
            &run_id,
            cwd,
            &signal,
            TurnStartEvent {
                turn_index: 3,
                timestamp_ms: 1_700_000_000_000,
            },
        )
        .await;
    driver
        .turn_end(
            &run_id,
            cwd,
            &signal,
            TurnEndEvent {
                turn_index: 3,
                message: assistant("done"),
                tool_results: Vec::<ToolResultMessage>::new(),
            },
        )
        .await;
    let replacement = driver
        .message_end(
            &run_id,
            cwd,
            &signal,
            MessageEndEvent {
                message: Message::User(UserMessage::text("original", 1)),
            },
        )
        .await;

    assert!(matches!(replacement, Message::User(user)
        if matches!(&user.content[0], ContentBlock::Text(text)
            if text.text == "replaced at message_end")));
    let invocations = dispatcher.invocations.lock().unwrap();
    let turn_start = invocations
        .iter()
        .find(|invocation| invocation.callback_id == "turn-start")
        .unwrap();
    assert_eq!(turn_start.payload["event"]["turnIndex"], 3);
    assert_eq!(
        turn_start.payload["event"]["timestamp"],
        1_700_000_000_000_i64
    );
    let turn_end = invocations
        .iter()
        .find(|invocation| invocation.callback_id == "turn-end")
        .unwrap();
    assert_eq!(turn_end.payload["event"]["turnIndex"], 3);
}

#[tokio::test]
async fn agent_settled_is_active_and_isolates_callback_failures() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.agent_plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();

    driver
        .agent_settled(
            &RunId::new("settled-run"),
            std::path::Path::new("/workspace"),
            &signal,
            AgentSettledEvent,
        )
        .await;

    let invocations = dispatcher.invocations.lock().unwrap();
    let settled = invocations
        .iter()
        .find(|invocation| invocation.callback_id == "agent-settled")
        .expect("later agent_settled callback should still run");
    assert_eq!(settled.payload["event"], json!({"type": "agent_settled"}));
    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook == "agent_settled"
            && diagnostic.message.contains("intentional callback failure")
    }));
}
