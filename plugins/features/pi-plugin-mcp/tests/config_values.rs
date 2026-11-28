//! Trust and execution boundaries for command-backed local MCP configuration.
use std::fs;
#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use pi_core::AbortHandle;
use pi_plugin_mcp::{McpLibrary, McpScope, McpServerState};
use serde_json::json;

#[tokio::test]
async fn authoring_metadata_disabled_and_untrusted_entries_never_execute_commands() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    fs::create_dir(&agent).unwrap();
    fs::create_dir(dir.path().join(".pi")).unwrap();
    let content = json!({"mcpServers":{"global":{
        "url":"http://127.0.0.1:9/mcp",
        "oauth":{"clientId":"fixture","clientSecret":"!touch forbidden; printf secret"},
        "headers":{"X-Private":"!touch forbidden; printf header"}
    }}})
    .to_string();
    fs::write(agent.join("mcp.json"), &content).unwrap();
    fs::write(
        dir.path().join(".pi/mcp.json"),
        json!({"mcpServers":{"project":{
            "command":"never-start", "env":{"SECRET":"!touch forbidden; printf secret"}
        }}})
        .to_string(),
    )
    .unwrap();
    let library = McpLibrary::new(&agent, Some(dir.path()), false).with_command_values(true);
    let snapshot = library.read(McpScope::Global).unwrap();
    assert!(snapshot.diagnostic.is_none());
    library
        .save(McpScope::Global, &snapshot.revision, &content)
        .unwrap();
    library.credential_watch().unwrap();
    assert!(!library.has_oauth_credentials("global").unwrap());
    library.logout("global", None).unwrap();
    assert!(!dir.path().join("forbidden").exists());
    let restricted = McpLibrary::new(&agent, Some(dir.path()), false);
    let result = restricted
        .begin_login("global", None, AbortHandle::new().1)
        .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("explicit local-library opt-in")
    );
    assert!(!dir.path().join("forbidden").exists());
    library
        .set_enabled(McpScope::Global, "global", false)
        .unwrap();
    library.prepare_generation().await.unwrap();
    let inspection = library.inspect().await;
    assert_eq!(
        inspection.servers.len(),
        1,
        "untrusted project stays unread"
    );
    assert_eq!(inspection.servers[0].state, McpServerState::Disabled);
    assert!(
        library
            .begin_login("global", None, AbortHandle::new().1)
            .await
            .is_err()
    );
    assert!(!dir.path().join("forbidden").exists());
    assert!(!agent.join("sessions").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn trusted_live_inspection_rereads_helpers_and_static_authorization_wins() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route(
        "/mcp",
        post({
            let seen = seen.clone();
            move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(headers["authorization"].to_str().unwrap().to_string());
                    (
                        StatusCode::UNAUTHORIZED,
                        [("www-authenticate", "Bearer realm=fixture")],
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    fs::create_dir(&agent).unwrap();
    fs::create_dir(dir.path().join(".pi")).unwrap();
    let content = json!({"mcpServers":{"project":{
        "url":url, "headers":{"Authorization":"!printf x >> invoked; printf 'Bearer '; read value < token; printf '%s' \"$value\""},
        "oauth":{"clientId":"fixture","clientSecret":"!touch forbidden; printf secret"}
    }}}).to_string();
    fs::write(dir.path().join(".pi/mcp.json"), &content).unwrap();
    let library = McpLibrary::new(&agent, Some(dir.path()), true).with_command_values(true);
    for value in ["first-fixture", "rotated-fixture"] {
        fs::write(dir.path().join("token"), format!("{value}\n")).unwrap();
        library.credential_watch().unwrap();
        let report = library.inspect().await;
        // Static Authorization bypasses OAuth; a rejected header is a failure, not
        // an actionable OAuth challenge (legacy McpServerConnection.needsSignIn).
        assert_eq!(report.servers[0].state, McpServerState::Failed);
        assert!(
            report.servers[0]
                .error
                .as_deref()
                .unwrap()
                .contains("handshake failed")
        );
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains(value));
    }
    assert_eq!(
        &*seen.lock().unwrap(),
        &["Bearer first-fixture", "Bearer rotated-fixture"]
    );
    assert_eq!(fs::read(dir.path().join("invoked")).unwrap(), b"xx");
    assert!(!dir.path().join("forbidden").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join(".pi/mcp.json")).unwrap(),
        content
    );
    assert!(!agent.join("sessions").exists());
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn local_library_valid_token_inspection_never_resolves_its_oauth_secret() {
    use axum::response::IntoResponse;
    use pi_plugin_mcp::{McpOAuthCredentialStore, McpOAuthTokens};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let url = format!("{origin}/mcp");
    let metadata = json!({"issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token")});
    let app = Router::new()
        .route("/metadata", axum::routing::get(move || {let value = metadata.clone(); async {axum::Json(value)}}))
        .route("/mcp", post(|headers:HeaderMap, axum::Json(request):axum::Json<serde_json::Value>| async move {
            assert_eq!(headers["authorization"], "Bearer fixture-valid-token");
            if request["id"].is_null() {return StatusCode::ACCEPTED.into_response();}
            if request["method"] == "server/discover" {
                return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"legacy fixture"}})).into_response();
            }
            let result = if request["method"] == "initialize" {
                json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            } else { json!({"tools":[]}) };
            axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
        }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("mcp.json"), json!({"mcpServers":{"fixture":{
        "url":url,"exposure":"direct","oauth":{"clientId":"fixture", "clientSecret":"!touch forbidden; exit 1", "authServerMetadataUrl":format!("{origin}/metadata")}
    }}}).to_string()).unwrap();
    McpOAuthCredentialStore::new(dir.path().join("mcp-auth.json"))
        .save_tokens(
            "fixture",
            &url,
            "fixture",
            McpOAuthTokens {
                access_token: "fixture-valid-token".into(),
                token_type: "Bearer".into(),
                refresh_token: Some("unused-refresh".into()),
                expires_in: Some(3600),
                scope: None,
            },
        )
        .unwrap();
    let library = McpLibrary::new(dir.path(), None, false).with_command_values(true);
    assert!(library.inspect().await.is_ok());
    assert!(library.test("fixture").await.unwrap().is_empty());
    assert!(!dir.path().join("forbidden").exists());
    assert!(!dir.path().join("sessions").exists());
    server.abort();
}
