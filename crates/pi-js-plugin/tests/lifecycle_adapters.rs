use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AgentSettledEvent, AssistantMessage, AssistantStream, AssistantStreamId,
    AssistantStreamView, BeforeAgentStartEvent, BeforeProviderRequestEvent, CommandContext,
    CommandOutcome, ContentBlock, CustomMessageContent, ImageContent, InputEvent, InputPatch,
    InputSource, InputStreamingBehavior, Message, MessageEndEvent, MessageUpdateEvent, ModelId,
    ModelsContextAccess, PluginContext, PluginContextEpoch, PluginContextError,
    PluginContextHandle, PluginId, PresentationMode, ProviderId, ProviderPluginContext,
    RegistriesBuilder, RunId, SessionContextAccess, StopReason, StreamEvent, TextContent,
    ToolResultMessage, TurnEndEvent, TurnStartEvent, UiContextAccess, Usage, UserMessage,
};
use pi_js_plugin::{
    ExtensionContextQuery, JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError,
    JsCommandManifest, JsGenerationManifest, JsHookBatchError, JsHookBatchInvocation,
    JsHookBatchResult, JsHookManifest, JsInvocation, JsPluginGeneration, JsProviderPluginManifest,
    JsSessionPluginManifest, JsStreamHookBatchInvocation, PluginContextScope,
    execute_context_query,
};
use pi_session::{
    SessionBeforeSwitchEvent, SessionIdentity, SessionPluginContext, SessionStartEvent,
    SessionStartReason, SessionSwitchReason,
};
use serde_json::{Value, json};

#[derive(Default)]
struct LifecycleDispatcher {
    invocations: Mutex<Vec<JsInvocation>>,
    scopes: Mutex<Vec<PluginContextScope>>,
    contexts: Mutex<Vec<PluginContextHandle>>,
}

#[derive(Default)]
struct HookBatchDispatcher {
    batches: Mutex<Vec<JsHookBatchInvocation>>,
    stream_batches: Mutex<Vec<JsStreamHookBatchInvocation>>,
}

#[async_trait]
impl JsCallbackDispatcher for HookBatchDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        panic!("observer callbacks should use one hook batch, got {invocation:?}")
    }

    async fn invoke_hook_batch(
        &self,
        invocation: JsHookBatchInvocation,
        _context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        self.batches.lock().unwrap().push(invocation);
        Ok(JsHookBatchResult {
            errors: vec![JsHookBatchError {
                callback_id: "message-update:first".to_string(),
                message: "intentional observer failure".to_string(),
            }],
        })
    }

    async fn invoke_stream_hook_batch(
        &self,
        invocation: JsStreamHookBatchInvocation,
        _context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        self.stream_batches.lock().unwrap().push(invocation);
        Ok(JsHookBatchResult {
            errors: vec![JsHookBatchError {
                callback_id: "message-update:first".to_string(),
                message: "intentional observer failure".to_string(),
            }],
        })
    }
}

struct FixedAssistantStream(AssistantMessage);

impl AssistantStreamView for FixedAssistantStream {
    fn snapshot(&self) -> Option<AssistantMessage> {
        Some(self.0.clone())
    }
}

fn assistant_stream(message: AssistantMessage) -> AssistantStream {
    AssistantStream::new(
        AssistantStreamId::new("test-stream"),
        Arc::new(FixedAssistantStream(message)),
    )
}

#[async_trait]
impl JsCallbackDispatcher for LifecycleDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        context: pi_js_plugin::PluginContextHandle,
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
        self.contexts.lock().unwrap().push(context);
        self.invocations.lock().unwrap().push(invocation);
        Ok(response)
    }
}

struct ModeContext;

impl ModelsContextAccess for ModeContext {}

#[async_trait]
impl SessionContextAccess for ModeContext {}

impl UiContextAccess for ModeContext {
    fn mode(&self) -> Result<PresentationMode, PluginContextError> {
        Ok(PresentationMode::Print)
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
            provider_registrations: Vec::new(),
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
async fn javascript_callbacks_use_the_runtime_plugin_context_epoch() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    let plugin_access: Arc<dyn PluginContext> = Arc::new(ModeContext);
    let epoch = PluginContextEpoch::new(plugin_access);
    let (driver, _, _) = RegistriesBuilder::new()
        .register_plugin_sets_with_context(
            generation.agent_plugins(),
            generation.provider_plugins(),
            epoch.clone(),
        )
        .unwrap();
    let (_, signal) = AbortHandle::new();

    driver
        .input(
            std::path::Path::new("/workspace"),
            &signal,
            InputEvent {
                text: "original".to_string(),
                images: None,
                source: InputSource::Rpc,
                streaming_behavior: None,
            },
        )
        .await
        .unwrap();

    let handle = dispatcher.contexts.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        execute_context_query(&handle, ExtensionContextQuery::Mode).unwrap(),
        json!("print")
    );
    epoch.retire();
    assert!(matches!(
        execute_context_query(&handle, ExtensionContextQuery::Mode),
        Err(PluginContextError::Retired)
    ));
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
                CommandContext::standalone("/workspace".into(), signal.clone()),
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
                ProviderPluginContext::unavailable_for_testing(
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
            &SessionPluginContext::unavailable_for_testing(
                PluginId::new("extension"),
                2,
                SessionIdentity {
                    id: "session".to_string(),
                    path: "/sessions/session.jsonl".into(),
                    cwd: "/workspace".into(),
                    parent_session_id: None,
                },
            ),
            &SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            },
        )
        .await
        .unwrap();

    let switch_result = generation.session_plugins()[0]
        .session_before_switch(
            &SessionPluginContext::unavailable_for_testing(
                PluginId::new("extension"),
                2,
                SessionIdentity {
                    id: "session".to_string(),
                    path: "/sessions/session.jsonl".into(),
                    cwd: "/workspace".into(),
                    parent_session_id: None,
                },
            ),
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
            PluginContextScope::Command,
            PluginContextScope::Base,
            PluginContextScope::Base,
            PluginContextScope::Base,
            PluginContextScope::Base,
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
                message: assistant("done").into(),
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

#[tokio::test]
async fn observer_hooks_share_one_generation_batch_and_one_compact_stream_encoding() {
    let dispatcher = Arc::new(HookBatchDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-observer-batch".to_string(),
            agent_plugins: vec![
                JsAgentPluginManifest {
                    id: "extension-first".to_string(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: vec![JsHookManifest {
                        name: "message_update".to_string(),
                        callback_id: "message-update:first".to_string(),
                    }],
                },
                JsAgentPluginManifest {
                    id: "extension-second".to_string(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: vec![JsHookManifest {
                        name: "message_update".to_string(),
                        callback_id: "message-update:second".to_string(),
                    }],
                },
            ],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    )
    .unwrap();
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.agent_plugins())
        .unwrap();
    assert_eq!(
        driver.plugin_order(),
        [
            PluginId::new("extension-first"),
            PluginId::new("extension-second")
        ]
    );

    let (_, signal) = AbortHandle::new();
    driver
        .message_update(
            &RunId::new("batch-run"),
            std::path::Path::new("/workspace"),
            &signal,
            MessageUpdateEvent {
                stream: assistant_stream(assistant("cumulative-message-only")),
                update: Arc::new(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: "delta".to_string(),
                }),
            },
        )
        .await;

    let batches = dispatcher.stream_batches.lock().unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(
        batch
            .callbacks
            .iter()
            .map(|callback| callback.callback_id.as_str())
            .collect::<Vec<_>>(),
        ["message-update:first", "message-update:second"]
    );
    assert_eq!(batch.callbacks[0].context["pluginId"], "extension-first");
    assert_eq!(batch.callbacks[1].context["pluginId"], "extension-second");
    assert_eq!(
        batch.initial_message.as_ref().unwrap().content[0],
        ContentBlock::Text(TextContent::new("cumulative-message-only"))
    );
    assert_eq!(
        *batch.update,
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "delta".to_string(),
        }
    );
    assert_eq!(
        serde_json::to_string(batch)
            .unwrap()
            .matches("cumulative-message-only")
            .count(),
        1
    );
    drop(batches);

    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.plugin_id == PluginId::new("extension-first")
            && diagnostic.hook == "message_update"
            && diagnostic.message.contains("intentional observer failure")
    }));
}
