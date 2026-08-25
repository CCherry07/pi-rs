use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsHookManifest, JsInvocation, JsPluginGeneration, JsProviderPluginManifest,
};
use serde_json::Value;

#[derive(Default)]
struct RetiringDispatcher {
    retired: Mutex<Vec<String>>,
}

#[async_trait]
impl JsCallbackDispatcher for RetiringDispatcher {
    async fn invoke(
        &self,
        _invocation: JsInvocation,
        _context: pi_js_plugin::ExtensionContextHandle,
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
