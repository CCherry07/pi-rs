use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, ContentBlock, ModelId, ProviderId, ProviderPluginDriver, RegistriesBuilder,
    ToolCallId, ToolContext, ToolExecutionMode, ToolUpdate, ToolUpdateSink,
};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsHookManifest, JsInvocation, JsInvocationKind, JsPluginGeneration, JsProviderPluginManifest,
    JsToolManifest,
};
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingDispatcher {
    invocations: Mutex<Vec<JsInvocation>>,
}

#[derive(Default)]
struct ProviderDispatcher {
    invocations: Mutex<Vec<JsInvocation>>,
}

#[async_trait]
impl JsCallbackDispatcher for ProviderDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: pi_js_plugin::PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.invocations.lock().unwrap().push(invocation.clone());
        match invocation.callback_id.as_str() {
            "headers:first" => {
                let mut headers = invocation.payload["event"]["headers"].clone();
                headers["X-First"] = json!("first");
                headers["X-Remove"] = Value::Null;
                Ok(headers)
            }
            "headers:failing" => Err(JsCallbackError::new("header callback failed")),
            "headers:second" => {
                assert!(invocation.payload["event"]["headers"]["X-Remove"].is_null());
                let mut headers = invocation.payload["event"]["headers"].clone();
                headers["X-Second"] = json!("second");
                Ok(headers)
            }
            "response:failing" => Err(JsCallbackError::new("response callback failed")),
            "response:second" => {
                assert_eq!(invocation.payload["event"]["status"], 429);
                assert_eq!(invocation.payload["event"]["headers"]["retry-after"], "2");
                Ok(Value::Null)
            }
            callback => panic!("unexpected callback {callback}"),
        }
    }
}

#[async_trait]
impl JsCallbackDispatcher for RecordingDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: pi_js_plugin::PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        let kind = invocation.kind;
        self.invocations.lock().unwrap().push(invocation);
        if kind == JsInvocationKind::ToolPrepareArguments {
            return Ok(json!({"name": "CHERRY"}));
        }
        Ok(json!({
            "content": [{"type": "text", "text": "Hello from JavaScript"}],
            "details": {"runtime": "node"},
            "isError": false,
            "terminate": false
        }))
    }

    async fn invoke_with_tool_updates(
        &self,
        invocation: JsInvocation,
        context: pi_js_plugin::PluginContextHandle,
        updates: ToolUpdateSink,
    ) -> Result<Value, JsCallbackError> {
        assert!(updates.send(ToolUpdate {
            content: vec![ContentBlock::Text(pi_core::TextContent::new("working"))],
            details: Some(json!({"phase": 1})),
        }));
        self.invoke(invocation, context).await
    }
}

#[tokio::test]
async fn manifest_tool_registers_and_dispatches_through_the_public_tool_interface() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-7".to_string(),
            agent_plugins: vec![JsAgentPluginManifest {
                id: "example".to_string(),
                tools: vec![JsToolManifest {
                    callback_id: "example:tool:greet".to_string(),
                    prepare_callback_id: Some("example:tool:greet:prepareArguments".to_string()),
                    name: "greet".to_string(),
                    label: "Greet".to_string(),
                    description: "Greet a person".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"name": {"type": "string"}},
                        "required": ["name"]
                    }),
                    prompt_snippet: Some("Greet a person".to_string()),
                    prompt_guidelines: vec!["Use the greet tool when asked.".to_string()],
                    execution_mode: ToolExecutionMode::Sequential,
                }],
                commands: Vec::new(),
                hooks: Vec::new(),
            }],
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    )
    .unwrap();

    let (_, registries) = RegistriesBuilder::new()
        .register_plugins(generation.agent_plugins())
        .unwrap();
    let tool = registries.tool("greet").expect("registered JS tool");
    let spec = tool.spec();
    assert_eq!(spec.execution_mode, pi_core::ToolExecutionMode::Sequential);
    assert_eq!(spec.prompt_snippet.as_deref(), Some("Greet a person"));
    let (_, abort_signal) = AbortHandle::new();
    let (updates, mut update_receiver) = ToolUpdateSink::channel();
    let tool_context = ToolContext::standalone("/workspace".into(), abort_signal);
    let prepared = tool
        .prepare_arguments(&tool_context, json!({"name": "Cherry"}))
        .await
        .unwrap();
    assert_eq!(prepared, json!({"name": "CHERRY"}));
    let result = tool
        .execute(tool_context, ToolCallId::new("call-1"), prepared, updates)
        .await
        .unwrap();

    assert_eq!(
        result.content,
        vec![ContentBlock::Text(pi_core::TextContent::new(
            "Hello from JavaScript"
        ))]
    );
    assert_eq!(result.details, Some(json!({"runtime": "node"})));
    assert_eq!(
        update_receiver.try_recv().unwrap(),
        ToolUpdate {
            content: vec![ContentBlock::Text(pi_core::TextContent::new("working"))],
            details: Some(json!({"phase": 1})),
        }
    );

    let invocations = dispatcher.invocations.lock().unwrap();
    assert_eq!(invocations.len(), 2);
    assert_eq!(invocations[0].kind, JsInvocationKind::ToolPrepareArguments);
    assert_eq!(invocations[0].payload, json!({"input": {"name": "Cherry"}}));
    assert_eq!(invocations[1].generation_id, "js-7");
    assert_eq!(invocations[1].callback_id, "example:tool:greet");
    assert_eq!(invocations[1].kind, JsInvocationKind::Tool);
    assert_eq!(
        invocations[1].payload,
        json!({
            "context": {"cwd": "/workspace", "toolCallId": "call-1"},
            "input": {"name": "CHERRY"}
        })
    );
}

#[tokio::test]
async fn provider_wire_hooks_chain_mutations_and_isolate_javascript_failures() {
    let dispatcher = Arc::new(ProviderDispatcher::default());
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "js-provider-hooks".to_string(),
            agent_plugins: Vec::new(),
            provider_plugins: vec![JsProviderPluginManifest {
                id: "wire-hooks".to_string(),
                hooks: vec![
                    JsHookManifest {
                        name: "before_provider_headers".to_string(),
                        callback_id: "headers:first".to_string(),
                    },
                    JsHookManifest {
                        name: "before_provider_headers".to_string(),
                        callback_id: "headers:failing".to_string(),
                    },
                    JsHookManifest {
                        name: "before_provider_headers".to_string(),
                        callback_id: "headers:second".to_string(),
                    },
                    JsHookManifest {
                        name: "after_provider_response".to_string(),
                        callback_id: "response:failing".to_string(),
                    },
                    JsHookManifest {
                        name: "after_provider_response".to_string(),
                        callback_id: "response:second".to_string(),
                    },
                ],
            }],
            provider_registrations: Vec::new(),
            session_plugins: Vec::new(),
            diagnostics: Vec::new(),
        },
        dispatcher.clone(),
    )
    .unwrap();
    let driver = ProviderPluginDriver::new(generation.provider_plugins()).unwrap();
    let (_, signal) = AbortHandle::new();
    let provider = ProviderId::new("provider");
    let model = ModelId::new("model");

    let headers = driver
        .before_provider_headers(
            3,
            &provider,
            &model,
            std::path::Path::new("/workspace"),
            &signal,
            BTreeMap::from([
                ("Existing".to_string(), "yes".to_string()),
                ("X-Remove".to_string(), "remove-me".to_string()),
            ]),
        )
        .await;
    assert_eq!(headers["Existing"], "yes");
    assert_eq!(headers["X-First"], "first");
    assert_eq!(headers["X-Second"], "second");
    assert!(!headers.contains_key("X-Remove"));

    driver
        .after_provider_response(
            3,
            &provider,
            &model,
            std::path::Path::new("/workspace"),
            &signal,
            429,
            BTreeMap::from([("retry-after".to_string(), "2".to_string())]),
        )
        .await;

    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook == "before_provider_headers"
            && diagnostic.message.contains("header callback failed")
    }));
    assert!(driver.diagnostics().iter().any(|diagnostic| {
        diagnostic.hook == "after_provider_response"
            && diagnostic.message.contains("response callback failed")
    }));
    assert_eq!(dispatcher.invocations.lock().unwrap().len(), 5);
}
