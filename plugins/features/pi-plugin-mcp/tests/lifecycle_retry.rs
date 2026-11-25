//! Local product-path evidence for the pinned Pi HTTP connection retry contract.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::IntoResponse;
use pi_plugin_mcp::{
    McpClientAuth, McpOAuthCredentialStore, McpOAuthTokens, McpProviderTokenResolver,
    McpServerConfig, McpToolSet, McpTransport,
};
use serde_json::{Value, json};
use tokio::time::Instant;

#[derive(Default)]
struct State {
    failures: BTreeMap<&'static str, VecDeque<http::StatusCode>>,
    requests: Vec<(String, Instant)>,
    authorization: Vec<Option<String>>,
}

struct Fixture {
    config: McpServerConfig,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(method: &'static str, statuses: &[u16]) -> Self {
        let state = Arc::new(Mutex::new(State {
            failures: BTreeMap::from([(
                method,
                statuses
                    .iter()
                    .map(|code| http::StatusCode::from_u16(*code).unwrap())
                    .collect(),
            )]),
            ..Default::default()
        }));
        let handler_state = state.clone();
        let router = axum::Router::new().route("/mcp", axum::routing::post(move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let state = handler_state.clone();
            async move {
                let method = request["method"].as_str().unwrap();
                let failure = {
                    let mut state = state.lock().unwrap();
                    state.requests.push((method.to_owned(), Instant::now()));
                    state.authorization.push(headers.get(http::header::AUTHORIZATION).map(|value| value.to_str().unwrap().to_owned()));
                    state.failures.get_mut(method).and_then(VecDeque::pop_front)
                };
                if let Some(status) = failure {
                    return status.into_response();
                }
                let id = &request["id"];
                if id.is_null() {
                    return http::StatusCode::ACCEPTED.into_response();
                }
                let result = match method {
                    "server/discover" => return axum::Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"legacy fixture"}})).into_response(),
                    "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"retry-fixture","version":"1"}}),
                    "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                    _ => panic!("unexpected method {method}"),
                };
                axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http(
            "retry",
            format!("http://{}/mcp", listener.local_addr().unwrap()),
        )
        .headers(BTreeMap::from([(
            "Authorization".into(),
            "Bearer local-fixture-only".into(),
        )]));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            config,
            state,
            task,
        }
    }

    fn calls(&self, method: &str) -> Vec<Instant> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter_map(|(name, time)| (name == method).then_some(*time))
            .collect()
    }
}

#[tokio::test]
async fn transient_initialize_retries_after_250_and_1000_ms() {
    let fixture = Fixture::new("initialize", &[503, 429]).await;
    let pool = McpToolSet::connect(vec![fixture.config.clone()])
        .await
        .unwrap();
    let calls = fixture.calls("initialize");
    assert_eq!(calls.len(), 3);
    assert!(calls[1].duration_since(calls[0]) >= Duration::from_millis(250));
    assert!(calls[2].duration_since(calls[1]) >= Duration::from_millis(1000));
    assert_eq!(pool.tools()[0].name, "mcp__retry__echo");
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn exhausted_initialize_retry_budget_stops_after_three_attempts() {
    let fixture = Fixture::new("initialize", &[500, 500, 500]).await;
    assert!(
        McpToolSet::connect(vec![fixture.config.clone()])
            .await
            .is_err()
    );
    assert_eq!(fixture.calls("initialize").len(), 3);
}

#[tokio::test]
async fn transient_tool_discovery_retries_the_complete_connection() {
    let fixture = Fixture::new("tools/list", &[408]).await;
    let pool = McpToolSet::connect(vec![fixture.config.clone()])
        .await
        .unwrap();
    let calls = fixture.calls("tools/list");
    assert_eq!(calls.len(), 2);
    assert_eq!(fixture.calls("initialize").len(), 2);
    assert!(calls[1].duration_since(calls[0]) >= Duration::from_millis(250));
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn permanent_and_auth_failures_do_not_retry_initialization() {
    for status in [400, 401, 403, 404, 501] {
        let fixture = Fixture::new("initialize", &[status]).await;
        assert!(
            McpToolSet::connect(vec![fixture.config.clone()])
                .await
                .is_err(),
            "status {status}"
        );
        assert_eq!(fixture.calls("initialize").len(), 1, "status {status}");
    }
}

#[tokio::test]
async fn cancelling_connection_during_backoff_prevents_later_attempts() {
    let fixture = Fixture::new("initialize", &[503, 503, 503]).await;
    let config = fixture.config.clone();
    let connect = tokio::spawn(async move { McpToolSet::connect(vec![config]).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.calls("initialize").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    connect.abort();
    assert!(connect.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(fixture.calls("initialize").len(), 1);
}

struct FixtureProvider;

#[async_trait::async_trait]
impl McpProviderTokenResolver for FixtureProvider {
    async fn token(&self, _provider: &str) -> Result<Option<String>, String> {
        Ok(Some("provider-fixture-token".into()))
    }
}

#[tokio::test]
async fn anonymous_provider_and_oauth_paths_share_the_typed_retry_contract() {
    for mode in ["anonymous", "provider", "oauth"] {
        let fixture = Fixture::new("initialize", &[503]).await;
        let mut config = fixture.config.clone().headers(BTreeMap::new());
        let mut auth = McpClientAuth::default();
        let expected = match mode {
            "provider" => {
                config = config.provider_auth("fixture");
                auth = auth.with_provider_tokens(Arc::new(FixtureProvider));
                Some("Bearer provider-fixture-token")
            }
            "oauth" => {
                let McpTransport::Http { url, .. } = &config.transport else {
                    unreachable!()
                };
                let store = McpOAuthCredentialStore::memory();
                store
                    .save_tokens(
                        "retry",
                        url,
                        "fixture-client",
                        McpOAuthTokens {
                            access_token: "oauth-fixture-token".into(),
                            token_type: "Bearer".into(),
                            refresh_token: None,
                            expires_in: Some(3600),
                            scope: None,
                        },
                    )
                    .unwrap();
                auth = auth.with_oauth_store(store);
                Some("Bearer oauth-fixture-token")
            }
            _ => None,
        };
        let pool = McpToolSet::connect_with_auth(vec![config], auth)
            .await
            .unwrap();
        assert_eq!(fixture.calls("initialize").len(), 2, "{mode}");
        assert!(
            fixture
                .state
                .lock()
                .unwrap()
                .authorization
                .iter()
                .all(|header| header.as_deref() == expected),
            "{mode}"
        );
        pool.shutdown().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_stdio_startup_is_not_retried_as_a_handshake_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let counter = directory.path().join("starts");
    let config = McpServerConfig::stdio("stdio", "python3").args([
        "-c",
        "import sys; open(sys.argv[1], 'a').write('start\\n')",
        counter.to_str().unwrap(),
    ]);
    assert!(McpToolSet::connect(vec![config]).await.is_err());
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "start\n");
}
