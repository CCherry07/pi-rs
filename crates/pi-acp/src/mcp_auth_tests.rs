use super::*;
use std::sync::Mutex as StdMutex;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use pi_core::{AbortHandle, TextContent, ToolResult};
use pi_plugin::{FrozenRegistries, RegistriesBuilder, ToolContext, ToolError, ToolUpdateSink};
use pi_plugin_mcp::McpOAuthTokens;
use serde_json::{Value, json};

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

const HTTP_HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum OAuthReply {
    ReplaySucceeds,
    ReplayRejected,
    ScopeOnInitialize,
    StaticOnInitialize(StatusCode),
}

#[derive(Default)]
struct OAuthTrace {
    initializes: usize,
    initialize_authorizations: Vec<String>,
    auth_http_requests: usize,
    lists: usize,
    calls: Vec<(Value, String, String)>,
    executions: usize,
    resource_reads: usize,
    refresh_forms: Vec<BTreeMap<String, String>>,
}

struct OAuthState {
    reply: OAuthReply,
    metadata: Value,
    resource_metadata: Value,
    trace: StdMutex<OAuthTrace>,
}

struct OAuthFixture {
    directory: tempfile::TempDir,
    url: String,
    store: McpOAuthCredentialStore,
    state: Arc<OAuthState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for OAuthFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OAuthFixture {
    async fn new(reply: OAuthReply) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let state = Arc::new(OAuthState {
            reply,
            metadata: json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "token_endpoint_auth_methods_supported": ["none"]
            }),
            resource_metadata: json!({"resource":url,"authorization_servers":[origin]}),
            trace: StdMutex::new(OAuthTrace::default()),
        });
        // ACP supplies no OAuth settings. Restore the host's public-client credentials
        // and discover metadata through normal loopback endpoints, not a test-only override.
        let app = axum::Router::new()
            .route("/.well-known/oauth-authorization-server", axum::routing::get(oauth_metadata))
            .route("/.well-known/oauth-authorization-server/mcp", axum::routing::get(oauth_metadata))
            .route("/.well-known/oauth-protected-resource", axum::routing::get(resource_metadata))
            .route("/.well-known/oauth-protected-resource/mcp", axum::routing::get(resource_metadata))
            .route("/token", axum::routing::post(oauth_refresh))
            .route("/mcp", axum::routing::post(oauth_mcp).delete(|| async { StatusCode::NO_CONTENT }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                record_auth_http_requests,
            ))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let store = McpOAuthCredentialStore::new(directory.path().join("host-mcp-auth.json"));
        store.save_tokens("account", &url, "host-public-client", McpOAuthTokens {
            access_token: "unchanged-access".into(),
            token_type: "Bearer".into(),
            refresh_token: Some("host-refresh".into()),
            expires_in: Some(3600),
            scope: None,
        }).unwrap();
        Self { directory, url, store, state, task }
    }

    fn servers(&self) -> Vec<acp::McpServer> {
        vec![acp::McpServer::Http(acp::McpServerHttp::new("account", &self.url))]
    }
}

async fn record_auth_http_requests(
    State(state): State<Arc<OAuthState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // Include unknown discovery routes and GET /mcp, not only successful metadata handlers.
    // Rejected initialization cannot have opened the normal MCP GET stream yet.
    if request.method() == axum::http::Method::GET || request.uri().path() != "/mcp" {
        state.trace.lock().unwrap().auth_http_requests += 1;
    }
    next.run(request).await
}

async fn oauth_metadata(State(state): State<Arc<OAuthState>>) -> Json<Value> {
    Json(state.metadata.clone())
}

async fn resource_metadata(State(state): State<Arc<OAuthState>>) -> Json<Value> {
    Json(state.resource_metadata.clone())
}

async fn oauth_refresh(
    State(state): State<Arc<OAuthState>>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Json<Value> {
    state.trace.lock().unwrap().refresh_forms.push(form);
    Json(json!({
        "access_token":"unchanged-access", "token_type":"Bearer",
        "refresh_token":"rotated-refresh", "expires_in":3600
    }))
}

fn oauth_response(request: &Value, result: Value, status: StatusCode) -> Response {
    (status, Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})))
        .into_response()
}

async fn oauth_mcp(
    State(state): State<Arc<OAuthState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if request["id"].is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let mut trace = state.trace.lock().unwrap();
    match request["method"].as_str().unwrap() {
        "server/discover" => Json(json!({
            "jsonrpc":"2.0","id":request["id"],
            "error":{"code":-32601,"message":"Use initialize"}
        })).into_response(),
        "initialize" => {
            trace.initializes += 1;
            trace.initialize_authorizations.push(
                headers["authorization"].to_str().unwrap().to_string(),
            );
            let result = json!({
                "protocolVersion":"2025-03-26", "capabilities":{"tools":{},"resources":{}},
                "serverInfo":{"name":"acp-fixture","version":"1"}
            });
            let rejection = match state.reply {
                OAuthReply::ScopeOnInitialize => Some(StatusCode::FORBIDDEN),
                OAuthReply::StaticOnInitialize(status) => Some(status),
                _ => None,
            };
            if let Some(status) = rejection {
                let mut response = oauth_response(&request, result, status);
                if status == StatusCode::FORBIDDEN {
                    response.headers_mut().insert("www-authenticate", HeaderValue::from_static(
                        "Bearer error=\"insufficient_scope\", scope=\"private-step-up\""
                    ));
                }
                return response;
            }
            let mut response = oauth_response(&request, result, StatusCode::OK);
            response.headers_mut().insert("mcp-session-id", HeaderValue::from_static("acp-session"));
            response
        }
        "tools/list" => {
            trace.lists += 1;
            oauth_response(&request, json!({"tools":[{
                "name":"echo", "description":"Frozen ACP callable",
                "inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}
            }]}), StatusCode::OK)
        }
        "tools/call" => {
            trace.calls.push((
                request.clone(),
                headers["authorization"].to_str().unwrap().to_string(),
                headers["mcp-session-id"].to_str().unwrap().to_string(),
            ));
            if trace.calls.len() == 1 || matches!(state.reply, OAuthReply::ReplayRejected) {
                // A valid RPC body on headerless 401 cannot be accepted as a tool result.
                return oauth_response(&request, json!({"content":[{
                    "type":"text","text":"spoofed HTTP 401 success"
                }]}), StatusCode::UNAUTHORIZED);
            }
            trace.executions += 1;
            oauth_response(&request, json!({"content":[{"type":"text","text":"executed once"}]}), StatusCode::OK)
        }
        "resources/read" => {
            trace.resource_reads += 1;
            oauth_response(&request, json!({"contents":[{
                "uri":"fixture://doc","text":"spoofed HTTP 503 success"
            }]}), StatusCode::SERVICE_UNAVAILABLE)
        }
        method => panic!("unexpected method {method}"),
    }
}

async fn execute_oauth_tool(
    registry: &FrozenRegistries,
    cwd: &Path,
    name: &str,
    arguments: Value,
) -> Result<ToolResult, ToolError> {
    let (updates, _) = ToolUpdateSink::channel();
    tokio::time::timeout(
        HTTP_HANDOFF_TIMEOUT,
        registry.tool(name).unwrap().execute(
            ToolContext::standalone(cwd.to_path_buf(), AbortHandle::new().1),
            "acp-handoff".into(),
            arguments,
            updates,
        ),
    ).await.expect("ACP tool request timed out")
}

#[tokio::test]
async fn acp_host_store_refreshes_once_without_supervised_resource_recovery() {
    for reply in [OAuthReply::ReplaySucceeds, OAuthReply::ReplayRejected] {
        let fixture = OAuthFixture::new(reply).await;
        let (_, pool) = tokio::time::timeout(
            HTTP_HANDOFF_TIMEOUT,
            generation_overlay(fixture.servers(), fixture.directory.path(), Some(&fixture.store)),
        ).await.expect("ACP connection timed out").unwrap();
        let pool = pool.unwrap();
        let frozen = pool.tools();
        let (_, registry) = RegistriesBuilder::new()
            .register_plugins(vec![pool.plugin()]).unwrap();
        assert!(registry.command_specs().is_empty());
        let result = execute_oauth_tool(
            &registry, fixture.directory.path(), "mcp__account__echo", json!({"value":"original"}),
        ).await;
        let succeeded = matches!(reply, OAuthReply::ReplaySucceeds);
        if succeeded {
            assert_eq!(result.unwrap().content, vec![PiContentBlock::Text(TextContent::new("executed once"))]);
            // A 503 must not acquire Coding's supervised resource replay. Do not prohibit
            // stock SDK session-attached 404 recovery, which is a separate retained behavior.
            assert!(matches!(execute_oauth_tool(
                &registry, fixture.directory.path(), "read_mcp_resource",
                json!({"server":"account","uri":"fixture://doc"}),
            ).await, Err(ToolError::Execution(_))));
        } else {
            assert!(matches!(result, Err(ToolError::Execution(_))));
        }
        assert_eq!(pool.tools(), frozen);
        pool.shutdown().await.unwrap();
        {
            let trace = fixture.state.trace.lock().unwrap();
            assert_eq!((trace.initializes, trace.lists), (1, 1));
            assert_eq!(trace.calls.len(), 2);
            assert_eq!(trace.calls[0].0, trace.calls[1].0);
            for (request, authorization, session) in &trace.calls {
                assert_eq!(authorization, "Bearer unchanged-access");
                assert_eq!(session, "acp-session");
                assert_eq!(request["params"]["arguments"], json!({"value":"original"}));
            }
            assert_eq!(trace.executions, usize::from(succeeded));
            assert_eq!(trace.resource_reads, usize::from(succeeded));
            assert_eq!(trace.refresh_forms.len(), 1);
            let form = &trace.refresh_forms[0];
            assert_eq!(form["grant_type"], "refresh_token");
            assert_eq!(form["client_id"], "host-public-client");
            assert_eq!(form["refresh_token"], "host-refresh");
            assert!(!form.contains_key("client_secret"));
        }
    }
}

#[tokio::test]
async fn acp_scope_403_projects_auth_required_before_rpc_without_refresh() {
    let fixture = OAuthFixture::new(OAuthReply::ScopeOnInitialize).await;
    let error = tokio::time::timeout(
        HTTP_HANDOFF_TIMEOUT,
        generation_overlay(fixture.servers(), fixture.directory.path(), Some(&fixture.store)),
    ).await.expect("ACP scope challenge timed out").expect_err("scope challenge must reject setup");
    assert_eq!(error.code, acp::ErrorCode::AuthRequired);
    let wire = serde_json::to_string(&error).unwrap();
    assert!(wire.contains("pi mcp login account"));
    for secret in ["private-step-up", "unchanged-access", "host-refresh"] {
        assert!(!wire.contains(secret), "ACP error disclosed fixture auth state");
    }
    let trace = fixture.state.trace.lock().unwrap();
    assert_eq!(trace.initializes, 1);
    assert_eq!(trace.lists, 0);
    assert!(trace.calls.is_empty());
    assert!(trace.refresh_forms.is_empty());
}

#[tokio::test]
async fn acp_static_authorization_rejections_bypass_stored_oauth_and_remain_ordinary_errors() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        let fixture = OAuthFixture::new(OAuthReply::StaticOnInitialize(status)).await;
        assert!(fixture.store.has_credentials("account", &fixture.url).unwrap());
        let credentials_path = fixture.directory.path().join("host-mcp-auth.json");
        let credentials_before = std::fs::read(&credentials_path).unwrap();
        let servers = vec![acp::McpServer::Http(
            acp::McpServerHttp::new("account", &fixture.url).headers(vec![
                acp::HttpHeader::new("Authorization", "Bearer static-private"),
            ]),
        )];
        // ACP exposes HTTP headers plus an optional host store, not provider/OAuth settings.
        // The stored OAuth grant must not attach a coordinator behind this static header.
        let error = tokio::time::timeout(
            HTTP_HANDOFF_TIMEOUT,
            generation_overlay(servers, fixture.directory.path(), Some(&fixture.store)),
        )
        .await
        .expect("ACP static rejection timed out")
        .expect_err("static rejection must fail setup");
        assert_eq!(error.code, acp::ErrorCode::InternalError);
        assert_ne!(error.code, acp::ErrorCode::AuthRequired);
        let wire = serde_json::to_string(&error).unwrap();
        for secret in [
            "static-private",
            "private-step-up",
            "unchanged-access",
            "host-refresh",
            fixture.url.as_str(),
        ] {
            assert!(!wire.contains(secret), "ACP error disclosed fixture auth state");
        }
        assert_eq!(std::fs::read(credentials_path).unwrap(), credentials_before);
        let trace = fixture.state.trace.lock().unwrap();
        assert_eq!(trace.initializes, 1);
        assert_eq!(trace.initialize_authorizations, ["Bearer static-private"]);
        assert_eq!(trace.auth_http_requests, 0, "unexpected OAuth discovery or refresh");
        assert!(trace.refresh_forms.is_empty());
        assert_eq!(trace.lists, 0);
        assert!(trace.calls.is_empty());
        assert_eq!(trace.executions, 0);
        assert_eq!(trace.resource_reads, 0);
    }
}
