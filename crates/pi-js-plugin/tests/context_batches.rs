use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, Message, PluginId, RunId, UserMessage};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsContextHookBatchInvocation,
    JsContextHookBatchResult, JsContextReplacement, JsGenerationManifest, JsHookBatchError,
    JsHookManifest, JsInvocation, JsPluginGeneration, PluginContextHandle,
};
use pi_plugin::{
    AgentPluginContext, ContextEvent, ContextPatch, Plugin, PluginDriver, PluginError,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Dispatcher {
    batches: Mutex<Vec<JsContextHookBatchInvocation>>,
}

#[async_trait]
impl JsCallbackDispatcher for Dispatcher {
    async fn invoke(
        &self,
        _: JsInvocation,
        _: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        panic!("context hooks must use the batch transport")
    }

    async fn invoke_context_hook_batch(
        &self,
        invocation: JsContextHookBatchInvocation,
        _: PluginContextHandle,
    ) -> Result<JsContextHookBatchResult, JsCallbackError> {
        self.batches.lock().unwrap().push(invocation.clone());
        let mut result = JsContextHookBatchResult {
            processed: 0,
            replacement: None,
            errors: Vec::new(),
        };
        for callback in &invocation.callbacks {
            result.processed += 1;
            match callback.callback_id.as_str() {
                "throw" => result.errors.push(JsHookBatchError {
                    callback_id: callback.callback_id.clone(),
                    message: "hook failed".into(),
                }),
                "invalid" => {
                    result.replacement = Some(JsContextReplacement {
                        messages: Value::Null,
                    });
                    break;
                }
                "replace" => {
                    result.replacement = Some(JsContextReplacement {
                        messages: json!([{ "role": "user", "content": "replacement", "timestamp": 2 }]),
                    });
                    break;
                }
                _ => {}
            }
        }
        Ok(result)
    }
}

fn generation(dispatcher: Arc<Dispatcher>, plugins: &[(&str, &[&str])]) -> JsPluginGeneration {
    JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "context-generation".into(),
            agent_plugins: plugins
                .iter()
                .map(|(id, callbacks)| JsAgentPluginManifest {
                    id: (*id).into(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: callbacks
                        .iter()
                        .map(|callback| JsHookManifest {
                            name: "context".into(),
                            callback_id: (*callback).into(),
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
    .unwrap()
}

struct NativeContext;
#[pi_plugin::plugin]
impl Plugin for NativeContext {
    fn id(&self) -> PluginId {
        PluginId::new("native")
    }
    async fn context(
        &self,
        _: AgentPluginContext,
        event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        let mut messages = Arc::unwrap_or_clone(event.messages);
        messages.push(Message::User(UserMessage::text("native", 2)));
        Ok(ContextPatch {
            messages: Some(messages),
        })
    }
}

async fn dispatch(driver: &PluginDriver) -> Vec<Message> {
    let (_, signal) = AbortHandle::new();
    driver
        .context(
            &RunId::new("run"),
            std::path::Path::new("."),
            &signal,
            vec![Message::User(UserMessage::text("original", 1))],
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn contiguous_js_hooks_share_transport_and_native_hooks_split_batches() {
    for with_native in [false, true] {
        let dispatcher = Arc::new(Dispatcher::default());
        let generation = generation(
            dispatcher.clone(),
            &[("a", &["a"]), ("b", &["b"]), ("c", &["c"]), ("d", &["d"])],
        );
        let mut plugins = generation.plugins();
        if with_native {
            plugins.insert(2, Arc::new(NativeContext));
        }
        let driver = PluginDriver::new(plugins).unwrap();
        let output = dispatch(&driver).await;
        let batches = dispatcher.batches.lock().unwrap();
        assert_eq!(batches.len(), if with_native { 2 } else { 1 });
        let ids = batches
            .iter()
            .flat_map(|batch| {
                batch
                    .callbacks
                    .iter()
                    .map(|callback| callback.context["pluginId"].as_str().unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        assert_eq!(batches[0].messages.len(), 1);
        if with_native {
            assert_eq!(batches[0].callbacks.len(), 2);
            assert_eq!(batches[1].callbacks.len(), 2);
            assert_eq!(batches[1].messages.as_ref(), &output);
            assert_eq!(output.len(), 2);
        } else {
            assert_eq!(batches[0].callbacks.len(), 4);
            assert_eq!(output.len(), 1);
        }
        assert!(driver.diagnostics().is_empty());
    }
}

#[tokio::test]
async fn replacements_are_validated_before_later_callbacks_and_errors_keep_the_chain_running() {
    let dispatcher = Arc::new(Dispatcher::default());
    let generation = generation(
        dispatcher.clone(),
        &[
            (
                "first",
                &["noop", "throw", "invalid", "after-invalid", "replace"],
            ),
            ("last", &["after-replace"]),
        ],
    );
    let driver = PluginDriver::new(generation.plugins()).unwrap();
    let output = dispatch(&driver).await;
    let batches = dispatcher.batches.lock().unwrap();
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[1].callbacks[0].callback_id, "after-invalid");
    assert_eq!(batches[0].messages, batches[1].messages);
    assert_eq!(batches[2].callbacks[0].callback_id, "after-replace");
    assert_eq!(batches[2].messages.as_ref(), &output);
    assert_eq!(
        output,
        vec![Message::User(UserMessage::text("replacement", 2))]
    );
    let errors = driver.diagnostics();
    assert_eq!(errors.len(), 2);
    assert!(
        errors
            .iter()
            .all(|error| error.plugin_id.as_str() == "first")
    );
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("hook failed"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("invalid messages result"))
    );
}
