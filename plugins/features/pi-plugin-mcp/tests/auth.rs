use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::response::IntoResponse;
use pi_core::AbortHandle;
use pi_plugin_mcp::{
    McpClientAuth, McpLibrary, McpOAuthClientRegistration, McpOAuthConfig, McpOAuthCredentialStore,
    McpOAuthError, McpOAuthSignIn, McpOAuthTokens, McpProviderTokenResolver, McpServerConfig,
    McpToolSet,
};
use serde_json::{Value, json};

#[test]
fn http_auth_configuration_is_validated_without_echoing_secrets() {
    let valid = r#"{
        "mcpServers": {
            "oauth": {
                "url": "https://example.com/mcp",
                "oauth": {
                    "clientId": "registered",
                    "clientSecret": "${CLIENT_SECRET}",
                    "callbackPort": 8765,
                    "callbackUrl": "http://127.0.0.1:8765/callback",
                    "scope": "read write",
                    "clientRegistration": "auto",
                    "authServerMetadataUrl": "https://login.example.com/.well-known/openid-configuration"
                }
            },
            "provider": {
                "url": "${MCP_URL}",
                "auth": { "provider": "anthropic" }
            }
        }
    }"#;
    let entries = pi_plugin_mcp::config::parse_config(valid).unwrap();
    assert_eq!(entries.len(), 2);
    let debug = format!(
        "{:?}",
        McpServerConfig::http("server", "https://example.com/mcp").oauth(McpOAuthConfig {
            client_secret: Some("debug-secret".into()),
            ..Default::default()
        })
    );
    assert!(!debug.contains("debug-secret"), "{debug}");
    let debug = format!(
        "{:?}",
        McpOAuthTokens {
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            token_type: "Bearer".into(),
            expires_in: None,
            scope: None,
        }
    );
    assert!(!debug.contains("access-secret"), "{debug}");
    assert!(!debug.contains("refresh-secret"), "{debug}");

    for invalid in [
        r#"{"mcpServers":{"server":{"url":"https://example.com","oauth":{"callbackPort":0,"clientSecret":"do-not-echo"}}}}"#,
        r#"{"mcpServers":{"server":{"url":"http://example.com","oauth":{}}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.com","oauth":{"callbackUrl":"https://attacker.example/callback","clientSecret":"do-not-echo"}}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.com","oauth":{"clientRegistration":"cimd","clientId":"secret-client"}}}}"#,
        r#"{"mcpServers":{"server":{"url":"http://example.com","auth":{"provider":"secret-provider"}}}}"#,
        r#"{"mcpServers":{"server":{"command":"tool","oauth":{"clientSecret":"do-not-echo"}}}}"#,
    ] {
        let error = pi_plugin_mcp::config::parse_config(invalid).unwrap_err();
        assert!(!error.contains("do-not-echo"), "{error}");
        assert!(!error.contains("secret-client"), "{error}");
        assert!(!error.contains("secret-provider"), "{error}");
    }
}

#[test]
fn project_files_cannot_choose_a_provider_credential() {
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let project = directory.path().join("project");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(project.join(".pi")).unwrap();
    std::fs::write(
        agent.join("mcp.json"),
        r#"{"mcpServers":{"global":{"url":"https://example.com","auth":{"provider":"anthropic"}}}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".pi/mcp.json"),
        r#"{"mcpServers":{"project":{"url":"https://example.com","auth":{"provider":"openai"}}}}"#,
    )
    .unwrap();
    let library = McpLibrary::new(&agent, Some(&project), true);
    let project_doc = library.read(pi_plugin_mcp::McpScope::Project).unwrap();
    let diagnostic = project_doc.diagnostic.unwrap();
    assert!(diagnostic.contains("auth is only allowed"), "{diagnostic}");
    assert!(!diagnostic.contains("openai"), "{diagnostic}");
}

#[test]
fn credential_identity_includes_server_name_and_canonical_url() {
    let directory = tempfile::tempdir().unwrap();
    let store = McpOAuthCredentialStore::new(directory.path().join("mcp-auth.json"));
    store
        .save_tokens(
            "account-a",
            "https://example.com/mcp",
            "registered-client",
            McpOAuthTokens {
                access_token: "token-a".into(),
                token_type: "Bearer".into(),
                refresh_token: None,
                expires_in: None,
                scope: None,
            },
        )
        .unwrap();
    assert!(
        store
            .has_credentials("account-a", "https://example.com/mcp")
            .unwrap()
    );
    assert!(
        !store
            .has_credentials("account-b", "https://example.com/mcp")
            .unwrap()
    );
    assert!(
        !store
            .has_credentials("account-a", "https://example.com/other")
            .unwrap()
    );
    assert!(
        store
            .sign_out("account-a", "https://example.com/mcp")
            .unwrap()
    );
    assert!(
        !store
            .has_credentials("account-a", "https://example.com/mcp")
            .unwrap()
    );
    let text = std::fs::read_to_string(directory.path().join("mcp-auth.json")).unwrap();
    assert!(!text.contains("token-a"));
}

#[tokio::test]
async fn oauth_start_observes_host_cancellation_before_network_io() {
    let (abort, signal) = AbortHandle::new();
    abort.abort();
    let result = McpOAuthSignIn::start(
        "server",
        "https://example.com/mcp",
        &McpOAuthConfig {
            auth_server_metadata_url: Some("https://example.com/metadata".into()),
            ..Default::default()
        },
        McpOAuthCredentialStore::memory(),
        "http://127.0.0.1:8910/callback",
        None,
        signal,
    )
    .await;
    assert!(matches!(result, Err(McpOAuthError::Cancelled)));
}

#[tokio::test]
async fn preregistered_oauth_uses_explicit_metadata_and_accepts_a_pasted_redirect() {
    let token_requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let metadata = json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/authorize"),
        "token_endpoint": format!("{origin}/token"),
        "registration_endpoint": format!("{origin}/register"),
        "scopes_supported": ["read", "offline_access"],
        "code_challenge_methods_supported": ["S256"]
    });
    let app = axum::Router::new()
        .route(
            "/metadata",
            axum::routing::get({
                let metadata = metadata.clone();
                move || async move { axum::Json(metadata.clone()) }
            }),
        )
        .route(
            "/token",
            axum::routing::post({
                let token_requests = token_requests.clone();
                move |body: String| {
                    let token_requests = token_requests.clone();
                    async move {
                        token_requests.lock().unwrap().push(body);
                        axum::Json(json!({
                            "access_token": "fixture-access",
                            "refresh_token": "fixture-refresh",
                            "token_type": "Bearer",
                            "expires_in": 3600,
                            "scope": "read"
                        }))
                    }
                }
            }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let store = McpOAuthCredentialStore::new(directory.path().join("mcp-auth.json"));
    let settings = McpOAuthConfig {
        client_id: Some("registered-client".into()),
        client_secret: Some("fixture-secret".into()),
        callback_port: Some(8910),
        callback_url: Some("http://127.0.0.1:8910/callback".into()),
        scope: Some("read".into()),
        client_name: None,
        client_registration: None,
        auth_server_metadata_url: Some(format!("{origin}/metadata")),
    };
    let (_abort, signal) = AbortHandle::new();
    let (mut sign_in, redirect) = McpOAuthSignIn::start(
        "account",
        &format!("{origin}/mcp"),
        &settings,
        store.clone(),
        "http://127.0.0.1:8910/callback",
        None,
        signal,
    )
    .await
    .unwrap();
    let authorization = url::Url::parse(&redirect.authorization_url).unwrap();
    assert_eq!(authorization.path(), "/authorize");
    assert_eq!(
        authorization
            .query_pairs()
            .find_map(|(key, value)| (key == "client_id").then(|| value.into_owned())),
        Some("registered-client".into())
    );
    let (_abort, signal) = AbortHandle::new();
    sign_in
        .complete_redirect_url(
            &format!(
                "{}?code=fixture-code&state={}",
                redirect.redirect_url, redirect.state
            ),
            signal,
        )
        .await
        .unwrap();
    assert!(
        store
            .has_credentials("account", &format!("{origin}/mcp"))
            .unwrap()
    );
    let (_abort, signal) = AbortHandle::new();
    McpOAuthSignIn::refresh(
        "account",
        &format!("{origin}/mcp"),
        &settings,
        store,
        signal,
    )
    .await
    .unwrap();
    let token_requests = token_requests.lock().unwrap();
    assert_eq!(token_requests.len(), 2);
    assert!(token_requests[0].contains("code=fixture-code"));
    assert!(token_requests[0].contains("redirect_uri="));
    assert!(token_requests[1].contains("grant_type=refresh_token"));
    drop(token_requests);
    task.abort();
}

#[tokio::test]
async fn cimd_is_preferred_and_dcr_remains_available() {
    let registrations = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let metadata = json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/authorize"),
        "token_endpoint": format!("{origin}/token"),
        "registration_endpoint": format!("{origin}/register"),
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
        "token_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"]
    });
    let app = axum::Router::new()
        .route(
            "/metadata",
            axum::routing::get({
                let metadata = metadata.clone();
                move || async move { axum::Json(metadata.clone()) }
            }),
        )
        .route(
            "/register",
            axum::routing::post({
                let registrations = registrations.clone();
                move |body: String| {
                    let registrations = registrations.clone();
                    async move {
                        registrations.lock().unwrap().push(body);
                        (
                            axum::http::StatusCode::CREATED,
                            axum::Json(json!({
                                "client_id": "dcr-client",
                                "redirect_uris": ["http://127.0.0.1:8911/callback"],
                                "token_endpoint_auth_method": "none"
                            })),
                        )
                    }
                }
            }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = McpOAuthConfig {
        auth_server_metadata_url: Some(format!("{origin}/metadata")),
        ..Default::default()
    };
    for (registration, expected_client, expected_registrations) in [
        (
            McpOAuthClientRegistration::Cimd,
            "https://pi.dev/oauth/client.json",
            0,
        ),
        (McpOAuthClientRegistration::Dcr, "dcr-client", 1),
    ] {
        let settings = McpOAuthConfig {
            client_registration: Some(registration),
            ..base.clone()
        };
        let (_abort, signal) = AbortHandle::new();
        let (_sign_in, redirect) = McpOAuthSignIn::start(
            "server",
            &format!("{origin}/mcp"),
            &settings,
            McpOAuthCredentialStore::memory(),
            "http://127.0.0.1:8911/callback",
            None,
            signal,
        )
        .await
        .unwrap();
        let authorization = url::Url::parse(&redirect.authorization_url).unwrap();
        assert_eq!(
            authorization
                .query_pairs()
                .find_map(|(key, value)| (key == "client_id").then(|| value.into_owned())),
            Some(expected_client.into())
        );
        assert_eq!(registrations.lock().unwrap().len(), expected_registrations);
    }
    task.abort();
}

#[derive(Default)]
struct RotatingResolver {
    calls: Mutex<usize>,
}

#[async_trait]
impl McpProviderTokenResolver for RotatingResolver {
    async fn token(&self, provider: &str) -> Result<Option<String>, String> {
        assert_eq!(provider, "fixture");
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        Ok(Some(format!("token-{calls}")))
    }
}

#[tokio::test]
async fn provider_auth_is_resolved_for_each_http_request_and_static_headers_survive() {
    let authorizations = Arc::new(Mutex::new(Vec::new()));
    let static_headers = Arc::new(Mutex::new(Vec::new()));
    let get_observed = Arc::new(tokio::sync::Notify::new());
    let get_seen = get_observed.clone();
    let get_authorizations = authorizations.clone();
    let get_static_headers = static_headers.clone();
    let route = axum::routing::post({
        let authorizations = authorizations.clone();
        let static_headers = static_headers.clone();
        move |headers: axum::http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let authorizations = authorizations.clone();
            let static_headers = static_headers.clone();
            async move {
                authorizations.lock().unwrap().push(
                    headers
                        .get("authorization")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string(),
                );
                static_headers.lock().unwrap().push(
                    headers
                        .get("x-static")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string(),
                );
                let id = &request["id"];
                if id.is_null() {
                    return axum::http::StatusCode::ACCEPTED.into_response();
                }
                let result = match request["method"].as_str().unwrap() {
                    "server/discover" => {
                        return axum::Json(json!({
                            "jsonrpc":"2.0", "id":id,
                            "error":{"code":-32601,"message":"initialize"}
                        }))
                        .into_response();
                    }
                    "initialize" => json!({
                        "protocolVersion":"2025-03-26",
                        "capabilities":{"tools":{}},
                        "serverInfo":{"name":"auth-fixture","version":"1"}
                    }),
                    "tools/list" => json!({"tools":[]}),
                    method => panic!("unexpected method {method}"),
                };
                axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
            }
        }
    });
    // Frozen legacy opens the common GET after initialized even without a session ID.
    // Count it as a real authenticated request rather than comparing POSTs alone.
    let route = route.get(move |headers: axum::http::HeaderMap| {
        let authorizations = get_authorizations.clone();
        let static_headers = get_static_headers.clone();
        let observed = get_seen.clone();
        async move {
            authorizations
                .lock()
                .unwrap()
                .push(headers["authorization"].to_str().unwrap().to_owned());
            static_headers
                .lock()
                .unwrap()
                .push(headers["x-static"].to_str().unwrap().to_owned());
            observed.notify_one();
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    });

    let resolver = Arc::new(RotatingResolver::default());
    let config = McpServerConfig::http("remote", url)
        .headers(BTreeMap::from([("X-Static".into(), "kept".into())]))
        .provider_auth("fixture");
    let pool = McpToolSet::connect_with_auth(
        vec![config],
        McpClientAuth::default().with_provider_tokens(resolver.clone()),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), get_observed.notified())
        .await
        .expect("stateless common GET must be dispatched");
    pool.shutdown().await.unwrap();
    task.abort();

    let seen = authorizations.lock().unwrap();
    assert_eq!(seen.len(), 5, "four setup POSTs plus one common GET");
    assert!(seen.len() >= 3, "expected handshake requests, saw {seen:?}");
    assert!(seen.iter().all(|value| value.starts_with("Bearer token-")));
    assert!(seen.windows(2).all(|values| values[0] != values[1]));
    assert!(
        static_headers
            .lock()
            .unwrap()
            .iter()
            .all(|value| value == "kept")
    );
    assert_eq!(*resolver.calls.lock().unwrap(), seen.len());
}
