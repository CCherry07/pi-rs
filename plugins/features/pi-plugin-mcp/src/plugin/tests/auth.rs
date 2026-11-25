use super::*;
use crate::{
    McpOAuthCredentialStore, McpOAuthError, McpOAuthHost, McpOAuthLoginRequest, McpOAuthPrompt,
    McpOAuthTokens,
};
use axum::response::IntoResponse;
use serde_json::{Value, json};

struct ScriptedRedirect;
#[async_trait]
impl McpOAuthHost for ScriptedRedirect {
    async fn sign_in(
        &self,
        request: McpOAuthLoginRequest,
        _: Arc<dyn McpOAuthPrompt>,
        signal: pi_core::AbortSignal,
    ) -> Result<(), McpOAuthError> {
        let (mut flow, redirect) = request
            .start("http://127.0.0.1:53682/callback", signal.clone())
            .await?;
        flow.complete_redirect_url(
            &format!(
                "{}?code=fixture-code&state={}",
                redirect.redirect_url, redirect.state
            ),
            signal,
        )
        .await
    }
}

#[tokio::test]
async fn semantic_auth_mutations_reload_and_notify_only_through_the_replacement_context() {
    for (login, named) in [(true, true), (true, false), (false, true), (false, false)] {
        for reload_succeeds in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let metadata = json!({"issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),"code_challenge_methods_supported":["S256"]});
            let app = axum::Router::new()
                .route("/metadata",axum::routing::get(move || {let metadata = metadata.clone(); async move {axum::Json(metadata)}}))
                .route("/token",axum::routing::post(|body:String| async move {
                    assert!(body.contains("code=fixture-code") && body.contains("code_verifier="));
                    axum::Json(json!({"access_token":"fixture-access","token_type":"Bearer","expires_in":3600}))
                }))
                .route("/mcp",axum::routing::post(|headers:axum::http::HeaderMap,axum::Json(request):axum::Json<Value>| async move {
                    if headers.get("authorization").and_then(|header| header.to_str().ok()) != Some("Bearer fixture-access") {
                        return (axum::http::StatusCode::UNAUTHORIZED,[("www-authenticate","Bearer realm=fixture")]).into_response();
                    }
                    if request["id"].is_null() {return axum::http::StatusCode::ACCEPTED.into_response();}
                    if request["method"] == "initialize" {
                        axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}})).into_response()
                    } else {
                        axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Use initialize"}})).into_response()
                    }
                }));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("mcp.json"),json!({"mcpServers":{"account":{"url":format!("{origin}/mcp"),"oauth":{"clientId":"fixture","authServerMetadataUrl":format!("{origin}/metadata")}}}}).to_string()).unwrap();
            let library = McpLibrary::new(directory.path(), None, false)
                .with_oauth_host(Arc::new(ScriptedRedirect));
            if !login {
                McpOAuthCredentialStore::new(directory.path().join("mcp-auth.json"))
                    .save_tokens(
                        "account",
                        &format!("{origin}/mcp"),
                        "fixture",
                        McpOAuthTokens {
                            access_token: "fixture-access".into(),
                            token_type: "Bearer".into(),
                            refresh_token: None,
                            expires_in: None,
                            scope: None,
                        },
                    )
                    .unwrap();
            }
            let command = McpCommand {
                rows: library.read(McpScope::Global).unwrap().servers,
                library: library.clone(),
                pool: McpToolSet::connect_with_policies(vec![]).await.unwrap(),
            };
            let next_access = Arc::new(ManagerAccess::new(vec![], None));
            let next = reload_succeeds.then(|| PluginContextEpoch::new(next_access.clone()));
            let access = Arc::new(ManagerAccess::new(vec![], next));
            let epoch = PluginContextEpoch::new(access.clone());
            *access.old.lock().unwrap() = Some(epoch.clone());
            let context = CommandContext::with_plugin_context(
                directory.path().into(),
                pi_core::AbortHandle::new().1,
                epoch.command_context(),
            );
            let result = command
                .execute(
                    context.clone(),
                    format!(
                        "{}{}",
                        if login { "login" } else { "logout" },
                        if named { " account" } else { "" }
                    ),
                )
                .await;
            assert_eq!(access.reloads.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                library.has_oauth_credentials("account").unwrap(),
                login,
                "disk auth mutation survives reload failure"
            );
            assert!(
                access.notices.lock().unwrap().is_empty(),
                "no success on retired/failed old context"
            );
            if reload_succeeds {
                result.unwrap();
                assert!(matches!(
                    context.ui.is_available(),
                    Err(pi_plugin::PluginContextError::Retired)
                ));
                let notices = next_access.notices.lock().unwrap();
                assert_eq!(notices.len(), 1);
                assert!(notices[0].contains(if login { "Signed in" } else { "Signed out" }));
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("previous generation remains active")
                );
                assert!(context.ui.is_available().unwrap());
                assert!(next_access.notices.lock().unwrap().is_empty());
            }
            access.old.lock().unwrap().take();
            assert!(!directory.path().join("sessions").exists());
            server.abort();
        }
    }
}
