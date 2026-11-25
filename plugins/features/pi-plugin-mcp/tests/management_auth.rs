//! Management must use actual OAuth state and preserve defining-scope/trust restrictions.
use axum::response::IntoResponse;
use pi_core::AbortHandle;
use pi_plugin_mcp::{McpLibrary, McpOAuthError, McpScope};
use serde_json::json;

#[tokio::test]
async fn management_sign_in_validates_redirect_and_logout_removes_real_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let metadata = json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/authorize"),
        "token_endpoint": format!("{origin}/token"),
        "code_challenge_methods_supported": ["S256"]
    });
    let app = axum::Router::new()
        .route("/metadata", axum::routing::get(move || {
            let metadata = metadata.clone();
            async move { axum::Json(metadata) }
        }))
        .route("/mcp", axum::routing::post(|headers: axum::http::HeaderMap, axum::Json(request): axum::Json<serde_json::Value>| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-access");
            if request["id"].is_null() { return axum::http::StatusCode::ACCEPTED.into_response(); }
            let response = if request["method"] == "initialize" {
                json!({"jsonrpc":"2.0", "id":request["id"], "result":{
                    "protocolVersion":"2025-03-26", "capabilities":{}, "serverInfo":{"name":"auth-fixture", "version":"1"}
                }})
            } else {
                json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32601,"message":"Use initialize"}})
            };
            axum::Json(response).into_response()
        }))
        .route("/token", axum::routing::post(|body: String| async move {
            assert!(body.contains("code=fixture-code"));
            assert!(body.contains("code_verifier="));
            axum::Json(json!({"access_token":"fixture-access", "token_type":"Bearer", "expires_in":3600}))
        }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("mcp.json"),
        json!({"mcpServers": {
            "account": {"url":format!("{origin}/mcp"), "oauth":{
                "clientId":"fixture-client", "callbackPort":8910,
                "authServerMetadataUrl":format!("{origin}/metadata")
            }}
        }})
        .to_string(),
    )
    .unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let watch = library.credential_watch().unwrap();
    let revision = watch.oauth_revision().unwrap();
    assert!(!library.has_oauth_credentials("account").unwrap());
    let (mut flow, redirect) = library
        .begin_login("account", None, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(redirect.redirect_url, "http://127.0.0.1:8910/callback");
    assert_eq!(
        watch.oauth_revision().unwrap(),
        revision,
        "pending login is not a credential change"
    );
    assert!(
        !library.has_oauth_credentials("account").unwrap(),
        "starting sign-in is not authentication"
    );
    assert!(matches!(
        flow.complete_redirect_url(
            "http://127.0.0.1:8910/callback?code=fixture-code&state=wrong",
            AbortHandle::new().1
        )
        .await,
        Err(McpOAuthError::RedirectMismatch)
    ));
    flow.complete_redirect_url(
        &format!(
            "{}?code=fixture-code&state={}",
            redirect.redirect_url, redirect.state
        ),
        AbortHandle::new().1,
    )
    .await
    .unwrap();
    assert!(library.has_oauth_credentials("account").unwrap());
    assert_ne!(watch.oauth_revision().unwrap(), revision);
    let inspection = library.inspect().await;
    assert!(inspection.is_ok(), "{inspection:?}");
    assert_eq!(
        inspection.servers[0].state,
        pi_plugin_mcp::McpServerState::Connected
    );
    assert!(
        library
            .logout("account", None)
            .unwrap()
            .contains("Signed out")
    );
    assert!(!library.has_oauth_credentials("account").unwrap());
    assert!(
        library
            .logout("account", None)
            .unwrap()
            .contains("No stored credentials")
    );
    server.abort();
}

#[tokio::test]
async fn authorization_challenges_are_visible_and_do_not_prevent_the_management_generation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                [("www-authenticate", "Bearer realm=must-not-log-challenge")],
            )
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("mcp.json"),
        json!({"mcpServers":{"account":{"url":url}}}).to_string(),
    )
    .unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let inspection = library.inspect().await;
    assert!(!inspection.is_ok());
    assert_eq!(
        serde_json::to_value(&inspection.servers[0]).unwrap()["state"],
        "needs-auth"
    );
    assert!(
        inspection.servers[0]
            .error
            .as_deref()
            .unwrap()
            .contains("pi mcp login account")
    );
    assert!(!format!("{inspection:?}").contains("must-not-log-challenge"));
    assert!(
        library.prepare().await.is_ok(),
        "The /mcp control plane must remain reachable before sign-in"
    );
    let error = pi_plugin_mcp::McpToolSet::connect(vec![pi_plugin_mcp::McpServerConfig::http(
        "account", url,
    )])
    .await
    .unwrap_err();
    assert!(error.to_string().contains("requires authorization"));
    assert!(!format!("{error:?}").contains("must-not-log-challenge"));
    server.abort();
}

#[tokio::test]
async fn management_auth_rejects_wrong_scope_header_provider_and_untrusted_project() {
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let project = directory.path().join("project");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(project.join(".pi")).unwrap();
    std::fs::write(agent.join("mcp.json"), json!({"mcpServers": {
        "account": {"url":"https://example.invalid/mcp"},
        "header": {"url":"https://example.invalid/mcp", "headers":{"authorization":"Bearer fixture"}},
        "provider": {"url":"https://example.invalid/mcp", "auth":{"provider":"anthropic"}},
        "stdio": {"command":"unused"}
    }}).to_string()).unwrap();
    std::fs::write(
        project.join(".pi/mcp.json"),
        r#"{"mcpServers":{"account":{"exposure":"direct"}}}"#,
    )
    .unwrap();
    let library = McpLibrary::new(&agent, Some(&project), true);
    for name in ["header", "provider", "stdio"] {
        assert!(
            library
                .begin_login(name, None, AbortHandle::new().1)
                .await
                .is_err()
        );
        assert!(library.logout(name, None).is_err());
    }
    assert!(
        library
            .logout("account", Some(McpScope::Project))
            .unwrap_err()
            .contains("another scope")
    );
    let library = McpLibrary::new(&agent, Some(&project), false);
    assert!(
        library
            .logout("account", Some(McpScope::Project))
            .unwrap_err()
            .contains("trust")
    );
    assert!(!agent.join("mcp-auth.json").exists());
    let (abort, signal) = AbortHandle::new();
    abort.abort();
    assert!(matches!(
        library.begin_login("account", None, signal).await,
        Err(McpOAuthError::Cancelled)
    ));
}
