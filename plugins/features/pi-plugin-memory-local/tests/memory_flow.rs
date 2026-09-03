use std::sync::Arc;

use pi_agent::AgentOptions;
use pi_core::{
    ContentBlock, Message, ModelId, PluginContext, PresentationMode, ProviderId, TextContent,
    ToolCall, ToolCallId, UserMessage,
};
use pi_memory_loader::{MemoryLoader, MemoryLoaderOptions};
use pi_plugin_memory_local::{LocalMemoryProviderFactory, MEMORY_EVENT_TYPE};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, PiPluginContext, PluginContextBinding, SessionEntry,
    SessionPlugins, SessionStartEvent, SessionStartReason,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::json;

#[tokio::test]
async fn memory_tool_journals_before_indexing_and_later_recall_is_transient() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("project");
    let agent_dir = directory.path().join("agent");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("memory.json"),
        r#"{"version": 1, "provider": "local"}"#,
    )
    .unwrap();
    let memory = MemoryLoader::new(MemoryLoaderOptions::new(&cwd, &agent_dir))
        .provider_factory(LocalMemoryProviderFactory)
        .load()
        .await
        .unwrap()
        .unwrap();
    let memory_agent_plugin = memory.agent_plugin();
    let memory_session_plugin = memory.session_plugin();
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            ToolCallId::new("remember-1"),
            "memory",
            json!({
                "action": "remember",
                "text": "The user prefers Rust examples.",
                "scope": "project",
                "kind": "preference",
                "evidence": "The user explicitly asked to remember this preference."
            }),
        )]),
        ScriptedTurn::Text("saved".to_string()),
        ScriptedTurn::Text("using the preference".to_string()),
    ]);
    let provider = scripted.provider();
    let binding = PluginContextBinding::new();
    let plugin_context = Arc::new(PiPluginContext::new(PresentationMode::Print, true, binding));
    let context_access: Arc<dyn PluginContext> = plugin_context.clone();
    let runtime = PiRuntime::builder()
        .plugin_context(context_access)
        .provider_plugin(scripted)
        .agent_plugin_arc(memory_agent_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["memory".to_string(), "session_search".to_string()],
            cwd: cwd.clone(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let prepared = AgentSession::prepare_create_with_options(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default()
            .plugins(SessionPlugins::new().plugin_arc(memory_session_plugin)),
    )
    .await
    .unwrap();
    plugin_context.bind_generation_session(prepared.session());
    let session = prepared
        .activate(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })
        .await;

    let first = session
        .prompt(vec![Message::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent::new(
                "Please remember that I prefer Rust examples",
            ))],
            timestamp_ms: 1,
        })])
        .await
        .unwrap();
    assert!(
        first
            .new_messages
            .iter()
            .any(|message| matches!(message, Message::ToolResult(result) if result.tool_name == "memory" && !result.is_error)),
        "memory tool did not succeed: {:?}",
        first.new_messages
    );
    session
        .prompt(vec![Message::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent::new(
                "Use Rust examples for this answer",
            ))],
            timestamp_ms: 2,
        })])
        .await
        .unwrap();

    let document = session.log().load().unwrap();
    assert!(document.entries.iter().any(|entry| {
        matches!(
            &entry.entry,
            SessionEntry::Custom(custom) if custom.custom_type == MEMORY_EVENT_TYPE
        )
    }));
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let recalled = requests[2].messages.iter().any(|message| match message {
        Message::User(message) => message.content.iter().any(|content| {
            matches!(content, ContentBlock::Text(text) if text.text.contains("<pi_memory>") && text.text.contains("prefers Rust"))
        }),
        _ => false,
    });
    assert!(recalled);
    assert!(!document.entries.iter().any(|entry| {
        matches!(
            &entry.entry,
            SessionEntry::CustomMessage(message) if message.custom_type == "pi.memory.recall.v1"
        )
    }));
}
