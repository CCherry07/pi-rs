//! Fatal overflow uses typed private errors, not rmcp's private overflow marker.
use super::*;
use crate::http_client::transport_error;
use futures::StreamExt;
use rmcp::{
    model::{CallToolRequestParams, ProgressNotificationParam},
    service::{NotificationContext, ServiceError},
};
use std::{
    convert::Infallible,
    sync::atomic::{AtomicUsize, Ordering},
};

const LIMIT: usize = 512;
const BOUND: Duration = Duration::from_secs(2);
const SESSION: &str = "overflow-session";

struct State {
    common_overflow: bool,
    common_release: Semaphore,
    held_entered: Semaphore,
    held_release: Semaphore,
    bodies_dropped: Semaphore,
    gets: Mutex<Vec<Option<String>>>,
    calls: Mutex<Vec<String>>,
    initializes: AtomicUsize,
    deletes: AtomicUsize,
}

struct BodyLease(Arc<State>);
impl Drop for BodyLease {
    fn drop(&mut self) {
        self.0.bodies_dropped.add_permits(1);
    }
}

fn overflow(state: Arc<State>, common: bool) -> axum::response::Response {
    let lease = BodyLease(state.clone());
    // A complete, cursor-bearing prefix followed by a fatal oversized event. Keeping
    // the source open proves client cancellation/drop, rather than normal server EOF.
    let body = format!(
        "id: cursor-before-overflow\nretry: 0\ndata: {}\n\ndata: {}\n\n",
        json!({"jsonrpc":"2.0","method":"notifications/progress",
            "params":{"progressToken":17,"progress":1}}),
        "private-overflow-marker".repeat(LIMIT)
    );
    let source = futures::stream::once(async move {
        if common {
            state.common_release.acquire().await.unwrap().forget();
        }
        Ok::<_, Infallible>(axum::body::Bytes::from(body))
    })
    .chain(futures::stream::pending())
    .inspect(move |_| {
        let _ = &lease;
    });
    (
        [
            ("content-type", "text/event-stream"),
            ("mcp-session-id", SESSION),
        ],
        axum::body::Body::from_stream(source),
    )
        .into_response()
}

async fn fixture(common_overflow: bool) -> (Server, Arc<State>, String) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state = Arc::new(State {
        common_overflow,
        common_release: Semaphore::new(0),
        held_entered: Semaphore::new(0),
        held_release: Semaphore::new(0),
        bodies_dropped: Semaphore::new(0),
        gets: Mutex::new(Vec::new()),
        calls: Mutex::new(Vec::new()),
        initializes: AtomicUsize::new(0),
        deletes: AtomicUsize::new(0),
    });
    let router = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::post(
                |axum::extract::State(state): axum::extract::State<Arc<State>>,
                 headers: http::HeaderMap,
                 axum::Json(request): axum::Json<Value>| async move {
                    if request["method"] == "initialize" {
                        state.initializes.fetch_add(1, Ordering::SeqCst);
                        return (
                            [("mcp-session-id", SESSION)],
                            axum::Json(json!({
                    "jsonrpc":"2.0","id":request["id"],"result":{
                        "protocolVersion":"2025-03-26","capabilities":{"tools":{}},
                        "serverInfo":{"name":"overflow","version":"1"}}})),
                        )
                            .into_response();
                    }
                    assert_eq!(headers["mcp-session-id"], SESSION);
                    if request["id"].is_null() {
                        return http::StatusCode::ACCEPTED.into_response();
                    }
                    assert_eq!(request["method"], "tools/call");
                    let name = request["params"]["name"].as_str().unwrap().to_owned();
                    state.calls.lock().unwrap().push(name.clone());
                    if name == "overflow" {
                        return overflow(state, false);
                    }
                    if name == "held" {
                        state.held_entered.add_permits(1);
                        state.held_release.acquire().await.unwrap().forget();
                    }
                    axum::Json(json!({"jsonrpc":"2.0","id":request["id"],
                "result":{"content":[{"type":"text","text":name}]}}))
                    .into_response()
                },
            )
            .get(
                |axum::extract::State(state): axum::extract::State<Arc<State>>,
                 headers: http::HeaderMap| async move {
                    assert_eq!(headers["mcp-session-id"], SESSION);
                    state.gets.lock().unwrap().push(
                        headers
                            .get("last-event-id")
                            .map(|value| value.to_str().unwrap().to_owned()),
                    );
                    if state.common_overflow {
                        overflow(state, true)
                    } else {
                        http::StatusCode::METHOD_NOT_ALLOWED.into_response()
                    }
                },
            )
            .delete(
                |axum::extract::State(state): axum::extract::State<Arc<State>>,
                 headers: http::HeaderMap| async move {
                    assert_eq!(headers["mcp-session-id"], SESSION);
                    state.deletes.fetch_add(1, Ordering::SeqCst);
                    http::StatusCode::NO_CONTENT
                },
            ),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    (server, state, uri)
}

fn config(uri: String) -> HttpTransportConfig {
    let mut config = HttpTransportConfig::with_uri(uri);
    config.max_sse_event_size = LIMIT;
    config
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("overflow fixture did not settle")
}

fn assert_overflow(error: &HttpError) {
    let HttpError::Client(error) = error else {
        panic!("lost typed body error: {error:?}")
    };
    assert!(error.status().is_none());
    assert!(error.network_flags().is_none());
    assert!(error.auth_rejection().is_none());
    assert_eq!(
        error.to_string(),
        format!("MCP SSE event exceeded {LIMIT} bytes")
    );
    assert!(!format!("{error:?}").contains("private-overflow-marker"));
}

#[tokio::test]
async fn common_overflow_finishes_the_stream_task_with_its_complete_prefix() {
    let (_server, state, uri) = fixture(true).await;
    let (incoming, mut messages) = mpsc::channel(1);
    let (common, _changes) = watch::channel(None);
    let session = Session {
        id: Some(SESSION.into()),
        ..Session::default()
    };
    let shared = Shared {
        client: PiHttpClient::new(),
        config: config(uri),
        session: Arc::new(Mutex::new(session.clone())),
        cleanup: Arc::new(Mutex::new(None)),
        common,
        incoming,
    };
    state.common_release.add_permits(1);
    // Await the actual production stream future's terminal result; this is positive
    // task completion, not a timeout-based claim that no reopen was observed.
    let error = bounded(shared.common_stream(session)).await.unwrap_err();
    assert_overflow(&error);
    let prefix = serde_json::to_value(messages.try_recv().unwrap()).unwrap();
    assert_eq!(prefix["method"], "notifications/progress");
    assert_eq!(prefix["params"]["progress"].as_f64(), Some(1.0));
    assert!(messages.try_recv().is_err());
    bounded(state.bodies_dropped.acquire())
        .await
        .unwrap()
        .forget();
    assert_eq!(*state.gets.lock().unwrap(), [None]);
}

struct Progress(mpsc::UnboundedSender<ProgressNotificationParam>);
impl rmcp::ClientHandler for Progress {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        self.0.send(params).unwrap();
    }
}

#[tokio::test]
async fn post_overflow_preserves_prefix_and_keeps_independent_requests_usable() {
    let (_server, state, uri) = fixture(false).await;
    let (progress, mut updates) = mpsc::unbounded_channel();
    let mut service = bounded(Progress(progress).serve_with_lifecycle(
        PiHttpTransport::with_client(PiHttpClient::new(), config(uri)),
        ClientLifecycleMode::Initialize,
    ))
    .await
    .unwrap();
    let error = bounded(service.call_tool(CallToolRequestParams::new("overflow")))
        .await
        .unwrap_err();
    let ServiceError::TransportSend(error) = error else {
        panic!("not a typed transport failure: {error:?}")
    };
    let Some(HttpError::Client(stream)) = transport_error(error.error.as_ref()) else {
        panic!("missing stream provenance")
    };
    assert_overflow(
        stream
            .response_stream_cause()
            .expect("retain real overflow cause"),
    );
    assert_eq!(bounded(updates.recv()).await.unwrap().progress, 1.0);
    bounded(state.bodies_dropped.acquire())
        .await
        .unwrap()
        .forget();
    let result = bounded(service.call_tool(CallToolRequestParams::new("probe")))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    assert_eq!(
        serde_json::to_value(result).unwrap()["content"][0]["text"],
        "probe"
    );
    bounded(service.close()).await.unwrap();
    assert_eq!(*state.calls.lock().unwrap(), ["overflow", "probe"]);
    assert!(
        state.gets.lock().unwrap().iter().all(Option::is_none),
        "fatal overflow must not resume with its cursor"
    );
    assert_eq!(state.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn common_overflow_keeps_an_admitted_response_and_the_service_alive() {
    let (_server, state, uri) = fixture(true).await;
    let (progress, mut updates) = mpsc::unbounded_channel();
    let mut service = bounded(Progress(progress).serve_with_lifecycle(
        PiHttpTransport::with_client(PiHttpClient::new(), config(uri)),
        ClientLifecycleMode::Initialize,
    ))
    .await
    .unwrap();
    let peer = service.peer().clone();
    let held =
        tokio::spawn(async move { peer.call_tool(CallToolRequestParams::new("held")).await });
    bounded(state.held_entered.acquire())
        .await
        .unwrap()
        .forget();
    state.common_release.add_permits(1);
    assert_eq!(bounded(updates.recv()).await.unwrap().progress, 1.0);
    bounded(state.bodies_dropped.acquire())
        .await
        .unwrap()
        .forget();
    assert!(
        !held.is_finished(),
        "common overflow must not cancel the admitted RPC"
    );
    let probe = bounded(service.call_tool(CallToolRequestParams::new("probe")))
        .await
        .unwrap();
    assert_ne!(probe.is_error, Some(true));
    state.held_release.add_permits(1);
    let response = bounded(held).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(response).unwrap()["content"][0]["text"],
        "held"
    );
    bounded(service.close()).await.unwrap();
    assert_eq!(*state.gets.lock().unwrap(), [None]);
    assert_eq!(*state.calls.lock().unwrap(), ["held", "probe"]);
    assert_eq!(state.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
}
