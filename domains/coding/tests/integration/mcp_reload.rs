use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use axum::response::IntoResponse;
use pi_coding::{Config, Pi};
use pi_core::{AbortHandle, ContentBlock, CustomMessage, CustomMessageContent, TextContent};
use pi_plugin::{ContextParts, EphemeralSessionRequest, PluginContextScope};
use pi_session::PiSession;
use serde_json::{Value, json};

// A deterministic local provider drives the public Session tool-execution capability.
async fn fixture_provider_response(
    axum::extract::State(requests): axum::extract::State<Arc<RwLock<Vec<Value>>>>,
    axum::Json(request): axum::Json<Value>,
) -> impl IntoResponse {
    requests.write().unwrap().push(request.clone());
    let completed = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool");
    let (delta, reason) = if completed {
        (json!({"role": "assistant", "content": "done"}), "stop")
    } else {
        let name = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|tool| {
                let name = tool["function"]["name"].as_str()?;
                name.starts_with("mcp__fixture__").then_some(name)
            })
            .expect("fixture MCP tool in provider request");
        (
            json!({"role": "assistant", "tool_calls": [{
                "index": 0, "id": "fixture-call", "type": "function",
                "function": {"name": name, "arguments": "{}"}
            }]}),
            "tool_calls",
        )
    };
    let chunk = json!({
        "id": "fixture", "object": "chat.completion.chunk", "model": "test",
        "choices": [{"index": 0, "delta": delta, "finish_reason": null}]
    });
    let end = json!({
        "id": "fixture", "object": "chat.completion.chunk", "model": "test",
        "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]
    });
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
    )
}

#[derive(Default)]
struct DiscoveryCounts {
    initialize: AtomicUsize,
    tools_list: AtomicUsize,
}

struct McpFixture {
    directory: tempfile::TempDir,
    pi: Pi,
    session: PiSession,
    url: String,
    tool_name: Arc<RwLock<String>>,
    tool_calls: Arc<RwLock<Vec<String>>>,
    discoveries: Arc<DiscoveryCounts>,
    requests: Arc<RwLock<Vec<Value>>>,
    server_task: tokio::task::JoinHandle<()>,
}

impl McpFixture {
    async fn new(enabled: bool) -> Self {
        Self::with_policy(enabled, json!({})).await
    }

    async fn with_policy(enabled: bool, policy: Value) -> Self {
        let tool_name = Arc::new(RwLock::new("echo".to_string()));
        let tool_calls = Arc::new(RwLock::new(Vec::new()));
        let discoveries = Arc::new(DiscoveryCounts::default());
        let router = axum::Router::new().route(
            "/mcp",
            axum::routing::post({
                let tool_name = Arc::clone(&tool_name);
                let tool_calls = Arc::clone(&tool_calls);
                let discoveries = Arc::clone(&discoveries);
                move |axum::Json(request): axum::Json<Value>| {
                    let tool_name = Arc::clone(&tool_name);
                    let tool_calls = Arc::clone(&tool_calls);
                    let discoveries = Arc::clone(&discoveries);
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
                            "initialize" => {
                                discoveries.initialize.fetch_add(1, Ordering::AcqRel);
                                json!({
                                    "protocolVersion": "2025-03-26",
                                    "capabilities": {"tools": {}},
                                    "serverInfo": {"name": "reload-fixture", "version": "1"},
                                    "instructions": format!("Fixture guidance for {}.", tool_name.read().unwrap())
                                })
                            }
                            "tools/list" => {
                                discoveries.tools_list.fetch_add(1, Ordering::AcqRel);
                                json!({"tools": [{
                                    "name": tool_name.read().unwrap().clone(),
                                    "inputSchema": {"type": "object"}
                                }]})
                            }
                            "tools/call" => {
                                tool_calls.write().unwrap().push(
                                    request["params"]["name"].as_str().unwrap().to_string(),
                                );
                                json!({"content": [{
                                    "type": "text", "text": request["params"]["name"]
                                }]})
                            }
                            method => panic!("unexpected MCP method {method}"),
                        };
                        axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                            .into_response()
                    }
                }
            }),
        );
        let requests = Arc::new(RwLock::new(Vec::new()));
        let router = router
            .route(
                "/v1/chat/completions",
                axum::routing::post(fixture_provider_response),
            )
            .with_state(Arc::clone(&requests));
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
        Self::write_servers_with_policy(&agent.join("mcp.json"), &url, enabled, policy);
        fs::write(
            agent.join("models.json"),
            json!({"providers": {"fixture": {
                "baseUrl": url.replace("/mcp", "/v1"),
                "api": "openai-completions",
                "apiKey": "fixture",
                "models": [{"id": "test", "contextWindow": 100000, "maxTokens": 1024}]
            }}})
            .to_string(),
        )
        .unwrap();
        let mut config = Config::new(project.clone(), agent.clone());
        config.provider = "fixture".into();
        config.model = Some("test".into());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        let pi = Pi::builder(config).build().unwrap();
        let session = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent.join("session.jsonl"),
            ))
            .await
            .unwrap();
        Self {
            directory,
            pi,
            session,
            url,
            tool_name,
            tool_calls,
            discoveries,
            requests,
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
        Self::write_servers_with_policy(path, url, enabled, json!({}));
    }

    fn set_policy(&self, policy: Value) {
        Self::write_servers_with_policy(&self.config_path(), &self.url, true, policy);
    }

    fn write_servers_with_policy(path: &std::path::Path, url: &str, enabled: bool, policy: Value) {
        let mut server =
            json!({"type": "http", "url": url, "enabled": enabled, "exposure":"direct"});
        server
            .as_object_mut()
            .unwrap()
            .extend(policy.as_object().unwrap().clone());
        fs::write(
            path,
            json!({"version": 1, "mcpServers": {"fixture": server}}).to_string(),
        )
        .unwrap();
    }

    fn assert_discoveries(&self, expected: usize) {
        assert_eq!(
            self.discoveries.initialize.load(Ordering::Acquire),
            expected,
            "transport opens"
        );
        assert_eq!(
            self.discoveries.tools_list.load(Ordering::Acquire),
            expected,
            "inventory snapshots"
        );
    }

    fn assert_tool(&self, name: &str, expected: bool) {
        let session = self.session.current();
        assert_eq!(
            session
                .catalog()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == name),
            expected,
            "registered tool {name}"
        );
        assert_eq!(
            session
                .snapshot()
                .agent
                .active_tools
                .iter()
                .any(|tool| tool == name),
            expected,
            "active tool {name}"
        );
    }

    async fn invoke(&self, name: &str, expected: &str) {
        let session = self.session.current();
        let context = ContextParts::new(session.plugin_context(PluginContextScope::Base));
        let outcome = context
            .session
            .run_ephemeral(Self::invocation_request(name), AbortHandle::new().1)
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            pi_plugin::EphemeralSessionStatus::Completed,
            "{outcome:?}"
        );
        let result = outcome
            .messages
            .iter()
            .find_map(|message| match message {
                pi_core::Message::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("fixture tool result");
        assert_eq!(
            result.content,
            vec![ContentBlock::Text(TextContent::new(expected))]
        );
    }

    async fn assert_unavailable(&self, name: &str) {
        let before_calls = self.tool_calls.read().unwrap().len();
        let before_requests = self.requests.read().unwrap().len();
        let session = self.session.current();
        let context = ContextParts::new(session.plugin_context(PluginContextScope::Base));
        let error = context
            .session
            .run_ephemeral(Self::invocation_request(name), AbortHandle::new().1)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(name), "{error}");
        assert_eq!(self.tool_calls.read().unwrap().len(), before_calls);
        assert_eq!(self.requests.read().unwrap().len(), before_requests);
    }

    fn invocation_request(name: &str) -> EphemeralSessionRequest {
        EphemeralSessionRequest {
            system_prompt: Some("Call the fixture tool once.".into()),
            origin: "mcp-reload-test".into(),
            inherit_history: false,
            history_tail: None,
            messages: vec![pi_core::Message::User(pi_core::UserMessage::text(
                "call", 1,
            ))],
            tools: vec![name.into()],
            plugins: Vec::new(),
            model: None,
            thinking_level: None,
            max_tool_iterations: 2,
            max_input_tokens: None,
            compaction: None,
            // The first request initializes the native certificate store as well as HTTP.
            timeout: std::time::Duration::from_secs(30),
        }
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
async fn mcp_guidance_is_cached_on_reload_and_refreshed_on_reconnect_or_fingerprint_change() {
    let fixture = McpFixture::new(true).await;
    let assert_guidance = |expected: Option<&str>| {
        let requests = fixture.requests.read().unwrap();
        let request = requests.last().expect("main provider request");
        let prompt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| matches!(message["role"].as_str(), Some("system" | "developer")))
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            prompt.matches("Fixture guidance for").count(),
            usize::from(expected.is_some())
        );
        if let Some(expected) = expected {
            assert!(
                prompt.contains(expected),
                "missing current server instructions"
            );
            assert!(prompt.contains("namespace `mcp__fixture`"));
        }
    };

    let pi_session::PromptOutcome::Agent(outcome) = fixture
        .session
        .current()
        .prompt("Call the fixture MCP tool.")
        .await
        .unwrap()
    else {
        panic!("expected the main agent loop");
    };
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        pi_core::Message::ToolResult(result) if result.tool_name == "mcp__fixture__echo" && !result.is_error
    )));
    assert_guidance(Some("Fixture guidance for echo."));

    *fixture.tool_name.write().unwrap() = "renamed".into();
    fixture.session.current().prompt("Continue.").await.unwrap();
    // Instructions are a generation snapshot and do not accumulate between runs.
    assert_guidance(Some("Fixture guidance for echo."));

    fixture.session.reload().await.unwrap();
    fixture
        .session
        .current()
        .prompt("Continue after reload.")
        .await
        .unwrap();
    assert_guidance(Some("Fixture guidance for echo."));
    fixture.assert_discoveries(1);

    fixture
        .session
        .current()
        .prompt("/mcp reconnect fixture")
        .await
        .unwrap();
    fixture
        .session
        .current()
        .prompt("Continue after reconnect.")
        .await
        .unwrap();
    assert_guidance(Some("Fixture guidance for renamed."));
    fixture.assert_discoveries(2);

    *fixture.tool_name.write().unwrap() = "fingerprinted".into();
    fixture.set_policy(json!({"headers":{"x-fixture-generation":"changed"}}));
    fixture.session.reload().await.unwrap();
    fixture
        .session
        .current()
        .prompt("Continue after connection-input change.")
        .await
        .unwrap();
    assert_guidance(Some("Fixture guidance for fingerprinted."));
    fixture.assert_discoveries(3);

    fixture.set_enabled(false);
    fixture.session.reload().await.unwrap();
    fixture
        .session
        .current()
        .prompt("Continue without MCP.")
        .await
        .unwrap();
    assert_guidance(None);
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_reload_activates_a_newly_enabled_server() {
    let fixture = McpFixture::new(false).await;
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.set_enabled(true);
    fixture
        .session
        .current()
        .prompt("/mcp reload")
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
async fn hidden_mcp_tools_are_not_registered_active_or_callable() {
    let fixture = McpFixture::with_policy(true, json!({"exposure": "hidden"})).await;
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.assert_unavailable("mcp__fixture__echo").await;
    assert!(fixture.tool_calls.read().unwrap().is_empty());
    assert!(fixture.requests.read().unwrap().is_empty());
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_exact_direct_override_and_hidden_reload_preserve_session_usability() {
    let fixture = McpFixture::with_policy(
        true,
        json!({"exposure": "hidden", "toolExposure": {"e*": "hidden", "echo": "direct"}}),
    )
    .await;
    fixture.assert_tool("mcp__fixture__echo", true);
    // Persist a real turn so the subsequent reload exercises restored tool selection too.
    fixture
        .session
        .current()
        .prompt("Call the explicitly exposed fixture tool.")
        .await
        .unwrap();
    assert_eq!(fixture.tool_calls.read().unwrap().as_slice(), ["echo"]);

    fixture.set_policy(json!({"exposure": "hidden"}));
    fixture.session.reload().await.unwrap();
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.assert_unavailable("mcp__fixture__echo").await;
    fixture
        .session
        .current()
        .prompt("Continue after hiding the MCP tool.")
        .await
        .unwrap();
    assert_eq!(fixture.tool_calls.read().unwrap().as_slice(), ["echo"]);
    {
        let requests = fixture.requests.read().unwrap();
        let tools = requests.last().unwrap()["tools"].as_array().unwrap();
        assert!(tools.iter().all(|tool| {
            !tool["function"]["name"]
                .as_str()
                .unwrap()
                .starts_with("mcp__fixture__")
        }));
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn invalid_mcp_exposure_reload_preserves_the_previous_connection() {
    let fixture = McpFixture::new(true).await;
    let before = fixture.session.current();
    fixture.set_policy(json!({"exposure": "invalid-exposure"}));
    let Err(error) = fixture.session.reload().await else {
        panic!("invalid exposure must reject reload");
    };
    assert!(error.to_string().contains("exposure"), "{error}");
    assert!(Arc::ptr_eq(&before, &fixture.session.current()));
    fixture.assert_tool("mcp__fixture__echo", true);
    fixture.invoke("mcp__fixture__echo", "echo").await;
    assert_eq!(fixture.tool_calls.read().unwrap().as_slice(), ["echo"]);
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
async fn mcp_catalog_is_cached_on_reload_and_refreshed_on_reconnect_or_fingerprint_change() {
    let fixture = McpFixture::new(true).await;
    fixture.assert_tool("mcp__fixture__echo", true);
    *fixture.tool_name.write().unwrap() = "renamed".into();
    fixture.session.reload().await.unwrap();
    fixture.assert_tool("mcp__fixture__echo", true);
    fixture.assert_tool("mcp__fixture__renamed", false);
    fixture.invoke("mcp__fixture__echo", "echo").await;
    fixture.assert_discoveries(1);

    fixture
        .session
        .current()
        .prompt("/mcp reconnect fixture")
        .await
        .unwrap();
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.assert_tool("mcp__fixture__renamed", true);
    fixture.invoke("mcp__fixture__renamed", "renamed").await;
    fixture.assert_discoveries(2);

    *fixture.tool_name.write().unwrap() = "fingerprinted".into();
    fixture.set_policy(json!({"headers":{"x-fixture-generation":"changed"}}));
    fixture.session.reload().await.unwrap();
    fixture.assert_tool("mcp__fixture__renamed", false);
    fixture.assert_tool("mcp__fixture__fingerprinted", true);
    fixture
        .invoke("mcp__fixture__fingerprinted", "fingerprinted")
        .await;
    fixture.assert_discoveries(3);
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
async fn mcp_reload_isolates_an_unavailable_sibling_and_reuses_the_healthy_connection() {
    let fixture = McpFixture::new(true).await;
    let before = fixture.session.current();
    fs::write(
        fixture.config_path(),
        json!({"version": 1, "mcpServers": {
            "fixture": {"type": "http", "url": fixture.url, "exposure":"direct"},
            "unavailable": {"type": "stdio", "command": fixture.directory.path().join("missing-server")}
        }})
        .to_string(),
    )
    .unwrap();
    fixture.session.reload().await.unwrap();
    assert!(!Arc::ptr_eq(&before, &fixture.session.current()));
    let status =
        super::mcp_test_support::wait_for_status(&fixture.session, "unavailable", "failed").await;
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("fixture ·") && line.contains("connected")),
        "{status}"
    );
    fixture.assert_tool("mcp__fixture__echo", true);
    assert!(
        !fixture
            .session
            .current()
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name.starts_with("mcp__unavailable__"))
    );
    assert!(fixture.requests.read().unwrap().is_empty());
    assert!(!fixture.session.path().exists());
    fixture.invoke("mcp__fixture__echo", "echo").await;
    fixture.assert_discoveries(1);
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_removal_allows_resuming_a_session_that_used_the_old_tool() {
    let mut fixture = McpFixture::new(true).await;
    fixture.assert_tool("mcp__fixture__echo", true);
    let path = fixture.session.current().history().path().to_path_buf();
    // Persist a real local-provider turn using the MCP tool before removing it.
    let pi_session::PromptOutcome::Agent(outcome) = fixture
        .session
        .current()
        .prompt("initialize persisted session")
        .await
        .unwrap()
    else {
        panic!("expected an agent run");
    };
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        pi_core::Message::ToolResult(result) if result.tool_name == "mcp__fixture__echo" && !result.is_error
    )));
    fixture
        .pi
        .sessions()
        .close_session(&fixture.session)
        .await
        .unwrap();
    fixture.set_enabled(false);
    fixture.session = fixture
        .pi
        .sessions()
        .open_session(path, Default::default())
        .await
        .unwrap();
    fixture.assert_tool("mcp__fixture__echo", false);
    fixture.shutdown().await;
}
