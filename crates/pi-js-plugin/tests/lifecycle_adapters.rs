use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, BeforeProviderRequestEvent, CommandContext, CommandOutcome, InputEvent,
    InputPatch, ModelId, PluginId, ProviderId, ProviderPluginContext, RegistriesBuilder,
};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsCommandManifest,
    JsGenerationManifest, JsHookManifest, JsInvocation, JsPluginGeneration,
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
}

#[async_trait]
impl JsCallbackDispatcher for LifecycleDispatcher {
    async fn invoke(&self, invocation: JsInvocation) -> Result<Value, JsCallbackError> {
        let response = match invocation.callback_id.as_str() {
            "command" => json!({ "action": "transform", "text": "from command" }),
            "input" => json!({ "action": "transform", "text": "from input" }),
            "provider" => json!({ "model": "rewritten" }),
            "session" => Value::Null,
            "session-before-switch" => json!({ "cancel": true }),
            callback => {
                return Err(JsCallbackError::new(format!(
                    "unexpected callback {callback}"
                )));
            }
        };
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
                hooks: vec![JsHookManifest {
                    name: "input".to_string(),
                    callback_id: "input".to_string(),
                }],
            }],
            provider_plugins: vec![JsProviderPluginManifest {
                id: "extension".to_string(),
                hooks: vec![JsHookManifest {
                    name: "before_provider_request".to_string(),
                    callback_id: "provider".to_string(),
                }],
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
        },
        dispatcher,
    )
    .unwrap()
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
                },
            )
            .await
            .unwrap(),
        InputPatch::Transform("from input".to_string())
    );

    let provider = &generation.provider_plugins()[0];
    assert_eq!(
        provider
            .before_provider_request(
                ProviderPluginContext {
                    plugin_id: PluginId::new("extension"),
                    generation: 3,
                    provider_id: ProviderId::new("provider"),
                    model_id: ModelId::new("model"),
                    cwd: "/workspace".into(),
                    abort_signal: signal,
                },
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
}
