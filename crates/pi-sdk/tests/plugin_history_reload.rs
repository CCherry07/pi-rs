use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, CustomMessage,
    CustomMessageContent, Message, PluginError, PluginId, RegisterContext, Tool, ToolCallId,
    ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink, UserMessage,
};
use pi_sdk::{Pi, ProductConfig};
use pi_session::{AgentMessage, AgentSession, PiSession, SessionGenerationOverlay};
use serde_json::{Value, json};

#[derive(Clone)]
struct PluginVersion {
    tool_name: Option<&'static str>,
    version: u32,
}

struct HistoryPlugin {
    version: PluginVersion,
    executions: Arc<AtomicUsize>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for HistoryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("history-fixture")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        if let Some(name) = self.version.tool_name {
            context.register_tool(Arc::new(HistoryTool {
                name,
                version: self.version.version,
                executions: Arc::clone(&self.executions),
            }))?;
        }
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        _event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        if self.version.tool_name.is_none() {
            return Ok(BeforeAgentStartPatch::default());
        }
        Ok(BeforeAgentStartPatch {
            messages: vec![Message::custom(CustomMessage {
                custom_type: "history-fixture.context".to_string(),
                content: CustomMessageContent::Text("saved plugin context".to_string()),
                display: false,
                details: Some(json!({"version": self.version.version, "opaque": [1, "kept"]})),
                timestamp_ms: 1,
            })],
            ..BeforeAgentStartPatch::default()
        })
    }
}

struct HistoryTool {
    name: &'static str,
    version: u32,
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for HistoryTool {
    fn spec(&self) -> ToolSpec {
        let parameter = if self.version == 1 {
            "value"
        } else {
            "replacement"
        };
        ToolSpec {
            name: self.name.to_string(),
            label: self.name.to_string(),
            description: "Writes extension state and returns structured data".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {parameter: {"type": "string"}},
                "required": [parameter],
                "additionalProperties": false,
            }),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let details =
            json!({"version": self.version, "input": input, "opaque": {"cursor": [3, 5]}});
        context
            .session
            .append_entry("history-fixture.state", Some(details.clone()))
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        Ok(ToolResult {
            details: Some(details),
            ..ToolResult::text("persisted plugin result")
        })
    }
}

struct HistoryFixture {
    root: tempfile::TempDir,
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    executions: Arc<AtomicUsize>,
    version: Arc<Mutex<PluginVersion>>,
    server: tokio::task::JoinHandle<()>,
}

impl HistoryFixture {
    async fn new() -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post({
                let requests = Arc::clone(&requests);
                move |axum::Json(request): axum::Json<Value>| {
                    let requests = Arc::clone(&requests);
                    async move {
                        let first = {
                            let mut requests = requests.lock().unwrap();
                            let first = requests.is_empty();
                            requests.push(request);
                            first
                        };
                        let chunk = if first {
                            json!({"choices": [{"delta": {"tool_calls": [{
                                "index": 0, "id": "history-call", "type": "function",
                                "function": {"name": "history_tool", "arguments": "{\"value\":\"saved input\"}"}
                            }]}, "finish_reason": "tool_calls"}]})
                        } else {
                            json!({"choices": [{"delta": {"content": "continued"}, "finish_reason": "stop"}]})
                        };
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("agent")).unwrap();
        std::fs::write(
            root.path().join("agent/memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("agent/models.json"),
            json!({
                "providers": {"history-fixture": {
                    "baseUrl": base_url,
                    "api": "openai-completions",
                    "apiKey": "fixture-key",
                    "models": [{"id": "history-model"}]
                }}
            })
            .to_string(),
        )
        .unwrap();
        Self {
            root,
            base_url,
            requests,
            executions: Arc::new(AtomicUsize::new(0)),
            version: Arc::new(Mutex::new(PluginVersion {
                tool_name: Some("history_tool"),
                version: 1,
            })),
            server,
        }
    }

    fn host(&self) -> Pi {
        let mut config = ProductConfig::new(
            self.root.path().to_path_buf(),
            self.root.path().join("agent"),
        );
        config.base_url = self.base_url.clone();
        config.provider = "history-fixture".to_string();
        config.api_key = Some("fixture-key".to_string());
        config.model = Some("history-model".to_string());
        config.fallback_model = "history-model".to_string();
        config.trust_override = Some(true);
        config.discover_extensions = false;
        config.load_mcp_config = false;
        config.runtime_settings.retry.enabled = false;
        Pi::builder(config).build().unwrap()
    }

    fn overlay(&self) -> SessionGenerationOverlay {
        SessionGenerationOverlay::new().with_agent_plugin({
            let version = Arc::clone(&self.version);
            let executions = Arc::clone(&self.executions);
            move || {
                Arc::new(HistoryPlugin {
                    version: version.lock().unwrap().clone(),
                    executions: Arc::clone(&executions),
                })
            }
        })
    }

    fn assert_provider_history(&self) {
        let requests = self.requests.lock().unwrap();
        let request = requests.last().unwrap();
        let messages = request["messages"].as_array().unwrap();
        let calls = messages
            .iter()
            .filter_map(|message| message["tool_calls"].as_array())
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "history-call");
        assert_eq!(calls[0]["function"]["name"], "history_tool");
        assert_eq!(
            serde_json::from_str::<Value>(calls[0]["function"]["arguments"].as_str().unwrap())
                .unwrap(),
            json!({"value": "saved input"})
        );
        let results = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["tool_call_id"], "history-call");
        assert_eq!(results[0]["content"], "persisted plugin result");
        assert!(messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"]
                    .to_string()
                    .contains("saved plugin context")
        }));
        assert!(!request.to_string().contains("opaque-private-state"));
        assert_eq!(
            self.executions.load(Ordering::SeqCst),
            1,
            "history must never re-execute a plugin tool"
        );
    }
}

impl Drop for HistoryFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn stored_entries(session: &AgentSession) -> BTreeMap<String, Value> {
    session
        .log()
        .load()
        .unwrap()
        .branch()
        .unwrap()
        .into_iter()
        .map(|record| {
            (
                record.id.clone(),
                serde_json::to_value(&record.entry).unwrap(),
            )
        })
        .collect()
}

fn assert_stored_history(session: &PiSession, original: &BTreeMap<String, Value>) {
    let actual = stored_entries(&session.current());
    for (id, entry) in original {
        assert_eq!(actual.get(id), Some(entry), "history entry {id} changed");
    }
}

fn add_unknown_history(session: &PiSession) {
    let mut user =
        serde_json::to_value(Message::User(UserMessage::text("historical annotation", 2))).unwrap();
    user["extensionMetadata"] = json!({"nested": ["preserve", 7]});
    session
        .current()
        .log()
        .append_message(AgentMessage::custom(user).unwrap())
        .unwrap();
    session
        .current()
        .log()
        .append_message(
            AgentMessage::custom(json!({
                "role": "history-fixture.opaque", "payload": "opaque-private-state", "timestamp": 3
            }))
            .unwrap(),
        )
        .unwrap();
}

async fn check_history_after_plugin_change(version: PluginVersion, reload_first: bool) {
    let fixture = HistoryFixture::new().await;
    let host = fixture.host();
    let path = fixture.root.path().join("history.jsonl");
    let session = host
        .sessions()
        .create_session_with_overlay(fixture.root.path(), &path, fixture.overlay())
        .await
        .unwrap();
    session
        .current()
        .set_active_tools(["read", "history_tool"])
        .unwrap();
    session.current().prompt("use the plugin").await.unwrap();
    assert!(path.exists());
    fixture.assert_provider_history();
    add_unknown_history(&session);
    let original = stored_entries(&session.current());
    assert!(
        original
            .values()
            .any(|entry| entry["customType"] == "history-fixture.state")
    );
    assert!(
        original
            .values()
            .any(|entry| entry["type"] == "custom_message"
                && entry["customType"] == "history-fixture.context")
    );
    *fixture.version.lock().unwrap() = version.clone();

    if reload_first {
        session.reload().await.unwrap();
        assert_stored_history(&session, &original);
        session
            .current()
            .prompt("continue after reload")
            .await
            .unwrap();
        fixture.assert_provider_history();
    }
    host.sessions().shutdown().await.unwrap();
    drop(session);
    drop(host);

    let reopened_host = fixture.host();
    let reopened = if version.tool_name.is_none() {
        reopened_host.sessions().open_session(&path).await.unwrap()
    } else {
        reopened_host
            .sessions()
            .open_session_with_overlay(&path, fixture.overlay())
            .await
            .unwrap()
    };
    assert_stored_history(&reopened, &original);
    let tools = reopened.current().runtime().tool_specs();
    match version.tool_name {
        Some(name) => {
            assert!(tools.iter().any(|tool| tool.name == name
                && tool.parameters["required"]
                    == json!([if version.version == 1 {
                        "value"
                    } else {
                        "replacement"
                    }])));
            assert!(
                reopened
                    .current()
                    .runtime()
                    .active_tools()
                    .iter()
                    .any(|tool| tool == name)
            );
        }
        None => assert!(tools.iter().all(|tool| tool.name != "history_tool")),
    }
    reopened
        .current()
        .prompt("continue after process restart")
        .await
        .unwrap();
    fixture.assert_provider_history();
    assert_stored_history(&reopened, &original);
    reopened_host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn removed_plugin_history_survives_reload_and_process_restart() {
    check_history_after_plugin_change(
        PluginVersion {
            tool_name: None,
            version: 1,
        },
        true,
    )
    .await;
}

#[tokio::test]
async fn removed_plugin_history_survives_direct_process_restart() {
    check_history_after_plugin_change(
        PluginVersion {
            tool_name: None,
            version: 1,
        },
        false,
    )
    .await;
}

#[tokio::test]
async fn renamed_plugin_history_survives_reload_and_process_restart() {
    check_history_after_plugin_change(
        PluginVersion {
            tool_name: Some("renamed_history_tool"),
            version: 1,
        },
        true,
    )
    .await;
}

#[tokio::test]
async fn renamed_plugin_history_survives_direct_process_restart() {
    check_history_after_plugin_change(
        PluginVersion {
            tool_name: Some("renamed_history_tool"),
            version: 1,
        },
        false,
    )
    .await;
}

#[tokio::test]
async fn upgraded_plugin_history_survives_reload_and_process_restart() {
    check_history_after_plugin_change(
        PluginVersion {
            tool_name: Some("history_tool"),
            version: 2,
        },
        true,
    )
    .await;
}

#[tokio::test]
async fn upgraded_plugin_history_survives_direct_process_restart() {
    check_history_after_plugin_change(
        PluginVersion {
            tool_name: Some("history_tool"),
            version: 2,
        },
        false,
    )
    .await;
}
