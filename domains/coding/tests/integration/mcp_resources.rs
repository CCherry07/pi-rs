//! Resource-only MCP reaches the model through the normal generation/session seam.

use std::fs;
use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;
use pi_coding::{Config, Pi};
use pi_core::{ContentBlock, Message};
use serde_json::{Value, json};

const RESOURCE_TOOLS: [&str; 3] = [
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
];

async fn provider_response(
    axum::extract::State(requests): axum::extract::State<Arc<Mutex<Vec<Value>>>>,
    axum::Json(request): axum::Json<Value>,
) -> impl IntoResponse {
    requests.lock().unwrap().push(request.clone());
    let completed = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .count();
    let (delta, finish_reason) = if completed >= 2 {
        (
            json!({"role":"assistant","content":"Resource understood."}),
            "stop",
        )
    } else {
        let (name, arguments) = if completed == 0 {
            ("list_mcp_resources", "{}")
        } else {
            (
                "read_mcp_resource",
                r#"{"server":"docs","uri":"doc://guide"}"#,
            )
        };
        assert!(
            request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == name)
        );
        (
            json!({"role":"assistant","tool_calls":[{
                "index":0,"id":format!("resource-call-{completed}"),"type":"function",
                "function":{"name":name,"arguments":arguments}
            }]}),
            "tool_calls",
        )
    };
    let chunk = json!({
        "id":"resources","object":"chat.completion.chunk","model":"test",
        "choices":[{"index":0,"delta":delta,"finish_reason":null}]
    });
    let end = json!({
        "id":"resources","object":"chat.completion.chunk","model":"test",
        "choices":[{"index":0,"delta":{},"finish_reason":finish_reason}]
    });
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
    )
}

#[tokio::test]
async fn resource_only_mcp_is_callable_and_hidden_reload_preserves_its_history() {
    let provider_requests = Arc::new(Mutex::new(Vec::new()));
    let resource_requests = Arc::new(Mutex::new(Vec::new()));
    let router = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::post({
                let requests = resource_requests.clone();
                move |axum::Json(request): axum::Json<Value>| {
                    let requests = requests.clone();
                    async move {
                        let id = &request["id"];
                        if id.is_null() {
                            return axum::http::StatusCode::ACCEPTED.into_response();
                        }
                        let result = match request["method"].as_str().unwrap() {
                    "server/discover" => return axum::Json(json!({
                        "jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Use initialize"}
                    })).into_response(),
                    "initialize" => json!({
                        "protocolVersion":"2025-03-26","capabilities":{"resources":{}},
                        "serverInfo":{"name":"resources","version":"1"},
                        "instructions":"Resource fixture instructions."
                    }),
                    "resources/list" => {
                        requests.lock().unwrap().push("resources/list");
                        json!({"resources":[{"uri":"doc://guide","name":"guide"}]})
                    }
                    "resources/read" => {
                        requests.lock().unwrap().push("resources/read");
                        assert_eq!(request["params"]["uri"], "doc://guide");
                        json!({"contents":[{"uri":"doc://guide","text":"Reference contents."}]})
                    }
                    method => panic!("unexpected MCP request {method}"),
                };
                        axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
                    }
                }
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(provider_response),
        )
        .with_state(provider_requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let cwd = directory.path().join("project");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(
        agent_dir.join("memory.json"),
        r#"{"version":1,"enabled":false}"#,
    )
    .unwrap();
    let write_mcp = |hidden| {
        fs::write(
            agent_dir.join("mcp.json"),
            json!({"mcpServers":{"docs":{
                "url":format!("{base}/mcp"),"exposure":if hidden { "hidden" } else { "direct" }
            }}})
            .to_string(),
        )
        .unwrap();
    };
    write_mcp(false);
    fs::write(
        agent_dir.join("models.json"),
        json!({"providers":{"fixture":{
            "baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"fixture",
            "models":[{"id":"test","contextWindow":100000,"maxTokens":1024}]
        }}})
        .to_string(),
    )
    .unwrap();
    let mut config = Config::new(cwd.clone(), agent_dir.clone());
    config.provider = "fixture".into();
    config.model = Some("test".into());
    config.discover_extensions = false;
    config.trust_override = Some(false);
    let pi = Pi::builder(config).build().unwrap();
    let session_path = agent_dir.join("session.jsonl");
    let mut session = pi
        .sessions()
        .create_session(pi_session::CreateSessionOptions::new(&cwd, &session_path))
        .await
        .unwrap();
    let pi_session::PromptOutcome::Agent(outcome) = session
        .current()
        .prompt("Read the fixture guide.")
        .await
        .unwrap()
    else {
        panic!("expected agent turn")
    };
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        Message::ToolResult(result) if result.tool_name == "read_mcp_resource" && result.content.iter().any(|content| matches!(content, ContentBlock::Text(text) if text.text == "Reference contents."))
    )));
    assert_eq!(
        *resource_requests.lock().unwrap(),
        ["resources/list", "resources/read"]
    );
    assert!(session_path.exists());
    assert!(
        provider_requests.lock().unwrap()[0]["messages"]
            .to_string()
            .contains("Resource fixture instructions.")
    );
    let history_before = fs::read_to_string(&session_path).unwrap();
    assert!(history_before.contains("list_mcp_resources"));
    assert!(history_before.contains("read_mcp_resource"));

    write_mcp(true);
    session.reload().await.unwrap();
    for name in RESOURCE_TOOLS {
        assert!(
            !session
                .current()
                .catalog()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == name)
        );
        assert!(
            !session
                .current()
                .snapshot()
                .agent
                .active_tools
                .contains(&name.to_string())
        );
    }
    pi.sessions().close_session(&session).await.unwrap();
    session = pi
        .sessions()
        .open_session(&session_path, Default::default())
        .await
        .unwrap();
    session
        .current()
        .prompt("Continue after hiding the server.")
        .await
        .unwrap();
    assert_eq!(
        *resource_requests.lock().unwrap(),
        ["resources/list", "resources/read"]
    );
    {
        let requests = provider_requests.lock().unwrap();
        let last = requests.last().unwrap();
        assert!(
            !last["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|tool| RESOURCE_TOOLS.contains(&tool["function"]["name"].as_str().unwrap()))
        );
        assert!(
            !last["messages"]
                .to_string()
                .contains("Resource fixture instructions.")
        );
    }
    assert!(
        fs::read_to_string(&session_path)
            .unwrap()
            .contains("Reference contents.")
    );
    pi.sessions().shutdown().await.unwrap();
    server.abort();
}
