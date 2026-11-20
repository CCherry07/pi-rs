//! Tool declarations are real transcript messages, not RPC-only projection fields.
use std::sync::{Arc, Mutex};

use pi_agent::{Agent, AgentConfigurationPatch, AgentOptions, AgentRuntime};
use pi_core::{AgentEvent, Message};
use pi_plugin::{Plugin, ProviderPlugin, RegistriesBuilder};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn, TestToolsPlugin};
use serde_json::json;

#[tokio::test]
async fn tool_loadout_changes_are_declared_once_in_transcript_and_native_events() {
    let provider = Arc::new(ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("first".into()),
        ScriptedTurn::Text("unchanged".into()),
        ScriptedTurn::Text("schema order restored".into()),
        ScriptedTurn::Text("removed".into()),
    ]));
    let requests = provider.provider();
    let plugins: Vec<Arc<dyn Plugin>> = vec![Arc::new(TestToolsPlugin::new())];
    let providers: Vec<Arc<dyn ProviderPlugin>> = vec![provider];
    let (plugins, providers, registries) = RegistriesBuilder::new()
        .register_plugin_sets(plugins, providers)
        .unwrap();
    let runtime = Arc::new(AgentRuntime::new(
        1,
        "",
        Arc::new(registries),
        Arc::new(plugins),
        Arc::new(providers),
    ));
    let agent = Agent::with_runtime(
        AgentOptions {
            active_tools: vec!["echo".into()],
            ..Default::default()
        },
        runtime,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    agent.subscribe(Arc::new({
        let events = events.clone();
        move |event, _| {
            let events = events.clone();
            async move {
                events.lock().unwrap().push(event);
                Ok(())
            }
        }
    }));
    let first = agent.prompt("first").await.unwrap();
    assert!(
        matches!(first.new_messages.first(), Some(Message::System(_))),
        "initial declarations must be in the actual transcript"
    );
    let value = serde_json::to_value(&first.new_messages[0]).unwrap();
    assert_eq!(value["content"], "");
    assert_eq!(value["toolsAdded"][0]["name"], "echo");
    assert_eq!(value["toolsAdded"].as_array().unwrap().len(), 1);
    assert!(value.get("toolsRemoved").is_none());
    assert!(value["toolsAdded"][0].get("label").is_none());
    assert!(value["toolsAdded"][0].get("executionMode").is_none());
    assert_eq!(requests.requests()[0].messages[0], first.new_messages[0]);
    assert!(matches!(&events.lock().unwrap()[..4], [
        AgentEvent::AgentStart, AgentEvent::TurnStart,
        AgentEvent::MessageStart { message: Message::System(start) },
        AgentEvent::MessageEnd { message: Message::System(end) },
    ] if start == end));
    let second = agent.prompt("same tool set").await.unwrap();
    assert!(
        !second
            .new_messages
            .iter()
            .any(|message| matches!(message, Message::System(_))),
        "unchanged tools must not add another checkpoint"
    );
    // Pi compares JSON declarations exactly, including insertion order within
    // schema objects; structural Value equality alone is not its contract.
    let mut replay = agent.state().messages.as_ref().clone();
    let Message::System(system) = &mut replay[0] else {
        panic!("system")
    };
    let parameters = &mut system.tools_added.as_mut().unwrap()[0].parameters;
    let original = parameters.clone();
    *parameters = serde_json::Value::Object(
        parameters
            .as_object()
            .unwrap()
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    assert_eq!(*parameters, original, "only property order changed");
    assert_ne!(
        serde_json::to_string(parameters).unwrap(),
        serde_json::to_string(&original).unwrap()
    );
    agent.replace_context_messages(replay).unwrap();
    let updated = agent.prompt("restore schema order").await.unwrap();
    let update = serde_json::to_value(&updated.new_messages[0]).unwrap();
    assert_eq!(update["toolsRemoved"], json!([{"name":"echo"}]));
    assert_eq!(update["toolsAdded"][0]["parameters"], original);
    agent
        .configure(AgentConfigurationPatch {
            active_tools: Some(Vec::new()),
            ..Default::default()
        })
        .unwrap();
    let third = agent.prompt("no tools").await.unwrap();
    let removed = serde_json::to_value(&third.new_messages[0]).unwrap();
    assert_eq!(removed["role"], "system");
    assert_eq!(removed["toolsRemoved"], json!([{"name":"echo"}]));
    assert!(removed.get("toolsAdded").is_none());
    assert!(requests.requests()[3].tools.is_empty());
}

struct HiddenTool(std::sync::atomic::AtomicBool);
#[async_trait::async_trait]
impl pi_plugin::Tool for HiddenTool {
    fn spec(&self) -> pi_core::ToolSpec {
        pi_core::ToolSpec {
            name: "hidden".into(),
            label: "Private label".into(),
            description: "original".into(),
            parameters: json!({"type":"object","properties":{"input":{"type":"string"}}}),
            output_schema: None,
            constrained_sampling: Some(pi_core::ConstrainedSampling::Disabled),
            execution_mode: Default::default(),
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }
    fn prepare_loadout(&self, _: &pi_plugin::ToolLoadout) -> pi_plugin::ToolLoadoutChanges {
        pi_plugin::ToolLoadoutChanges {
            descriptions: [("hidden".into(), "prepared description".into())].into(),
            hidden_declarations: if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                vec!["hidden".into()]
            } else {
                Vec::new()
            },
        }
    }
    async fn execute(
        &self,
        _: pi_plugin::ToolContext,
        _: pi_core::ToolCallId,
        _: serde_json::Value,
        _: pi_plugin::ToolUpdateSink,
    ) -> Result<pi_core::ToolResult, pi_plugin::ToolError> {
        unreachable!("this test only declares tools")
    }
}
struct ToolPlugin(Arc<HiddenTool>);
#[pi_plugin::plugin]
impl Plugin for ToolPlugin {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("tool-declarations")
    }
    fn register(&self, context: &mut pi_plugin::RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(self.0.clone())
    }
}

#[tokio::test]
async fn pending_intent_reconciles_replayed_definitions_and_hidden_requests_do_not_rewrite_history()
{
    let tool = Arc::new(HiddenTool(std::sync::atomic::AtomicBool::new(true)));
    let provider = Arc::new(ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("hidden answer".into()),
        ScriptedTurn::Text("visible answer".into()),
    ]));
    let requests = provider.provider();
    let (plugins, providers, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![Arc::new(ToolPlugin(tool.clone()))], vec![provider])
        .unwrap();
    let previous: Message = serde_json::from_value(json!({
        "role":"system", "content":"old instruction", "timestamp":1,
        "toolsAdded":[{"name":"hidden","description":"old definition","parameters":{},"constrainedSampling":false}]
    })).unwrap();
    let intent: Message = serde_json::from_value(json!({
        "role":"system", "content":"keep instruction", "timestamp":2,
        "sections":{"section":"keep section"},
        "toolsAdded":[{"name":"fiction","description":"discard intent","parameters":{}}],
        "toolsRemoved":[{"name":"unknown"}]
    }))
    .unwrap();
    let agent = Agent::with_runtime(
        AgentOptions {
            messages: vec![previous.clone()],
            active_tools: vec!["hidden".into()],
            ..Default::default()
        },
        Arc::new(AgentRuntime::new(
            1,
            "",
            Arc::new(registries),
            Arc::new(plugins),
            Arc::new(providers),
        )),
    );
    let outcome = agent
        .prompt(vec![
            intent,
            Message::User(pi_core::UserMessage::text("first", 3)),
        ])
        .await
        .unwrap();
    let Message::System(update) = &outcome.new_messages[0] else {
        panic!("missing declaration")
    };
    assert_eq!(update.timestamp_ms, 2);
    assert_eq!(update.instruction_text(), "keep instruction");
    assert_eq!(
        update.sections,
        [("section".into(), Some("keep section".into()))]
    );
    assert_eq!(
        update.tools_removed,
        Some(vec![pi_core::ToolReference {
            name: "hidden".into()
        }])
    );
    let definition = update.tools_added.as_ref().unwrap();
    assert_eq!(definition.len(), 1);
    assert_eq!(definition[0].name, "hidden");
    assert_eq!(definition[0].description, "prepared description");
    assert_eq!(
        serde_json::to_value(&definition[0]).unwrap()["constrainedSampling"],
        false
    );
    assert_eq!(
        pi_core::SystemMessage::current_tools(
            [&previous].into_iter().chain(outcome.new_messages.iter())
        ),
        *definition
    );
    assert!(requests.requests()[0].tools.is_empty());
    assert!(requests.requests()[0].messages.iter().all(|message| !matches!(message,
        Message::System(system) if system.tools_added.is_some() || system.tools_removed.is_some()
    )));
    tool.0.store(false, std::sync::atomic::Ordering::SeqCst);
    let next = agent.prompt("visible again").await.unwrap();
    assert!(
        !next
            .new_messages
            .iter()
            .any(|message| matches!(message, Message::System(_)))
    );
    assert_eq!(requests.requests()[1].tools.len(), 1);
    assert_eq!(
        pi_core::SystemMessage::current_tools(&requests.requests()[1].messages),
        *definition
    );
}
