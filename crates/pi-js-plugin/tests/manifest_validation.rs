use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::AgentHook;
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsHookManifest, JsInvocation, JsPluginGeneration, JsProviderPluginManifest,
    JsProviderRegistration,
};
use serde_json::Value;

#[derive(Default)]
struct RetiringDispatcher {
    retired: Mutex<Vec<String>>,
}

#[test]
fn configured_provider_registrations_are_validated_and_retained() {
    let dispatcher = Arc::new(RetiringDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-provider-registration".to_string(),
            agent_plugins: Vec::new(),
            provider_plugins: Vec::new(),
            provider_registrations: vec![JsProviderRegistration {
                plugin_id: "js:0:provider.ts".to_string(),
                path: "/provider.ts".to_string(),
                name: "proxy".to_string(),
                config: serde_json::json!({"baseUrl": "https://proxy.example/v1"}),
            }],
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap();

    assert_eq!(generation.provider_registrations()[0].name, "proxy");
}

#[test]
fn malformed_provider_registration_retires_the_candidate_generation() {
    let dispatcher = Arc::new(RetiringDispatcher::default());
    let result = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-invalid-provider-registration".to_string(),
            agent_plugins: Vec::new(),
            provider_plugins: Vec::new(),
            provider_registrations: vec![JsProviderRegistration {
                plugin_id: "js:0:provider.ts".to_string(),
                path: "/provider.ts".to_string(),
                name: "proxy".to_string(),
                config: serde_json::json!("not-an-object"),
            }],
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    );

    let error = match result {
        Ok(_) => panic!("malformed provider registration was accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("config must be an object"));
    assert_eq!(
        *dispatcher.retired.lock().unwrap(),
        ["js-invalid-provider-registration"]
    );
}

#[async_trait]
impl JsCallbackDispatcher for RetiringDispatcher {
    async fn invoke(
        &self,
        _invocation: JsInvocation,
        _context: pi_js_plugin::PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        unreachable!("manifest validation does not invoke callbacks")
    }

    fn retire_generation(&self, generation_id: &str) {
        self.retired.lock().unwrap().push(generation_id.to_string());
    }
}

fn empty_agent(hooks: Vec<JsHookManifest>) -> JsAgentPluginManifest {
    JsAgentPluginManifest {
        id: "extension".to_string(),
        tools: Vec::new(),
        commands: Vec::new(),
        hooks,
    }
}

#[test]
fn agent_hook_interests_are_derived_from_the_validated_manifest() {
    let dispatcher = Arc::new(RetiringDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-interests".to_string(),
            agent_plugins: vec![empty_agent(vec![JsHookManifest {
                name: "input".to_string(),
                callback_id: "input-callback".to_string(),
            }])],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap();

    let plugin = generation.agent_plugins().remove(0);
    let interests = plugin.hook_interests();
    assert!(interests.contains(AgentHook::Input));
    assert!(!interests.contains(AgentHook::Context));
}

#[test]
fn observer_batching_keeps_mutating_hooks_on_their_owning_plugin() {
    let dispatcher = Arc::new(RetiringDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-observer-routes".to_string(),
            agent_plugins: vec![
                JsAgentPluginManifest {
                    id: "first".to_string(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: Vec::new(),
                },
                JsAgentPluginManifest {
                    id: "second".to_string(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: vec![
                        JsHookManifest {
                            name: "message_update".to_string(),
                            callback_id: "message-update".to_string(),
                        },
                        JsHookManifest {
                            name: "tool_call".to_string(),
                            callback_id: "tool-call".to_string(),
                        },
                    ],
                },
            ],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap();

    let plugins = generation.agent_plugins();
    assert_eq!(plugins[0].id().as_str(), "first");
    assert!(
        plugins[0]
            .hook_interests()
            .contains(AgentHook::MessageUpdate)
    );
    assert!(!plugins[0].hook_interests().contains(AgentHook::ToolCall));
    assert_eq!(plugins[1].id().as_str(), "second");
    assert!(plugins[1].hook_interests().contains(AgentHook::ToolCall));
    assert!(
        !plugins[1]
            .hook_interests()
            .contains(AgentHook::MessageUpdate)
    );
}

#[test]
fn unsupported_hooks_fail_the_candidate_and_retire_its_host_generation() {
    let dispatcher = Arc::new(RetiringDispatcher::default());
    let result = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-invalid".to_string(),
            agent_plugins: vec![empty_agent(vec![JsHookManifest {
                name: "model_select".to_string(),
                callback_id: "model-select".to_string(),
            }])],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    );

    let error = match result {
        Ok(_) => panic!("unsupported hook was accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unsupported agent hook"));
    assert_eq!(*dispatcher.retired.lock().unwrap(), ["js-invalid"]);
}

#[test]
fn callback_ids_are_unique_across_the_three_lifecycle_manifests() {
    let dispatcher = Arc::new(RetiringDispatcher::default());
    let result = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-duplicate".to_string(),
            agent_plugins: vec![empty_agent(vec![JsHookManifest {
                name: "input".to_string(),
                callback_id: "same-callback".to_string(),
            }])],
            provider_plugins: vec![JsProviderPluginManifest {
                id: "extension".to_string(),
                hooks: vec![JsHookManifest {
                    name: "before_provider_request".to_string(),
                    callback_id: "same-callback".to_string(),
                }],
            }],
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    );

    let error = match result {
        Ok(_) => panic!("duplicate callback was accepted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("duplicate callback id: same-callback")
    );
}
