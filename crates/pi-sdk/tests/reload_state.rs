use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::QueueMode;
use pi_core::{
    AgentPlugin, Message, ModelId, PluginId, ProviderId, RegisterContext, SessionExecutionOrigin,
    ThinkingLevel, Tool, ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult,
    ToolSpec, ToolUpdateSink, UserMessage,
};
use pi_sdk::{Pi, ProductConfig};
use pi_session::{QueueKind, SessionGenerationOverlay};

fn write_catalog(agent_dir: &Path, models: &[&str]) {
    std::fs::write(
        agent_dir.join("models.json"),
        serde_json::json!({
            "providers": {
                "reload-fixture": {
                    "baseUrl": "https://reload.invalid/v1",
                    "api": "openai-completions",
                    "apiKey": "fixture-key",
                    "models": models.iter().map(|id| serde_json::json!({
                        "id": id,
                        "reasoning": true,
                    })).collect::<Vec<_>>(),
                }
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn fixture(root: &Path) -> Pi {
    let agent_dir = root.join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("memory.json"),
        r#"{"version":1,"enabled":false}"#,
    )
    .unwrap();
    write_catalog(&agent_dir, &["alpha", "beta"]);
    let mut config = ProductConfig::new(root.to_path_buf(), agent_dir);
    config.provider = "reload-fixture".to_string();
    config.model = Some("alpha".to_string());
    config.thinking = Some(ThinkingLevel::Low);
    config.trust_override = Some(true);
    config.discover_extensions = false;
    config.load_mcp_config = false;
    Pi::builder(config).build().unwrap()
}

async fn assert_reload_keeps_current_state(materialized: bool) {
    let root = tempfile::tempdir().unwrap();
    let host = fixture(root.path());
    let path = root.path().join("session.jsonl");
    let session = host
        .sessions()
        .create_session(root.path(), &path)
        .await
        .unwrap();
    let original = session.current();
    original
        .set_model(ProviderId::new("reload-fixture"), ModelId::new("beta"))
        .unwrap();
    original.set_thinking_level(ThinkingLevel::High).unwrap();
    original
        .enqueue_message(
            Message::User(UserMessage::text("queued input", 1)),
            QueueKind::NextRun,
        )
        .unwrap();
    if materialized {
        original.log().materialize().unwrap();
    }
    std::fs::write(
        host.agent_dir().join("settings.json"),
        r#"{"steeringMode":"all","followUpMode":"all","defaultThinkingLevel":"minimal"}"#,
    )
    .unwrap();

    session.reload().await.unwrap();

    let reloaded = session.current();
    assert!(!Arc::ptr_eq(&original, &reloaded));
    assert!(original.is_closed());
    assert_eq!(reloaded.log().is_materialized(), materialized);
    assert_eq!(path.exists(), materialized);
    assert_eq!(reloaded.steering_mode(), QueueMode::All);
    assert_eq!(reloaded.follow_up_mode(), QueueMode::All);
    assert_eq!(reloaded.snapshot().queue.follow_up, ["queued input"]);
    assert_eq!(
        reloaded.runtime().agent().state().thinking_level,
        ThinkingLevel::High,
    );
    assert_eq!(reloaded.runtime().agent().state().model_id.as_str(), "beta");
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_keeps_current_model_thinking_and_queue_in_an_unsaved_session() {
    assert_reload_keeps_current_state(false).await;
}

#[tokio::test]
async fn reload_keeps_current_model_thinking_and_queue_in_a_saved_session() {
    assert_reload_keeps_current_state(true).await;
}

#[tokio::test]
async fn reload_accepts_removal_of_the_startup_model_after_switching_models() {
    let root = tempfile::tempdir().unwrap();
    let host = fixture(root.path());
    let session = host
        .sessions()
        .create_session(root.path(), root.path().join("session.jsonl"))
        .await
        .unwrap();
    session
        .current()
        .set_model(ProviderId::new("reload-fixture"), ModelId::new("beta"))
        .unwrap();
    write_catalog(host.agent_dir(), &["beta"]);

    session.reload().await.unwrap();

    assert_eq!(
        session
            .current()
            .runtime()
            .agent()
            .state()
            .model_id
            .as_str(),
        "beta",
    );
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_keeps_an_explicit_current_model_absent_from_the_catalog() {
    let root = tempfile::tempdir().unwrap();
    let host = fixture(root.path());
    let session = host
        .sessions()
        .create_session(root.path(), root.path().join("session.jsonl"))
        .await
        .unwrap();
    session
        .current()
        .set_model(
            ProviderId::new("reload-fixture"),
            ModelId::new("custom-unlisted"),
        )
        .unwrap();

    session.reload().await.unwrap();

    assert_eq!(
        session
            .current()
            .runtime()
            .agent()
            .state()
            .model_id
            .as_str(),
        "custom-unlisted",
    );
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_keeps_the_explicit_model_selected_when_resuming_a_session() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.jsonl");
    let original_host = fixture(root.path());
    let original = original_host
        .sessions()
        .create_session(root.path(), &path)
        .await
        .unwrap();
    original
        .current()
        .set_model(ProviderId::new("reload-fixture"), ModelId::new("beta"))
        .unwrap();
    original.current().log().materialize().unwrap();
    original_host.sessions().shutdown().await.unwrap();

    let resumed_host = fixture(root.path());
    let resumed = resumed_host.sessions().open_session(&path).await.unwrap();
    assert_eq!(
        resumed
            .current()
            .runtime()
            .agent()
            .state()
            .model_id
            .as_str(),
        "alpha",
    );

    resumed.reload().await.unwrap();

    assert_eq!(
        resumed
            .current()
            .runtime()
            .agent()
            .state()
            .model_id
            .as_str(),
        "alpha",
    );
    resumed_host.sessions().shutdown().await.unwrap();
}

struct ReloadTools(Vec<String>);

#[pi_core::agent_plugin]
impl AgentPlugin for ReloadTools {
    fn id(&self) -> PluginId {
        PluginId::new("reload-tools")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for name in &self.0 {
            context.register_tool(Arc::new(ReloadTool(name.clone())))?;
        }
        Ok(())
    }
}

struct ReloadTool(String);

#[async_trait]
impl Tool for ReloadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.clone(),
            label: self.0.clone(),
            description: "Reload fixture tool".to_string(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        _input: serde_json::Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(self.0.clone()))
    }
}

async fn assert_reload_reconciles_plugin_tools(origin: SessionExecutionOrigin) {
    for materialized in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let host = fixture(root.path());
        let tools = Arc::new(Mutex::new(vec!["keep".to_string(), "obsolete".to_string()]));
        let overlay = SessionGenerationOverlay::new()
            .with_execution_origin(origin)
            .with_agent_plugin({
                let tools = Arc::clone(&tools);
                move || Arc::new(ReloadTools(tools.lock().unwrap().clone()))
            });
        let path = root.path().join("session.jsonl");
        let session = host
            .sessions()
            .create_session_with_overlay(root.path(), &path, overlay)
            .await
            .unwrap();
        session
            .current()
            .set_active_tools(["read", "keep", "obsolete"])
            .unwrap();
        if materialized {
            session.current().log().materialize().unwrap();
        }
        *tools.lock().unwrap() = vec!["keep".to_string(), "added".to_string()];

        session.reload().await.unwrap();

        let reloaded = session.current();
        assert_eq!(reloaded.runtime().execution_origin(), origin);
        assert!(
            reloaded
                .runtime()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == "added")
        );
        assert!(
            reloaded
                .runtime()
                .tool_specs()
                .iter()
                .all(|tool| tool.name != "obsolete")
        );
        let expected = match origin {
            SessionExecutionOrigin::User => vec!["read", "keep", "added"],
            SessionExecutionOrigin::Subagent => vec!["read", "keep"],
        };
        assert_eq!(reloaded.runtime().active_tools(), expected);
        assert_eq!(
            reloaded
                .log()
                .load()
                .unwrap()
                .context()
                .unwrap()
                .active_tool_names,
            Some(expected.into_iter().map(str::to_string).collect()),
        );
        assert_eq!(path.exists(), materialized);
        host.sessions().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reload_reconciles_plugin_tools_and_preserves_disabled_builtin_tools() {
    assert_reload_reconciles_plugin_tools(SessionExecutionOrigin::User).await;
}

#[tokio::test]
async fn subagent_reload_removes_stale_tools_without_expanding_its_active_tools() {
    assert_reload_reconciles_plugin_tools(SessionExecutionOrigin::Subagent).await;
}
