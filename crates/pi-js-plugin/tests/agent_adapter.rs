use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortHandle, ContentBlock, RegistriesBuilder, ToolCallId, ToolContext, ToolUpdateSink,
};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsInvocation, JsInvocationKind, JsPluginGeneration, JsToolExecutionMode, JsToolManifest,
};
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingDispatcher {
    invocations: Mutex<Vec<JsInvocation>>,
}

#[async_trait]
impl JsCallbackDispatcher for RecordingDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: pi_js_plugin::ExtensionContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.invocations.lock().unwrap().push(invocation);
        Ok(json!({
            "content": [{"type": "text", "text": "Hello from JavaScript"}],
            "details": {"runtime": "node"},
            "isError": false,
            "terminate": false
        }))
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
                    execution_mode: JsToolExecutionMode::Sequential,
                }],
                commands: Vec::new(),
                hooks: Vec::new(),
            }],
            provider_plugins: Vec::new(),
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
    let (updates, _) = ToolUpdateSink::channel();
    let result = tool
        .execute(
            ToolContext {
                cwd: "/workspace".into(),
                abort_signal,
            },
            ToolCallId::new("call-1"),
            json!({"name": "Cherry"}),
            updates,
        )
        .await
        .unwrap();

    assert_eq!(
        result.content,
        vec![ContentBlock::Text(pi_core::TextContent::new(
            "Hello from JavaScript"
        ))]
    );
    assert_eq!(result.details, Some(json!({"runtime": "node"})));

    let invocations = dispatcher.invocations.lock().unwrap();
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].generation_id, "js-7");
    assert_eq!(invocations[0].callback_id, "example:tool:greet");
    assert_eq!(invocations[0].kind, JsInvocationKind::Tool);
    assert_eq!(
        invocations[0].payload,
        json!({
            "context": {"cwd": "/workspace", "toolCallId": "call-1"},
            "input": {"name": "Cherry"}
        })
    );
}
