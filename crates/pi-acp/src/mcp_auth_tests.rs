use super::*;
use axum::response::IntoResponse;
use pi_plugin_mcp::McpOAuthTokens;

#[tokio::test]
async fn acp_reports_auth_required_and_reuses_only_host_supplied_durable_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new().route("/mcp", axum::routing::post(
        |headers: axum::http::HeaderMap, axum::Json(request): axum::Json<serde_json::Value>| async move {
            if headers.get("authorization").and_then(|value| value.to_str().ok()) != Some("Bearer acp-fixture") {
                return (axum::http::StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer realm=private-challenge")]).into_response();
            }
            if request["id"].is_null() { return axum::http::StatusCode::ACCEPTED.into_response(); }
            let response = if request["method"] == "initialize" {
                serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":{
                    "protocolVersion":"2025-03-26", "capabilities":{}, "serverInfo":{"name":"fixture","version":"1"}
                }})
            } else {
                serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32601,"message":"Initialize instead"}})
            };
            axum::Json(response).into_response()
        }
    ));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    // ACP must never interpret this local configuration, even though it is malformed.
    std::fs::create_dir(directory.path().join(".pi")).unwrap();
    std::fs::write(directory.path().join(".pi/mcp.json"), "not json").unwrap();
    let configs = || {
        vec![acp::McpServer::Http(acp::McpServerHttp::new(
            "account", &url,
        ))]
    };
    let error = generation_overlay(configs(), directory.path(), None)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, acp::ErrorCode::AuthRequired);
    let wire = serde_json::to_string(&error).unwrap();
    assert!(wire.contains("pi mcp login account"));
    assert!(!wire.contains("private-challenge"));
    let store = McpOAuthCredentialStore::new(directory.path().join("mcp-auth.json"));
    store
        .save_tokens(
            "account",
            &url,
            "fixture",
            McpOAuthTokens {
                access_token: "acp-fixture".into(),
                token_type: "Bearer".into(),
                refresh_token: None,
                expires_in: None,
                scope: None,
            },
        )
        .unwrap();
    // An embedding host must explicitly choose this store; the implicit memory store stays isolated.
    assert!(
        generation_overlay(configs(), directory.path(), None)
            .await
            .is_err()
    );
    let (_, pool) = generation_overlay(configs(), directory.path(), Some(&store))
        .await
        .unwrap();
    pool.unwrap().shutdown().await.unwrap();
    server.abort();
}
