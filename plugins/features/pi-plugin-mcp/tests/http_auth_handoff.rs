use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use pi_core::{AbortHandle, ContentBlock, TextContent, ToolResult};
use pi_plugin::{FrozenRegistries, RegistriesBuilder, ToolContext, ToolError, ToolUpdateSink};
use pi_plugin_mcp::{
    McpClientAuth, McpOAuthConfig, McpOAuthCredentialStore, McpOAuthTokens, McpServerConfig,
    McpToolSet,
};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Call {
    request: Value,
    authorization: String,
    session: String,
}

#[derive(Default)]
struct Trace {
    initializes: usize,
    lists: usize,
    calls: Vec<Call>,
    executions: usize,
    resource_reads: usize,
    refresh_forms: Vec<BTreeMap<String, String>>,
}

struct StateData {
    replay_ok: bool,
    trace: Mutex<Trace>,
}

struct Fixture {
    url: String,
    metadata_url: String,
    store: McpOAuthCredentialStore,
    state: Arc<StateData>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(replay_ok: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let metadata_url = format!("{origin}/metadata");
        let metadata = json!({
            "issuer": origin,
            "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "token_endpoint_auth_methods_supported": ["client_secret_post"]
        });
        let state = Arc::new(StateData {
            replay_ok,
            trace: Mutex::new(Trace::default()),
        });
        let router = Router::new()
            .route(
                "/metadata",
                axum::routing::get(move || {
                    let metadata = metadata.clone();
                    async move { Json(metadata) }
                }),
            )
            .route("/token", post(refresh))
            .route(
                "/mcp",
                post(mcp).delete(|| async { StatusCode::NO_CONTENT }),
            )
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let store = McpOAuthCredentialStore::memory();
        store
            .save_tokens(
                "explicit",
                &url,
                "fixture-client",
                McpOAuthTokens {
                    access_token: "unchanged-access".into(),
                    token_type: "Bearer".into(),
                    refresh_token: Some("fixture-refresh".into()),
                    expires_in: Some(3600),
                    scope: None,
                },
            )
            .unwrap();
        Self { url, metadata_url, store, state, task }
    }

    async fn connect(&self) -> McpToolSet {
        let config = McpServerConfig::http("explicit", &self.url).oauth(McpOAuthConfig {
            client_id: Some("fixture-client".into()),
            client_secret: Some("fixture-secret".into()),
            auth_server_metadata_url: Some(self.metadata_url.clone()),
            ..Default::default()
        });
        tokio::time::timeout(
            TIMEOUT,
            McpToolSet::connect_with_auth(
                vec![config],
                McpClientAuth::default().with_oauth_store(self.store.clone()),
            ),
        )
        .await
        .expect("explicit connection timed out")
        .expect("explicit OAuth connection failed")
    }
}

async fn refresh(
    State(state): State<Arc<StateData>>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Json<Value> {
    state.trace.lock().unwrap().refresh_forms.push(form);
    // A successful refresh can keep the same access token. It still permits one replay.
    Json(json!({
        "access_token": "unchanged-access", "token_type": "Bearer",
        "refresh_token": "rotated-refresh", "expires_in": 3600
    }))
}

fn response(request: &Value, result: Value, status: StatusCode) -> Response {
    (status, Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":result})))
        .into_response()
}

async fn mcp(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if request["id"].is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let mut trace = state.trace.lock().unwrap();
    match request["method"].as_str().unwrap() {
        "server/discover" => Json(json!({
            "jsonrpc":"2.0", "id":request["id"],
            "error":{"code":-32601,"message":"Use initialize"}
        })).into_response(),
        "initialize" => {
            trace.initializes += 1;
            let mut response = response(&request, json!({
                "protocolVersion":"2025-03-26",
                "capabilities":{"tools":{},"resources":{}},
                "serverInfo":{"name":"explicit-fixture","version":"1"}
            }), StatusCode::OK);
            response.headers_mut().insert("mcp-session-id", HeaderValue::from_static("explicit-session"));
            response
        }
        "tools/list" => {
            trace.lists += 1;
            response(&request, json!({"tools":[{
                "name":"echo", "description":"Frozen explicit callable",
                "inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}
            }]}), StatusCode::OK)
        }
        "tools/call" => {
            trace.calls.push(Call {
                request: request.clone(),
                authorization: headers["authorization"].to_str().unwrap().to_string(),
                session: headers["mcp-session-id"].to_str().unwrap().to_string(),
            });
            if trace.calls.len() == 1 || !state.replay_ok {
                // Status must win over a syntactically successful RPC body, without a challenge.
                return response(&request, json!({"content":[{
                    "type":"text","text":"spoofed success on HTTP 401"
                }]}), StatusCode::UNAUTHORIZED);
            }
            trace.executions += 1;
            response(&request, json!({"content":[{"type":"text","text":"executed once"}]}), StatusCode::OK)
        }
        "resources/read" => {
            trace.resource_reads += 1;
            response(&request, json!({"contents":[{
                "uri":"fixture://doc","text":"spoofed success on HTTP 503"
            }]}), StatusCode::SERVICE_UNAVAILABLE)
        }
        method => panic!("unexpected method {method}"),
    }
}

async fn execute(
    registry: &FrozenRegistries,
    name: &str,
    arguments: Value,
) -> Result<ToolResult, ToolError> {
    let (updates, _) = ToolUpdateSink::channel();
    tokio::time::timeout(
        TIMEOUT,
        registry.tool(name).unwrap().execute(
            ToolContext::standalone(std::env::temp_dir(), AbortHandle::new().1),
            "explicit-handoff".into(),
            arguments,
            updates,
        ),
    )
    .await
    .expect("explicit operation timed out")
}

#[tokio::test]
async fn explicit_oauth_replays_an_unchanged_token_once_without_supervised_resource_recovery() {
    // Separate scenarios, never a retry of a failed test or logical invocation.
    for replay_ok in [true, false] {
        let fixture = Fixture::new(replay_ok).await;
        let pool = fixture.connect().await;
        let frozen = pool.tools();
        let (_, registry) = RegistriesBuilder::new()
            .register_plugins(vec![pool.plugin()])
            .unwrap();
        assert!(registry.command_specs().is_empty());
        let result = execute(&registry, "mcp__explicit__echo", json!({"value":"original"})).await;
        if replay_ok {
            assert_eq!(
                result.unwrap().content,
                vec![ContentBlock::Text(TextContent::new("executed once"))]
            );
            // Explicit mode retains SDK session-expiry handling, not supervised resource retry.
            // Use 503, not session-attached 404, to distinguish that ownership boundary.
            assert!(matches!(
                execute(&registry, "read_mcp_resource", json!({"server":"explicit","uri":"fixture://doc"})).await,
                Err(ToolError::Execution(_))
            ));
        } else {
            assert!(matches!(result, Err(ToolError::Execution(_))));
        }
        assert_eq!(pool.tools(), frozen);
        pool.shutdown().await.unwrap();
        {
            let trace = fixture.state.trace.lock().unwrap();
            assert_eq!((trace.initializes, trace.lists), (1, 1));
            assert_eq!(trace.calls.len(), 2);
            assert_eq!(trace.calls[0].request, trace.calls[1].request);
            for call in &trace.calls {
                assert_eq!(call.authorization, "Bearer unchanged-access");
                assert_eq!(call.session, "explicit-session");
                assert_eq!(call.request["params"]["arguments"], json!({"value":"original"}));
            }
            assert_eq!(trace.executions, usize::from(replay_ok));
            assert_eq!(trace.resource_reads, usize::from(replay_ok));
            assert_eq!(trace.refresh_forms.len(), 1);
            let form = &trace.refresh_forms[0];
            assert_eq!(form["grant_type"], "refresh_token");
            assert_eq!(form["client_id"], "fixture-client");
            assert_eq!(form["client_secret"], "fixture-secret");
            assert_eq!(form["refresh_token"], "fixture-refresh");
        }
    }
}
