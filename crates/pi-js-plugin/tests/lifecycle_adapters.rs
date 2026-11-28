use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AssistantMessage, AssistantStream, AssistantStreamId, AssistantStreamView,
    ContentBlock, CustomMessageContent, ImageContent, Message, ModelId, PluginId, ProviderId,
    RunId, StopReason, StreamEvent, TextContent, ToolResultMessage, Usage, UserMessage,
};
use pi_js_plugin::{
    ExtensionContextQuery, JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError,
    JsCommandManifest, JsGenerationManifest, JsHookBatchError, JsHookBatchInvocation,
    JsHookBatchResult, JsHookManifest, JsHookSnapshot, JsInvocation, JsPluginGeneration,
    JsProviderPluginManifest, JsSessionPluginManifest, JsStreamHookBatchInvocation,
    PluginContextScope, execute_context_query,
};
use pi_plugin::{
    AgentSettledEvent, BeforeAgentStartEvent, BeforeProviderRequestEvent, CommandContext,
    CommandOutcome, InputEvent, InputPatch, InputSource, InputStreamingBehavior, MessageEndEvent,
    MessageUpdateEvent, ModelsContextAccess, PluginContext, PluginContextEpoch, PluginContextError,
    PluginContextHandle, PresentationMode, ProviderPluginContext, RegistriesBuilder,
    SessionContextAccess, TurnEndEvent, TurnStartEvent, UiContextAccess,
};
use pi_plugin::{
    SessionBeforeSwitchEvent, SessionDispatchContext, SessionIdentity, SessionPluginContext,
    SessionStartEvent, SessionStartReason, SessionSwitchReason,
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

struct DynamicHookDispatcher {
    snapshots: Mutex<Vec<Vec<JsHookSnapshot>>>,
    invocations: Mutex<Vec<String>>,
}

struct DynamicCrossLifecycleDispatcher {
    snapshots: Mutex<Vec<(&'static str, Vec<JsHookSnapshot>)>>,
    invocations: Mutex<Vec<JsInvocation>>,
}

#[async_trait]
impl JsCallbackDispatcher for DynamicCrossLifecycleDispatcher {
    async fn snapshot_hooks(
        &self,
        _generation_id: &str,
        hook: &str,
    ) -> Result<Option<Vec<JsHookSnapshot>>, JsCallbackError> {
        let (expected, snapshot) = self.snapshots.lock().unwrap().remove(0);
        assert_eq!(hook, expected);
        Ok(Some(snapshot))
    }

    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.invocations.lock().unwrap().push(invocation.clone());
        let result = match invocation.callback_id.as_str() {
            "dynamic-provider" => json!({"dynamic": true}),
            "failing-session" => {
                return Err(JsCallbackError::new("first session handler failed"));
            }
            "dynamic-session" => json!({"cancel": true}),
            callback => panic!("unexpected dynamic callback: {callback}"),
        };
        Ok(result)
    }
}

#[async_trait]
impl JsCallbackDispatcher for DynamicHookDispatcher {
    async fn snapshot_hooks(
        &self,
        _generation_id: &str,
        hook: &str,
    ) -> Result<Option<Vec<JsHookSnapshot>>, JsCallbackError> {
        assert_eq!(hook, "agent_end");
        Ok(Some(self.snapshots.lock().unwrap().remove(0)))
    }

    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.invocations
            .lock()
            .unwrap()
            .push(invocation.callback_id);
        Ok(Value::Null)
    }
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
            dynamic_hooks: false,
            agent_plugins: vec![JsAgentPluginManifest {
                id: "extension".to_string(),
                tools: Vec::new(),
                commands: vec![JsCommandManifest {
                    callback_id: "command".to_string(),
                    name: "rewrite".to_string(),
                    invocation_name: "rewrite".to_string(),
                    description: "Rewrite input".to_string(),
                    argument_hint: None,
                    source_info: None,
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
            mcp_server_registrations: Vec::new(),
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

#[tokio::test]
async fn dynamic_hook_snapshots_replace_the_frozen_manifest_callbacks() {
    let dispatcher = Arc::new(DynamicHookDispatcher {
        snapshots: Mutex::new(vec![
            vec![
                JsHookSnapshot {
                    source_path: None,
                    plugin_id: "extension".to_string(),
                    callback_id: "a".to_string(),
                },
                JsHookSnapshot {
                    source_path: None,
                    plugin_id: "extension".to_string(),
                    callback_id: "b".to_string(),
                },
            ],
            vec![
                JsHookSnapshot {
                    source_path: None,
                    plugin_id: "extension".to_string(),
                    callback_id: "b".to_string(),
                },
                JsHookSnapshot {
                    source_path: None,
                    plugin_id: "extension".to_string(),
                    callback_id: "c".to_string(),
                },
            ],
        ]),
        invocations: Mutex::new(Vec::new()),
    });
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "dynamic-hooks".to_string(),
            dynamic_hooks: true,
            agent_plugins: vec![JsAgentPluginManifest {
                id: "extension".to_string(),
                tools: Vec::new(),
                commands: Vec::new(),
                hooks: Vec::new(),
            }],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            mcp_server_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    )
    .unwrap();
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();

    for run in ["first", "second"] {
        driver
            .agent_end(
                &RunId::new(run),
                std::path::Path::new("/workspace"),
                &signal,
                pi_plugin::AgentEndEvent {
                    messages: Vec::new(),
                },
            )
            .await;
    }

    assert_eq!(
        *dispatcher.invocations.lock().unwrap(),
        ["a", "b", "b", "c"]
    );
}

#[test]
fn duplicate_logical_commands_register_by_unique_invocation_name() {
    let command = |plugin: &str, callback: &str, invocation: &str| JsAgentPluginManifest {
        id: plugin.to_string(),
        tools: Vec::new(),
        commands: vec![JsCommandManifest {
            callback_id: callback.to_string(),
            name: "shared".to_string(),
            invocation_name: invocation.to_string(),
            description: invocation.to_string(),
            argument_hint: None,
            source_info: None,
        }],
        hooks: Vec::new(),
    };
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "duplicate-commands".to_string(),
            dynamic_hooks: false,
            agent_plugins: vec![
                command("first", "first-command", "shared:1"),
                command("second", "second-command", "shared:2"),
            ],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            mcp_server_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        Arc::new(HookBatchDispatcher::default()),
    )
    .unwrap();

    let (_, registries) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap();
    assert!(registries.command("shared").is_none());
    assert!(registries.command("shared:1").is_some());
    assert!(registries.command("shared:2").is_some());
}

#[tokio::test]
async fn dynamic_provider_and_session_hooks_need_no_frozen_callback_manifest() {
    let owner = "second";
    let dispatcher = Arc::new(DynamicCrossLifecycleDispatcher {
        snapshots: Mutex::new(vec![
            (
                "before_provider_request",
                vec![JsHookSnapshot {
                    source_path: None,
                    plugin_id: owner.to_string(),
                    callback_id: "dynamic-provider".to_string(),
                }],
            ),
            (
                "session_before_switch",
                vec![
                    JsHookSnapshot {
                        source_path: None,
                        plugin_id: "first".to_string(),
                        callback_id: "failing-session".to_string(),
                    },
                    JsHookSnapshot {
                        source_path: None,
                        plugin_id: owner.to_string(),
                        callback_id: "dynamic-session".to_string(),
                    },
                ],
            ),
        ]),
        invocations: Mutex::new(Vec::new()),
    });
    let empty_agent = |id: &str| JsAgentPluginManifest {
        id: id.to_string(),
        tools: Vec::new(),
        commands: Vec::new(),
        hooks: Vec::new(),
    };
    let empty_provider = |id: &str| JsProviderPluginManifest {
        id: id.to_string(),
        hooks: Vec::new(),
    };
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "dynamic-cross-lifecycle".to_string(),
            dynamic_hooks: true,
            agent_plugins: vec![empty_agent("first"), empty_agent(owner)],
            provider_plugins: vec![empty_provider("first"), empty_provider(owner)],
            provider_registrations: Vec::new(),
            mcp_server_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    )
    .unwrap();
    assert_eq!(generation.provider_plugins().len(), 1);
    assert!(
        generation.plugins()[0]
            .session_hook_interests()
            .contains(pi_plugin::SessionHook::BeforeSwitch)
    );
    let (_, signal) = AbortHandle::new();

    let provider_result = generation.provider_plugins()[0]
        .before_provider_request(
            ProviderPluginContext::unavailable_for_testing(
                PluginId::new("first"),
                1,
                ProviderId::new("provider"),
                ModelId::new("model"),
                "/workspace".into(),
                signal,
            ),
            BeforeProviderRequestEvent {
                payload: json!({"original": true}),
            },
        )
        .await
        .unwrap();
    assert_eq!(provider_result, Some(json!({"dynamic": true})));

    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap();
    let session_result = driver
        .session_before_switch(
            &SessionDispatchContext {
                generation: 1,
                identity: SessionIdentity {
                    id: "session".to_string(),
                    path: "/sessions/session.jsonl".into(),
                    cwd: "/workspace".into(),
                    parent_session_id: None,
                },
            },
            &SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::Resume,
                target_session_file: None,
            },
        )
        .await
        .unwrap();
    assert!(session_result.cancel);
    let diagnostics = driver.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].plugin_id.as_str(), "first");
    assert_eq!(diagnostics[0].hook.as_str(), "session_before_switch");
    assert_eq!(diagnostics[0].generation, Some(1));
    assert!(
        diagnostics[0]
            .message
            .contains("first session handler failed")
    );

    let invocations = dispatcher.invocations.lock().unwrap();
    assert_eq!(invocations[0].payload["context"]["pluginId"], owner);
    assert_eq!(invocations[1].payload["context"]["pluginId"], "first");
    assert_eq!(invocations[2].payload["context"]["pluginId"], owner);
}

fn assistant(text: &str) -> AssistantMessage {
    AssistantMessage {
        duration_ms: None,
        thinking_level: None,
        content: vec![ContentBlock::Text(TextContent::new(text))],
        api: "test".to_string(),
        provider: ProviderId::new("provider"),
        model: ModelId::new("model"),
        provider_thinking_level: None,
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
            generation.plugins(),
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
        .register_plugins(generation.plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();

    let patch = driver
        .before_agent_start(
            &RunId::new("run"),
            std::path::Path::new("/workspace"),
            &signal,
            BeforeAgentStartEvent {
                prompt_state: None,
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
        diagnostic.hook.as_str() == "before_agent_start"
            && diagnostic.message.contains("intentional callback failure")
    }));
}

#[tokio::test]
async fn one_javascript_source_shares_one_plugin_across_agent_and_session_hooks() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    assert_eq!(generation.plugins().len(), 1);
    let (driver, registries) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
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

    generation.plugins()[0]
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

    let switch_result = generation.plugins()[0]
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
        diagnostic.hook.as_str() == "input"
            && diagnostic.message.contains("intentional callback failure")
    }));
}

#[tokio::test]
async fn turn_metadata_and_message_end_replacement_match_pi() {
    let dispatcher = Arc::new(LifecycleDispatcher::default());
    let generation = generation(Arc::clone(&dispatcher));
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
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
                boundary: None,
                message_entry_id: None,
                tool_result_entry_ids: Vec::new(),
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
        .register_plugins(generation.plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();

    driver
        .agent_settled(
            &RunId::new("settled-run"),
            std::path::Path::new("/workspace"),
            &signal,
            AgentSettledEvent { aborted: true },
        )
        .await;

    let invocations = dispatcher.invocations.lock().unwrap();
    let settled = invocations
        .iter()
        .find(|invocation| invocation.callback_id == "agent-settled")
        .expect("later agent_settled callback should still run");
    assert_eq!(
        settled.payload["event"],
        json!({"type": "agent_settled", "aborted": true})
    );
    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook.as_str() == "agent_settled"
            && diagnostic.message.contains("intentional callback failure")
    }));
}

#[tokio::test]
async fn observer_hooks_share_one_generation_batch_and_one_compact_stream_encoding() {
    let dispatcher = Arc::new(HookBatchDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-observer-batch".to_string(),
            dynamic_hooks: false,
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
            mcp_server_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    )
    .unwrap();
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
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
            && diagnostic.hook.as_str() == "message_update"
            && diagnostic.message.contains("intentional observer failure")
    }));
}

struct BoundaryDispatcher;
#[async_trait]
impl JsCallbackDispatcher for BoundaryDispatcher {
    async fn snapshot_hooks(
        &self,
        _generation_id: &str,
        hook: &str,
    ) -> Result<Option<Vec<JsHookSnapshot>>, JsCallbackError> {
        assert_eq!(hook, "agent_before_settle");
        Ok(Some(vec![
            JsHookSnapshot {
                source_path: None,
                plugin_id: "one".into(),
                callback_id: "boundary:first".into(),
            },
            JsHookSnapshot {
                source_path: None,
                plugin_id: "one".into(),
                callback_id: "boundary:repair".into(),
            },
            JsHookSnapshot {
                source_path: None,
                plugin_id: "two".into(),
                callback_id: "boundary:last".into(),
            },
        ]))
    }

    async fn invoke(
        &self,
        invocation: JsInvocation,
        _: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        let event = &invocation.payload["event"];
        match invocation.callback_id.as_str() {
            "boundary:first" => Ok(
                json!({"entries":[{"type":"context_edit","targetId":"missing","replacement":null}],"continue":true}),
            ),
            "boundary:repair" => {
                assert_eq!(event["entries"][0]["targetId"], "missing");
                assert_eq!(event["continue"], true);
                Ok(
                    json!({"entries":[{"type":"custom_message","customType":"repair","content":"fixed","display":false}]}),
                )
            }
            "boundary:last" => {
                assert_eq!(event["context"]["canContinue"], json!(true));
                assert_eq!(event["entries"][0]["content"], "fixed");
                Ok(json!({"continue":false}))
            }
            _ => panic!("unexpected boundary handler"),
        }
    }
}

#[tokio::test]
async fn boundary_callbacks_rebuild_previews_between_handlers_and_sources() {
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "boundaries".into(),
            dynamic_hooks: true,
            agent_plugins: vec![
                JsAgentPluginManifest {
                    id: "one".into(),
                    tools: vec![],
                    commands: vec![],
                    hooks: vec![
                        JsHookManifest {
                            name: "agent_before_settle".into(),
                            callback_id: "boundary:first".into(),
                        },
                        JsHookManifest {
                            name: "agent_before_settle".into(),
                            callback_id: "boundary:repair".into(),
                        },
                    ],
                },
                JsAgentPluginManifest {
                    id: "two".into(),
                    tools: vec![],
                    commands: vec![],
                    hooks: vec![JsHookManifest {
                        name: "agent_before_settle".into(),
                        callback_id: "boundary:last".into(),
                    }],
                },
            ],
            provider_plugins: vec![],
            provider_registrations: vec![],
            mcp_server_registrations: vec![],
            session_plugins: vec![],
            diagnostics: vec![],
        },
        Arc::new(BoundaryDispatcher),
    )
    .unwrap();
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap();
    let (_, signal) = AbortHandle::new();
    let result = driver
        .dispatch_boundary(
            &RunId::new("run"),
            std::path::Path::new("."),
            &signal,
            None,
            pi_plugin::AgentActivityOutcome::Completed,
            |entries| {
                if entries
                    .iter()
                    .any(|entry| matches!(entry, pi_plugin::SessionBoundaryDraft::ContextEdit(_)))
                {
                    return Err("unknown target".into());
                }
                Ok(pi_plugin::BoundaryContextPreview {
                    can_continue: !entries.is_empty(),
                    ..Default::default()
                })
            },
        )
        .await;
    assert_eq!(result.continue_run, Some(false));
    assert_eq!(result.entries.unwrap().len(), 1);
    assert!(
        driver
            .diagnostics()
            .iter()
            .any(|error| error.message.contains("unknown target"))
    );
}
