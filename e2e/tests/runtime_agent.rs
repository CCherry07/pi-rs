use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_core::{
    AgentEndEvent, AgentPlugin, AgentStartEvent, ContentBlock, ContextEvent, ContextPatch, Message,
    ModelId, PluginContext, PluginError, PluginId, ProviderId, ToolCall, ToolCallEvent,
    ToolCallPatch, ToolExecutionEndEvent, ToolResultEvent, ToolResultPatch, UserMessage,
};
use pi_plugin_read::ReadPlugin;
use pi_plugin_skills::{SkillLoaderOptions, SkillsPlugin};
use pi_plugin_write::WritePlugin;
use pi_resources::ResourceLoaderOptions;
use pi_runtime::{PiRuntime, SystemPrompt};
use pi_session::AgentSession;
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::json;

#[derive(Clone)]
struct AuditPlugin {
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for AuditPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("e2e-audit")
    }

    async fn agent_start(
        &self,
        _context: PluginContext,
        _event: AgentStartEvent,
    ) -> Result<(), PluginError> {
        self.events.lock().unwrap().push("agent_start");
        Ok(())
    }

    async fn agent_end(
        &self,
        _context: PluginContext,
        _event: AgentEndEvent,
    ) -> Result<(), PluginError> {
        self.events.lock().unwrap().push("agent_end");
        Ok(())
    }

    async fn context(
        &self,
        _context: PluginContext,
        mut event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        event
            .messages
            .insert(0, Message::User(UserMessage::text("ephemeral context", 1)));
        Ok(ContextPatch {
            messages: Some(event.messages),
        })
    }

    async fn tool_call(
        &self,
        _context: PluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        let arguments = (event.tool_call.name == "write").then(|| {
            let mut arguments = event.validated_args;
            arguments["content"] = json!("created by e2e");
            arguments
        });
        Ok(ToolCallPatch {
            arguments,
            block: None,
        })
    }

    async fn tool_result(
        &self,
        _context: PluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        if event.tool_call.name == "read" {
            Ok(ToolResultPatch {
                details: Some(json!({"audited":true})),
                ..ToolResultPatch::default()
            })
        } else {
            Ok(ToolResultPatch::default())
        }
    }

    async fn tool_execution_end(
        &self,
        _context: PluginContext,
        _event: ToolExecutionEndEvent,
    ) -> Result<(), PluginError> {
        self.events.lock().unwrap().push("tool_execution_end");
        Ok(())
    }
}

#[tokio::test]
async fn runtime_acceptance_covers_prompt_tools_plugins_resources_and_session() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("project");
    let agent_dir = directory.path().join("agent");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("projects/frontend-app");
    copy_project(&fixture, &cwd);
    std::fs::create_dir_all(&agent_dir).unwrap();

    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "write-1",
            "write",
            json!({"path":"result.txt","content":"model content"}),
        )]),
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "read-1",
            "read",
            json!({"path":"result.txt"}),
        )]),
        ScriptedTurn::Text("verified".to_string()),
    ]);
    let provider = scripted.provider();
    let events = Arc::new(Mutex::new(Vec::new()));
    let runtime = PiRuntime::builder()
        .agent_plugin(AuditPlugin {
            events: Arc::clone(&events),
        })
        .agent_plugin(ReadPlugin)
        .agent_plugin(WritePlugin)
        .agent_plugin_factory({
            let options = SkillLoaderOptions::new(&cwd, &agent_dir);
            move || SkillsPlugin::load(options.clone())
        })
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["read".to_string(), "write".to_string()],
            cwd: cwd.clone(),
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Pi(Box::default()))
        .resources({
            let mut resources = ResourceLoaderOptions::new(&cwd, &agent_dir);
            resources.project_trusted = true;
            resources
        })
        .build()
        .unwrap();
    let session_path = directory.path().join("session.jsonl");
    let session = AgentSession::create(runtime, &session_path).await.unwrap();

    let outcome = session
        .prompt("create and verify result.txt")
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(cwd.join("result.txt")).unwrap(),
        "created by e2e"
    );
    assert!(
        matches!(outcome.new_messages.last(), Some(Message::Assistant(message))
        if message.content.iter().any(|block| matches!(block, ContentBlock::Text(text) if text.text == "verified")))
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .system_prompt
            .contains("React 19 + TypeScript + Vite frontend fixture")
    );
    assert!(
        requests[0]
            .system_prompt
            .contains("<name>react-component</name>")
    );
    assert!(matches!(&requests[0].messages[0], Message::User(message)
        if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "ephemeral context")));
    assert!(!outcome.new_messages.iter().any(|message| matches!(message, Message::User(user)
        if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "ephemeral context"))));
    let read_result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "read" => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(read_result.details, Some(json!({"audited":true})));
    let events = events.lock().unwrap().clone();
    assert_eq!(events.first(), Some(&"agent_start"));
    assert_eq!(events.last(), Some(&"agent_end"));
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "tool_execution_end")
            .count(),
        2
    );
    assert!(!session.runtime().agent().state().is_running);

    let document = session.log().load().unwrap();
    assert!(document.latest_prompt_snapshot().is_some());
    assert_eq!(document.messages().len(), outcome.new_messages.len());
    let snapshot = document.latest_prompt_snapshot().unwrap();
    assert_eq!(snapshot.generation, 1);
    assert!(
        snapshot
            .base_system_prompt
            .contains("React 19 + TypeScript + Vite frontend fixture")
    );
}

fn copy_project(source: &std::path::Path, destination: &std::path::Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap().filter_map(Result::ok) {
        let name = entry.file_name();
        if name == "node_modules" || name == "dist" || name == ".agents" || name == "target" {
            continue;
        }
        let target = destination.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_project(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
