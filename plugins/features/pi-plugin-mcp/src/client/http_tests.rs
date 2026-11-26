//! Loopback proof of private HTTP evidence reaching stock SDK setup and public diagnostics.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;

use axum::response::IntoResponse;
use rmcp::transport::streamable_http_client::StreamableHttpClient;
use serde_json::json;

use crate::http_client::{PiHttpClient, PiHttpClientError};
use crate::McpProviderTokenResolver;

const SCOPE_CHALLENGE: &str = "Bearer error=\"insufficient_scope\", scope=\"private-scope-marker\"";
const BODY_MARKER: &str = "private-http-body-marker";
const TOKEN: &str = "private-static-token-marker";

type RecordedRequests = Arc<StdMutex<Vec<(String, Option<String>)>>>;

struct Fixture {
    config: McpServerConfig,
    requests: RecordedRequests,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(fail_method: &'static str, status: u16, scope: bool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let recorded = requests.clone();
        let router = axum::Router::new().route("/mcp", axum::routing::post(
            move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
                let recorded = recorded.clone();
                async move {
                    let method = request["method"].as_str().unwrap();
                    recorded.lock().unwrap().push((method.to_owned(),
                        headers.get("mcp-session-id").map(|value| value.to_str().unwrap().to_owned())));
                    let id = &request["id"];
                    if id.is_null() {
                        return http::StatusCode::ACCEPTED.into_response();
                    }
                    if method == fail_method {
                        // Valid correlated RPC on an HTTP failure must not hide status evidence.
                        let mut response = (http::StatusCode::from_u16(status).unwrap(),
                            axum::Json(json!({"jsonrpc":"2.0","id":id,"error":{
                                "code":-32603,"message":BODY_MARKER
                            }}))).into_response();
                        if scope {
                            response.headers_mut().insert(http::header::WWW_AUTHENTICATE,
                                http::HeaderValue::from_static(SCOPE_CHALLENGE));
                        }
                        return response;
                    }
                    let result = match method {
                        "server/discover" => return axum::Json(json!({"jsonrpc":"2.0","id":id,
                            "error":{"code":-32601,"message":"legacy fixture"}})).into_response(),
                        "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},
                            "serverInfo":{"name":"fixture","version":"1"}}),
                        "tools/list" => json!({"tools":[]}),
                        _ => panic!("unexpected method {method}"),
                    };
                    axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
                }
            }
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http("fixture", format!("http://{}/mcp", listener.local_addr().unwrap()))
            .headers(BTreeMap::from([("Authorization".into(), format!("Bearer {TOKEN}"))]));
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        Self { config, requests, task }
    }

    fn url(&self) -> &str {
        let McpTransport::Http { url, .. } = &self.config.transport else { unreachable!() };
        url
    }

    fn count(&self, method: &str) -> usize {
        self.requests.lock().unwrap().iter().filter(|(name, _)| name == method).count()
    }

    async fn rejection<C>(&self, client: C, headers: HashMap<http::HeaderName, http::HeaderValue>) -> ClientInitializeError
    where C: StreamableHttpClient<Error = PiHttpClientError> {
        let transport = StreamableHttpClientTransport::with_client(client,
            StreamableHttpClientTransportConfig::with_uri(self.url().to_owned())
                .custom_headers(headers).reinit_on_expired_session(false));
        tokio::time::timeout(Duration::from_secs(5),
            McpClientHandler::new(None).serve_with_lifecycle(transport, ClientLifecycleMode::Initialize))
            .await.expect("bounded HTTP handshake").err().expect("HTTP rejection, not RPC body")
    }

    async fn connect(&self, mode: ConnectionMode) -> Result<Arc<McpConnection>, McpError> {
        let options = McpConnectionOptions { mode, ..McpConnectionOptions::from(&McpServerPolicy::direct()) };
        tokio::time::timeout(Duration::from_secs(5),
            connect_inventory(self.config.clone(), options, McpClientAuth::default()))
            .await.expect("bounded setup").map(|(connection, _)| connection)
    }
}

#[tokio::test]
async fn unattached_discover_rejection_has_one_fresh_initialize_in_both_modes() {
    for mode in [ConnectionMode::Explicit, ConnectionMode::Supervised] {
        for status in [400, 404] {
            let fixture = Fixture::new("server/discover", status, false).await;
            let connection = fixture.connect(mode).await.unwrap();
            connection.close().await.unwrap();
            assert_eq!(fixture.count("server/discover"), 1, "{mode:?} {status}");
            assert_eq!(fixture.count("initialize"), 1, "{mode:?} {status}");
            assert_eq!(fixture.count("tools/list"), 1);
            assert!(fixture.requests.lock().unwrap().iter().all(|(_, session)| session.is_none()));
        }
    }
}

#[tokio::test]
async fn any_final_custom_session_blocks_discover_fallback_including_empty_and_case_variants() {
    for mode in [ConnectionMode::Explicit, ConnectionMode::Supervised] {
        for status in [400, 404] {
            for key in ["Mcp-Session-Id", "MCP-SESSION-ID"] {
                for value in ["", "caller-session"] {
                    let mut fixture = Fixture::new("server/discover", status, false).await;
                    let McpTransport::Http { headers, .. } = &mut fixture.config.transport else { unreachable!() };
                    headers.insert(key.into(), value.into());
                    let error = fixture.connect(mode).await.err().expect("no fresh Initialize fallback");
                    assert!(matches!(error, McpError::Initialize { .. }), "{error:?}");
                    assert_eq!(fixture.count("server/discover"), 1);
                    assert_eq!(fixture.count("initialize"), 0);
                    assert_eq!(fixture.requests.lock().unwrap()[0].1.as_deref(), Some(value));
                }
            }
        }
    }
}

#[tokio::test]
async fn other_discover_statuses_and_initialize_rejections_do_not_open_an_extra_fallback() {
    for mode in [ConnectionMode::Explicit, ConnectionMode::Supervised] {
        for status in [401, 403, 501] {
            let fixture = Fixture::new("server/discover", status, false).await;
            assert!(fixture.connect(mode).await.is_err());
            assert_eq!(fixture.count("server/discover"), 1);
            assert_eq!(fixture.count("initialize"), 0);
        }
        for status in [400, 404] {
            let fixture = Fixture::new("initialize", status, false).await;
            assert!(fixture.connect(mode).await.is_err());
            assert_eq!(fixture.count("server/discover"), 1);
            assert_eq!(fixture.count("initialize"), 1);
            assert_eq!(fixture.count("tools/list"), 0);
        }
    }
}

#[derive(Default)]
struct EmptyProvider(AtomicUsize);

#[async_trait]
impl McpProviderTokenResolver for EmptyProvider {
    async fn token(&self, _: &str) -> Result<Option<String>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

#[tokio::test]
async fn host_auth_projection_uses_actual_adapter_not_token_presence_or_error_text() {
    // A no-header public config attaches OAuth. Truly anonymous transport therefore belongs
    // at this private SDK/host projection seam, not a fabricated public ACP authentication mode.
    for source in ["anonymous", "static", "provider", "oauth"] {
        for status in [401, 403] {
            for scope in [false, true] {
                let fixture = Fixture::new("initialize", status, scope).await;
                let resolver = Arc::new(EmptyProvider::default());
                let cause = match source {
                    "anonymous" => fixture.rejection(PiHttpClient::new(), HashMap::new()).await,
                    "static" => fixture.rejection(operation::DispatchHttpClient::default(),
                        http_headers("fixture", &BTreeMap::from([("Authorization".into(), format!("Bearer {TOKEN}"))])).unwrap()).await,
                    "provider" => fixture.rejection(ProviderAuthClient::new("fixture-provider".into(), resolver.clone()), HashMap::new()).await,
                    "oauth" => {
                        let client = oauth_http_client("fixture", fixture.url(), None,
                            &McpClientAuth::default(), ConnectionMode::Explicit).await.unwrap();
                        fixture.rejection(client, HashMap::new()).await
                    }
                    _ => unreachable!(),
                };
                let config = McpServerConfig::http("fixture", fixture.url());
                let error = handshake_diagnostic("fixture", &config.transport, None,
                    &McpClientHandler::new(None).stderr, &cause);
                let expected = match source {
                    "provider" => status == 401,
                    "oauth" => status == 401 || scope,
                    _ => false,
                };
                assert_eq!(matches!(&error, McpError::AuthorizationRequired(_)), expected,
                    "{source} {status} scope={scope}: {error:?}");
                if let McpError::AuthorizationRequired(required) = &error {
                    assert_eq!(required.challenge.as_deref(), scope.then_some(SCOPE_CHALLENGE));
                }
                for output in [format!("{cause:?}"), format!("{error:?}"), error.to_string()] {
                    for secret in [BODY_MARKER, "private-scope-marker", TOKEN] {
                        assert!(!output.contains(secret), "diagnostic leaked {secret}");
                    }
                }
                assert_eq!(fixture.count("initialize"), 1, "no hidden replay without refresh credentials");
                assert_eq!(fixture.count("tools/list"), 0);
                assert_eq!(resolver.0.load(Ordering::SeqCst), usize::from(source == "provider"));
            }
        }
    }
}

#[tokio::test]
async fn effective_provider_setup_keeps_login_guidance_without_token_or_replay() {
    for status in [401, 403] {
        for scope in [false, true] {
            let mut fixture = Fixture::new("initialize", status, scope).await;
            fixture.config = fixture.config.clone().headers(BTreeMap::new());
            fixture.config.auth = Some(McpProviderAuth { provider: "fixture-provider".into() });
            let resolver = Arc::new(EmptyProvider::default());
            let auth = McpClientAuth::default().with_provider_tokens(resolver.clone());
            let failure = tokio::time::timeout(Duration::from_secs(5), connect_server(
                fixture.config.clone(), McpConnectionOptions::from(&McpServerPolicy::direct()), auth)).await
                .expect("bounded setup").err().expect("provider rejection");
            if status == 401 {
                let McpError::AuthorizationRequired(required) = failure.error else {
                    panic!("attached provider401 must remain actionable");
                };
                assert_eq!(required.provider.as_deref(), Some("fixture-provider"));
                assert!(!required.static_authorization);
                assert_eq!(required.guidance(), "use pi auth login fixture-provider");
                assert_eq!(required.challenge.as_deref(), scope.then_some(SCOPE_CHALLENGE));
            } else {
                assert!(matches!(failure.error, McpError::Initialize { .. }));
            }
            assert_eq!(resolver.0.load(Ordering::SeqCst), 2, "one token lookup per setup POST");
            assert_eq!(fixture.count("server/discover"), 1);
            assert_eq!(fixture.count("initialize"), 1);
            assert_eq!(fixture.count("tools/list"), 0);
        }
    }
}

#[tokio::test]
async fn static_authorization_over_provider_is_not_actionable_or_resolved() {
    for status in [401, 403] {
        for scope in [false, true] {
            let mut fixture = Fixture::new("initialize", status, scope).await;
            fixture.config.auth = Some(McpProviderAuth { provider: "unused-provider".into() });
            let resolver = Arc::new(EmptyProvider::default());
            let auth = McpClientAuth::default().with_provider_tokens(resolver.clone());
            let failure = tokio::time::timeout(Duration::from_secs(5), connect_server(
                fixture.config.clone(), McpConnectionOptions::from(&McpServerPolicy::direct()), auth)).await
                .expect("bounded setup").err().expect("static rejection");
            assert!(matches!(failure.error, McpError::Initialize { .. }), "{:?}", failure.error);
            assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.count("initialize"), 1);
            assert_eq!(fixture.count("tools/list"), 0);
            assert!(!failure.legacy_fallback);
            assert!(!failure.transient_http);
        }
    }
}
