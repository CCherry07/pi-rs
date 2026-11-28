//! Native output observations, not proof of silence with an embedding's custom subscriber.
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::sync::oneshot;

const SESSION: &str = "fixture-private-cleanup-session";
const TOKEN: &str = "Bearer fixture-private-cleanup-token";
const BODY: &str = "fixture-private-cleanup-body";

struct Observation {
    outcome: Option<StatusCode>,
    initializes: AtomicUsize,
    lists: AtomicUsize,
    deletes: AtomicUsize,
    cleanup_started: Mutex<Option<oneshot::Sender<()>>>,
}

struct Server(tokio::task::JoinHandle<()>);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
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
                        "serverInfo":{"name":"cleanup-fixture","version":"1"}
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
                "Bearer realm=fixture-private-cleanup-body",
            )],
            BODY,
        )
            .into_response(),
        None => std::future::pending().await,
    }
}

async fn observe_cleanup(outcome: Option<StatusCode>) {
    let (started, cleanup_started) = oneshot::channel();
    let state = Arc::new(Observation {
        outcome,
        initializes: AtomicUsize::new(0),
        lists: AtomicUsize::new(0),
        deletes: AtomicUsize::new(0),
        cleanup_started: Mutex::new(Some(started)),
    });
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
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::create_dir(&agent).unwrap();
    fs::write(
        agent.join("mcp.json"),
        json!({"mcpServers":{"fixture":{
            "url":url, "headers":{"Authorization":TOKEN}
        }}})
        .to_string(),
    )
    .unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pi"));
    command
        .arg("--cwd")
        .arg(root.path())
        .arg("--agent-dir")
        .arg(&agent)
        .args(["--no-approve", "mcp", "list", "--json"])
        // RUST_LOG alone does not install a subscriber. Observe the actual native product
        // configuration without adding a filter, suppressing an event or changing the SDK.
        .env("RUST_LOG", "trace")
        .kill_on_drop(true);
    let mut output = Box::pin(command.output());
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            biased;
            observed = cleanup_started => observed.expect("actual DELETE was not observed"),
            result = &mut output => panic!("CLI exited before actual DELETE: {result:?}"),
        }
    })
    .await
    .expect("native MCP startup did not reach cleanup");
    let output = tokio::time::timeout(Duration::from_secs(2), output)
        .await
        .expect("native CLI did not finish bounded cleanup")
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"], json!([]));
    assert_eq!(report["servers"].as_array().unwrap().len(), 1);
    assert_eq!(report["servers"][0]["name"], "fixture");
    assert_eq!(report["servers"][0]["state"], "connected");
    assert!(output.stderr.is_empty());
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for private in [SESSION, TOKEN, BODY] {
            assert!(!text.contains(private));
        }
    }
    assert_eq!(state.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(state.lists.load(Ordering::SeqCst), 1);
    assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
    assert!(!agent.join("sessions").exists());
    assert!(!agent.join("mcp-auth.json").exists());
}

#[tokio::test]
async fn native_cleanup_success_does_not_expose_http_secrets() {
    observe_cleanup(Some(StatusCode::NO_CONTENT)).await;
}

#[tokio::test]
async fn native_cleanup_auth_rejection_does_not_expose_http_secrets() {
    observe_cleanup(Some(StatusCode::UNAUTHORIZED)).await;
}

#[tokio::test]
async fn native_cleanup_server_rejection_does_not_expose_http_secrets() {
    observe_cleanup(Some(StatusCode::INTERNAL_SERVER_ERROR)).await;
}

#[tokio::test]
async fn native_cleanup_deadline_does_not_expose_http_secrets() {
    observe_cleanup(None).await;
}
