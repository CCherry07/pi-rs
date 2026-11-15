use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, ImageContent};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsHookManifest, JsHookSnapshot, JsInvocation, JsPluginGeneration, PluginContextHandle,
};
use pi_plugin::{
    InputEvent, InputPatch, InputSource, InputStreamingBehavior, PluginDriver, RegistriesBuilder,
};
use serde_json::{Value, json};

// Adapted from legacy/pi/packages/coding-agent/test/extensions-input-event.test.ts.
// The Node host validates each JavaScript callback independently; this suite
// locks the Pi chaining semantics at the Rust adapter that owns handler order.

#[derive(Default)]
struct InputDispatcher {
    calls: Mutex<Vec<String>>,
    events: Mutex<Vec<(String, Value)>>,
    dynamic_snapshot: Mutex<Option<Vec<JsHookSnapshot>>>,
}

#[async_trait]
impl JsCallbackDispatcher for InputDispatcher {
    async fn snapshot_hooks(
        &self,
        _generation_id: &str,
        hook: &str,
    ) -> Result<Option<Vec<JsHookSnapshot>>, JsCallbackError> {
        assert_eq!(hook, "input");
        Ok(self.dynamic_snapshot.lock().unwrap().clone())
    }

    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        let callback = invocation.callback_id;
        let event = invocation.payload["event"].clone();
        self.calls.lock().unwrap().push(callback.clone());
        self.events
            .lock()
            .unwrap()
            .push((callback.clone(), event.clone()));

        match callback.as_str() {
            "continue-implicit" => Ok(Value::Null),
            "continue-explicit" => Ok(json!({ "action": "continue" })),
            "throw" => Err(JsCallbackError::new("boom")),
            "append-first" => Ok(json!({
                "action": "transform",
                "text": format!("{}[1]", event["text"].as_str().unwrap())
            })),
            "append-second" => Ok(json!({
                "action": "transform",
                "text": format!("{}[2]", event["text"].as_str().unwrap()),
                "images": [{
                    "type": "image",
                    "data": "bmV3",
                    "mimeType": "image/jpeg"
                }]
            })),
            "handled" => Ok(json!({ "action": "handled" })),
            "after-handled" | "other-extension" => Ok(json!({
                "action": "transform",
                "text": "must not run"
            })),
            other => Err(JsCallbackError::new(format!("unexpected callback {other}"))),
        }
    }
}

fn dynamic_driver(dispatcher: Arc<InputDispatcher>) -> PluginDriver {
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "dynamic-input-conformance".to_string(),
            dynamic_hooks: true,
            agent_plugins: vec![JsAgentPluginManifest {
                id: "coordinator".to_string(),
                tools: Vec::new(),
                commands: Vec::new(),
                hooks: Vec::new(),
            }],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap();
    RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap()
        .0
}

fn driver(dispatcher: Arc<InputDispatcher>, plugins: &[(&str, &[&str])]) -> PluginDriver {
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "legacy-input-conformance".to_string(),
            dynamic_hooks: false,
            agent_plugins: plugins
                .iter()
                .map(|(id, callbacks)| JsAgentPluginManifest {
                    id: (*id).to_string(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: callbacks
                        .iter()
                        .map(|callback| JsHookManifest {
                            name: "input".to_string(),
                            callback_id: (*callback).to_string(),
                        })
                        .collect(),
                })
                .collect(),
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap();
    RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap()
        .0
}

fn input(
    text: &str,
    images: Option<Vec<ImageContent>>,
    source: InputSource,
    streaming_behavior: Option<InputStreamingBehavior>,
) -> InputEvent {
    InputEvent {
        text: text.to_string(),
        images,
        source,
        streaming_behavior,
    }
}

#[tokio::test]
async fn undefined_and_explicit_continue_leave_input_unchanged() {
    let dispatcher = Arc::new(InputDispatcher::default());
    let driver = driver(
        Arc::clone(&dispatcher),
        &[("extension", &["continue-implicit", "continue-explicit"])],
    );
    let (_, signal) = AbortHandle::new();

    let patch = driver
        .input(
            std::path::Path::new("/workspace"),
            &signal,
            input(
                "original",
                None,
                InputSource::Interactive,
                Some(InputStreamingBehavior::Steer),
            ),
        )
        .await
        .unwrap();

    assert_eq!(patch, InputPatch::Continue);
    assert_eq!(
        *dispatcher.calls.lock().unwrap(),
        ["continue-implicit", "continue-explicit"]
    );
    assert!(dispatcher.events.lock().unwrap().iter().all(|(_, event)| {
        event["source"] == "interactive" && event["streamingBehavior"] == "steer"
    }));
}

#[tokio::test]
async fn transforms_chain_preserve_omitted_images_and_isolate_errors() {
    let dispatcher = Arc::new(InputDispatcher::default());
    let driver = driver(
        Arc::clone(&dispatcher),
        &[
            ("first-extension", &["throw", "append-first"]),
            ("second-extension", &["append-second"]),
        ],
    );
    let (_, signal) = AbortHandle::new();
    let original_images = vec![ImageContent {
        data: "b3JpZw==".to_string(),
        mime_type: "image/png".to_string(),
    }];

    let patch = driver
        .input(
            std::path::Path::new("/workspace"),
            &signal,
            input(
                "X",
                Some(original_images),
                InputSource::Rpc,
                Some(InputStreamingBehavior::FollowUp),
            ),
        )
        .await
        .unwrap();

    assert_eq!(
        patch,
        InputPatch::Transform {
            text: "X[1][2]".to_string(),
            images: Some(vec![ImageContent {
                data: "bmV3".to_string(),
                mime_type: "image/jpeg".to_string(),
            }]),
        }
    );
    assert_eq!(
        *dispatcher.calls.lock().unwrap(),
        ["throw", "append-first", "append-second"]
    );

    let events = dispatcher.events.lock().unwrap();
    let second = events
        .iter()
        .find(|(callback, _)| callback == "append-second")
        .unwrap();
    assert_eq!(second.1["text"], "X[1]");
    assert_eq!(second.1["source"], "rpc");
    assert_eq!(second.1["streamingBehavior"], "followUp");
    assert_eq!(
        second.1["images"],
        json!([{"type": "image", "data": "b3JpZw==", "mimeType": "image/png"}])
    );
    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook.as_str() == "input" && diagnostic.message.contains("boom")
    }));
}

#[tokio::test]
async fn dynamic_snapshot_handlers_still_chain_through_rust_validation() {
    let dispatcher = Arc::new(InputDispatcher {
        dynamic_snapshot: Mutex::new(Some(vec![
            JsHookSnapshot {
                plugin_id: "first-extension".to_string(),
                callback_id: "append-first".to_string(),
            },
            JsHookSnapshot {
                plugin_id: "second-extension".to_string(),
                callback_id: "append-second".to_string(),
            },
        ])),
        ..Default::default()
    });
    let driver = dynamic_driver(Arc::clone(&dispatcher));
    let (_, signal) = AbortHandle::new();

    let patch = driver
        .input(
            std::path::Path::new("/workspace"),
            &signal,
            input("X", None, InputSource::Interactive, None),
        )
        .await
        .unwrap();

    assert_eq!(
        patch,
        InputPatch::Transform {
            text: "X[1][2]".to_string(),
            images: Some(vec![ImageContent {
                data: "bmV3".to_string(),
                mime_type: "image/jpeg".to_string(),
            }]),
        }
    );
    let events = dispatcher.events.lock().unwrap();
    assert_eq!(events[1].1["text"], "X[1]");
    assert_eq!(events[0].1["source"], "interactive");
}

#[tokio::test]
async fn handled_short_circuits_later_handlers_and_extensions() {
    let dispatcher = Arc::new(InputDispatcher::default());
    let driver = driver(
        Arc::clone(&dispatcher),
        &[
            ("first-extension", &["handled", "after-handled"]),
            ("second-extension", &["other-extension"]),
        ],
    );
    let (_, signal) = AbortHandle::new();

    let patch = driver
        .input(
            std::path::Path::new("/workspace"),
            &signal,
            input("X", None, InputSource::Extension, None),
        )
        .await
        .unwrap();

    assert_eq!(patch, InputPatch::Handled);
    assert_eq!(*dispatcher.calls.lock().unwrap(), ["handled"]);
    let events = dispatcher.events.lock().unwrap();
    assert_eq!(events[0].1["source"], "extension");
    assert!(events[0].1.get("streamingBehavior").is_none());
}
