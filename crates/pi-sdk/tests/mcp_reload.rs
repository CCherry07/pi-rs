use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::response::IntoResponse;
use pi_core::{
    AbortHandle, ContentBlock, CustomMessage, CustomMessageContent, TextContent, ToolContext,
    ToolUpdateSink,
};
use pi_sdk::{Pi, ProductConfig};
use pi_session::PiSession;
use serde_json::{Value, json};

struct McpFixture {
    directory: tempfile::TempDir,
    pi: Pi,
    session: PiSession,
    url: String,
    tool_name: Arc<RwLock<String>>,
    server_task: tokio::task::JoinHandle<()>,
}

impl McpFixture {
    async fn new(enabled: bool) -> Self {
        let tool_name = Arc::new(RwLock::new("echo".to_string()));
        let router = axum::Router::new().route(
            "/mcp",
            axum::routing::post({
                let tool_name = Arc::clone(&tool_name);
                move |axum::Json(request): axum::Json<Value>| {
                    let tool_name = Arc::clone(&tool_name);
                    async move {
                        let id = &request["id"];
                        if id.is_null() {
                            return axum::http::StatusCode::ACCEPTED.into_response();
                        }
                        let result = match request["method"].as_str().unwrap() {
                            "server/discover" => {
                                return axum::Json(json!({
                                    "jsonrpc": "2.0", "id": id,
                                    "error": {"code": -32601, "message": "Use initialize"}
                                }))
                                .into_response();
                            }
                            "initialize" => json!({
                                "protocolVersion": "2025-03-26",
                                "capabilities": {"tools": {}},
                                "serverInfo": {"name": "reload-fixture", "version": "1"}
                            }),
                            "tools/list" => json!({"tools": [{
                                "name": tool_name.read().unwrap().clone(),
                                "inputSchema": {"type": "object"}
                            }]}),
                            "tools/call" => json!({"content": [{
                                "type": "text", "text": request["params"]["name"]
                            }]}),
                            method => panic!("unexpected MCP method {method}"),
                        };
                        axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                            .into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let project = directory.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            agent.join("memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        Self::write_servers(&agent.join("mcp.json"), &url, enabled);
        let mut config = ProductConfig::new(project.clone(), agent.clone());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        let pi = Pi::builder(config).build().unwrap();
        let session = pi
            .sessions()
            .create_session(&project, agent.join("session.jsonl"))
            .await
            .unwrap();
        Self {
            directory,
            pi,
            session,
            url,
            tool_name,
            server_task,
        }
    }

    fn config_path(&self) -> PathBuf {
        self.directory.path().join("agent/mcp.json")
    }

    fn set_enabled(&self, enabled: bool) {
        Self::write_servers(&self.config_path(), &self.url, enabled);
    }

    fn write_servers(path: &std::path::Path, url: &str, enabled: bool) {
        fs::write(
            path,
            json!({"version": 1, "mcpServers": {"fixture": {
                "type": "http", "url": url, "enabled": enabled
            }}})
            .to_string(),
        )
        .unwrap();
    }

    fn assert_tool(&self, name: &str, expected: bool) {
        let session = self.session.current();
        assert_eq!(
            session
                .runtime()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == name),
            expected,
            "registered tool {name}"
        );
        assert_eq!(
            session
                .runtime()
                .active_tools()
                .iter()
                .any(|tool| tool == name),
            expected,
            "active tool {name}"
        );
    }

    async fn invoke(&self, name: &str, expected: &str) {
        let session = self.session.current();
        let tool = session
            .runtime()
            .agent()
            .runtime()
            .registries()
            .tool(name)
            .unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = tool
            .execute(
                ToolContext::standalone(self.directory.path().to_path_buf(), signal),
                "call".into(),
                json!({}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(
            result.content,
            vec![ContentBlock::Text(TextContent::new(expected))]
        );
    }

    async fn shutdown(&self) {
        self.pi.sessions().shutdown().await.unwrap();
    }
}

impl Drop for McpFixture {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

#[tokio::test]
async fn mcp_reload_activates_a_newly_enabled_server() {
    let fixture = McpFixture::new(false).await;
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.set_enabled(true);
    fixture
        .session
        .current()
        .submit("/mcp reload")
        .await
        .unwrap();
    fixture.assert_tool("mcp__fixture__echo", true);
    fixture
        .session
        .current()
        .append_custom_message(CustomMessage {
            custom_type: "reload-fixture".into(),
            content: CustomMessageContent::Text("Refresh session context".into()),
            display: false,
            details: None,
            timestamp_ms: 1,
        })
        .unwrap();
    fixture.assert_tool("mcp__fixture__echo", true);
    fixture.invoke("mcp__fixture__echo", "echo").await;
    assert!(
        !fixture
            .directory
            .path()
            .join("agent/session.jsonl")
            .exists()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_reload_removes_disabled_or_deleted_servers() {
    for delete in [false, true] {
        let fixture = McpFixture::new(true).await;
        fixture.assert_tool("mcp__fixture__echo", true);
        if delete {
            fs::remove_file(fixture.config_path()).unwrap();
        } else {
            fixture.set_enabled(false);
        }
        fixture.session.reload().await.unwrap();
        fixture.assert_tool("mcp__fixture__echo", false);
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn mcp_reload_reconciles_a_changed_remote_tool_catalog() {
    let fixture = McpFixture::new(true).await;
    fixture.assert_tool("mcp__fixture__echo", true);
    *fixture.tool_name.write().unwrap() = "renamed".into();
    fixture.session.reload().await.unwrap();
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.assert_tool("mcp__fixture__renamed", true);
    fixture.invoke("mcp__fixture__renamed", "renamed").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_reload_failure_keeps_the_previous_connection_usable() {
    let fixture = McpFixture::new(true).await;
    let before = fixture.session.current();
    fs::write(fixture.config_path(), "invalid").unwrap();
    assert!(fixture.session.reload().await.is_err());
    assert!(Arc::ptr_eq(&before, &fixture.session.current()));
    fixture.assert_tool("mcp__fixture__echo", true);
    fixture.invoke("mcp__fixture__echo", "echo").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_reload_connection_failure_keeps_the_previous_connection_usable() {
    let fixture = McpFixture::new(true).await;
    let before = fixture.session.current();
    fs::write(
        fixture.config_path(),
        json!({"version": 1, "mcpServers": {
            "fixture": {"type": "http", "url": fixture.url},
            "unavailable": {"type": "stdio", "command": fixture.directory.path().join("missing-server")}
        }})
        .to_string(),
    )
    .unwrap();
    assert!(fixture.session.reload().await.is_err());
    assert!(Arc::ptr_eq(&before, &fixture.session.current()));
    fixture.assert_tool("mcp__fixture__echo", true);
    fixture.invoke("mcp__fixture__echo", "echo").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_removal_allows_resuming_a_session_that_used_the_old_tool() {
    let mut fixture = McpFixture::new(true).await;
    fixture.assert_tool("mcp__fixture__echo", true);
    let path = fixture.session.current().log().path().to_path_buf();
    // Persist the existing configuration to exercise the same restore path as
    // a prior response without making any provider requests in this test.
    fixture.session.current().log().materialize().unwrap();
    fixture
        .pi
        .sessions()
        .close_session(&fixture.session)
        .await
        .unwrap();
    fixture.set_enabled(false);
    fixture.session = fixture.pi.sessions().open_session(path).await.unwrap();
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.shutdown().await;
}
