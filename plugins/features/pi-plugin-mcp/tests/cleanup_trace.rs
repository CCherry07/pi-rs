//! Observe the actual private worker through the public tool-set API, not a replacement transport.
#[path = "cleanup_trace/capture.rs"]
mod capture;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pi_plugin_mcp::{McpServerConfig, McpToolSet};
use serde_json::{Value, json};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::oneshot;

const SESSION: &str = "fixture-cleanup-trace-private-session";
const TOKEN: &str = "Bearer fixture-cleanup-trace-private-token";
const BODY: &str = "fixture-cleanup-trace-private-body";
const BOUND: Duration = Duration::from_secs(2);

struct Observation {
    outcome: Option<StatusCode>,
    initializes: AtomicUsize,
    lists: AtomicUsize,
    deletes: AtomicUsize,
    cleanup_started: Mutex<Option<oneshot::Sender<()>>>,
}

struct Fixture {
    // A separate runtime/thread excludes fixture-server logs from the client subscriber scope.
    _runtime: Runtime,
    server: tokio::task::JoinHandle<()>,
    url: String,
    state: Arc<Observation>,
    started: oneshot::Receiver<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    fn new(outcome: Option<StatusCode>) -> Self {
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (signal, started) = oneshot::channel();
        let state = Arc::new(Observation {
            outcome,
            initializes: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
            cleanup_started: Mutex::new(Some(signal)),
        });
        let (url, server) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let app = axum::Router::new()
                .route(
                    "/mcp",
                    axum::routing::post(post)
                        .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                        .delete(delete),
                )
                .with_state(state.clone());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (url, server)
        });
        Self {
            _runtime: runtime,
            server,
            url,
            state,
            started,
        }
    }
}

async fn post(
    State(state): State<Arc<Observation>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], TOKEN);
    match request["method"].as_str().unwrap() {
        "server/discover" => Json(json!({
            "jsonrpc":"2.0", "id":request["id"],
            "error":{"code":-32601,"message":"Use initialize"}
        }))
        .into_response(),
        "initialize" => {
            state.initializes.fetch_add(1, Ordering::SeqCst);
            (
                [("mcp-session-id", SESSION)],
                Json(json!({
                    "jsonrpc":"2.0", "id":request["id"], "result":{
                        "protocolVersion":"2025-03-26", "capabilities":{"tools":{}},
                        "serverInfo":{"name":"cleanup-trace-fixture","version":"1"}
                    }
                })),
            )
                .into_response()
        }
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            state.lists.fetch_add(1, Ordering::SeqCst);
            Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":{"tools":[]}}))
                .into_response()
        }
        method => panic!("unexpected MCP method {method}"),
    }
}

async fn delete(State(state): State<Arc<Observation>>, headers: HeaderMap) -> Response {
    assert_eq!(headers["authorization"], TOKEN);
    assert_eq!(headers["mcp-session-id"], SESSION);
    assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
    state.deletes.fetch_add(1, Ordering::SeqCst);
    if let Some(started) = state.cleanup_started.lock().unwrap().take() {
        started.send(()).unwrap();
    }
    match state.outcome {
        Some(StatusCode::NO_CONTENT) => StatusCode::NO_CONTENT.into_response(),
        Some(status) => (
            status,
            [(
                "www-authenticate",
                "Bearer realm=fixture-cleanup-trace-private-body",
            )],
            BODY,
        )
            .into_response(),
        None => std::future::pending().await,
    }
}

fn observe(outcome: Option<StatusCode>) {
    let mut fixture = Fixture::new(outcome);
    let capture = capture::Capture::default();
    tracing::subscriber::with_default(capture.clone(), || {
        // Every client task is polled on this thread while the all-events subscriber is active.
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let config = McpServerConfig::http("fixture", &fixture.url)
                .headers(BTreeMap::from([("Authorization".into(), TOKEN.into())]));
            let pool = tokio::time::timeout(BOUND, McpToolSet::connect(vec![config]))
                .await.expect("bounded setup").unwrap();
            capture.begin_cleanup();
            let close = pool.shutdown();
            tokio::pin!(close);
            tokio::time::timeout(BOUND, async {
                tokio::select! {
                    biased;
                    signal = &mut fixture.started => signal.expect("actual DELETE was not observed"),
                    result = &mut close => panic!("shutdown completed before DELETE: {result:?}"),
                }
                close.await.unwrap();
            }).await.expect("bounded cleanup");
        });
        // Retain the scoped subscriber through client runtime teardown, not only close().
        drop(runtime);
    });
    assert_eq!(fixture.state.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.lists.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.deletes.load(Ordering::SeqCst), 1);
    let entries = capture.entries();
    let exits = entries
        .iter()
        .filter(|entry| entry.cleanup && entry.is_sdk_worker_exit())
        .collect::<Vec<_>>();
    assert_eq!(
        exits.len(),
        1,
        "missing real SDK worker-exit positive control: {entries:#?}"
    );
    let before = entries
        .iter()
        .filter(|entry| entry.kind == "event" && !entry.cleanup)
        .count();
    let during = entries
        .iter()
        .filter(|entry| entry.kind == "event" && entry.cleanup)
        .count();
    assert!(
        before > 0,
        "setup events must also reach the same subscriber"
    );
    let exposed = entries
        .iter()
        .filter_map(|entry| {
            let rendered = entry.render();
            [SESSION, TOKEN.strip_prefix("Bearer ").unwrap(), BODY]
                .iter()
                .any(|marker| rendered.contains(marker))
                .then_some(rendered)
        })
        .collect::<Vec<_>>();
    println!(
        "cleanup_capture outcome={outcome:?} events_before={before} events_cleanup={during} sdk_exit={} sensitive_records={}",
        exits[0].render(),
        exposed.len()
    );
    assert!(
        exposed.is_empty(),
        "sensitive trace records (fake fixture markers only): {exposed:#?}"
    );
}

#[test]
fn scoped_cleanup_success_capture() {
    observe(Some(StatusCode::NO_CONTENT));
}

#[test]
fn scoped_cleanup_auth_rejection_capture() {
    observe(Some(StatusCode::UNAUTHORIZED));
}

#[test]
fn scoped_cleanup_server_rejection_capture() {
    observe(Some(StatusCode::INTERNAL_SERVER_ERROR));
}

#[test]
fn scoped_cleanup_deadline_capture() {
    observe(None);
}
