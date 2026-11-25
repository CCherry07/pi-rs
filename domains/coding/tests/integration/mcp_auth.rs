//! Oracle: agent-session-mcp-oauth.test.ts external-login pickup, adapted to immutable generations.
use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;
use pi_coding::{Config, Pi};
use pi_plugin_mcp::{McpOAuthCredentialStore, McpOAuthTokens};
use serde_json::{Value, json};

fn mcp_response(request: &Value) -> axum::response::Response {
    if request["id"].is_null() {
        return axum::http::StatusCode::ACCEPTED.into_response();
    }
    let result = match request["method"].as_str().unwrap() {
        "initialize" => json!({"protocolVersion":"2025-03-26", "capabilities":{"tools":{}}, "serverInfo":{"name":"fixture","version":"1"}}),
        "tools/list" => json!({"tools":[{"name":"whoami","description":"Who is signed in","inputSchema":{"type":"object"}}]}),
        _ => return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Use initialize"}})).into_response(),
    };
    axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
}

async fn external_login(provider_auth: bool) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(|headers: axum::http::HeaderMap, axum::Json(request): axum::Json<Value>| async move {
            if headers.get("authorization").and_then(|value| value.to_str().ok()) != Some("Bearer accepted-fixture") {
                return (axum::http::StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer realm=fixture")]).into_response();
            }
            mcp_response(&request)
        }))
        .route("/healthy", axum::routing::post(|axum::Json(request): axum::Json<Value>| async move {
            mcp_response(&request)
        }))
        .route("/v1/chat/completions", axum::routing::post({
            let requests = requests.clone();
            move |axum::Json(request): axum::Json<Value>| {
                requests.lock().unwrap().push(request);
                async move {
                    let chunk = json!({"id":"auth-test","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{"role":"assistant","content":"Ready"},"finish_reason":null}]});
                    let end = json!({"id":"auth-test","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                    ([("content-type","text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let project = directory.path().join("project");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let mut definition = json!({"url":format!("{base}/mcp"), "exposure":"direct"});
    if provider_auth {
        definition["auth"] = json!({"provider":"fixture-provider"});
    }
    std::fs::write(
        agent.join("mcp.json"),
        json!({"mcpServers":{
            "account":definition,
            "healthy":{"url":format!("{base}/healthy"),"exposure":"direct"}
        }})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent.join("models.json"),
        json!({"providers":{"fixture":{
            "baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"fixture",
            "models":[{"id":"test","contextWindow":100000,"maxTokens":1024}]
        }}})
        .to_string(),
    )
    .unwrap();
    let mut config = Config::new(project.clone(), agent.clone());
    config.provider = "fixture".into();
    config.model = Some("test".into());
    config.discover_extensions = false;
    config.trust_override = Some(false);
    config.features = pi_coding::Features::none();
    let pi = Pi::builder(config).build().unwrap();
    let path = agent.join("draft.jsonl");
    let session = pi
        .sessions()
        .create_session(pi_session::CreateSessionOptions::new(&project, &path))
        .await
        .unwrap();
    let before = session.current();
    assert!(
        !before
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == "mcp__account__whoami")
    );
    assert!(!path.exists());
    assert!(
        before
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == "mcp__healthy__whoami")
    );
    let status = super::mcp_test_support::status(&session).await;
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("account ·") && line.contains("needs sign-in")),
        "{status}"
    );
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("healthy ·") && line.contains("connected")),
        "{status}"
    );
    assert!(requests.lock().unwrap().is_empty());
    if provider_auth {
        pi_coding::modify_credentials(&agent, |credentials| {
            credentials.insert(
                "fixture-provider".into(),
                serde_json::from_value(json!({"type":"api_key","key":"accepted-fixture"})).unwrap(),
            );
        })
        .unwrap();
    } else {
        McpOAuthCredentialStore::new(agent.join("mcp-auth.json"))
            .save_tokens(
                "account",
                &format!("{base}/mcp"),
                "fixture",
                McpOAuthTokens {
                    access_token: "accepted-fixture".into(),
                    token_type: "Bearer".into(),
                    refresh_token: None,
                    expires_in: None,
                    scope: None,
                },
            )
            .unwrap();
    }
    // A server-local HTTP/auth failure is partial, not a reason to block healthy
    // siblings. Malformed configuration is genuinely global and must prevent admission.
    let config_path = agent.join("mcp.json");
    let valid_config = std::fs::read(&config_path).unwrap();
    std::fs::write(&config_path, "invalid global MCP configuration").unwrap();
    assert!(before.prompt("must not submit").await.is_err());
    assert!(Arc::ptr_eq(&before, &session.current()));
    assert!(requests.lock().unwrap().is_empty());
    assert!(!path.exists());
    std::fs::write(&config_path, valid_config).unwrap();
    before.prompt("use signed-in tools").await.unwrap();
    assert!(!Arc::ptr_eq(&before, &session.current()));
    assert!(
        session
            .current()
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == "mcp__account__whoami")
    );
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "mcp__account__whoami")
        );
    }
    let ready = session.current();
    let store = McpOAuthCredentialStore::new(agent.join("mcp-auth.json"));
    store
        .save_tokens(
            "unrelated",
            &format!("{base}/other"),
            "fixture",
            McpOAuthTokens {
                access_token: "other-fixture".into(),
                token_type: "Bearer".into(),
                refresh_token: None,
                expires_in: None,
                scope: None,
            },
        )
        .unwrap();
    pi_coding::modify_credentials(&agent, |credentials| {
        credentials.insert(
            "unrelated-provider".into(),
            serde_json::from_value(json!({"type":"api_key","key":"other-fixture"})).unwrap(),
        );
    })
    .unwrap();
    ready
        .prompt("unrelated login does not replace my generation")
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&ready, &session.current()));
    if provider_auth {
        pi_coding::modify_credentials(&agent, |credentials| {
            credentials.remove("fixture-provider");
        })
        .unwrap();
    } else {
        store.sign_out("account", &format!("{base}/mcp")).unwrap();
    }
    ready.prompt("after external logout").await.unwrap();
    assert!(!Arc::ptr_eq(&ready, &session.current()));
    assert!(
        session
            .current()
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == "mcp__healthy__whoami")
    );
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[2]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "mcp__healthy__whoami")
        );
        assert!(requests[2]["tools"].as_array().is_none_or(|tools| {
            tools
                .iter()
                .all(|tool| tool["function"]["name"] != "mcp__account__whoami")
        }));
    }
    pi.sessions().shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn mcp_external_oauth_login_is_atomic_on_global_failure_and_isolates_auth_faults() {
    external_login(false).await;
}

#[tokio::test]
async fn mcp_external_provider_login_is_atomic_on_global_failure_and_isolates_auth_faults() {
    external_login(true).await;
}
