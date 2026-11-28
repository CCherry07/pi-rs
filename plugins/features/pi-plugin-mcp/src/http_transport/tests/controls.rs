//! Raw typed control evidence retained separately from fresh-owner setup errors.
use super::*;
use crate::{client::operation::OperationError, http_client::transport_error};
use futures::StreamExt;
use rmcp::{
    ClientHandler, RoleClient,
    model::ProgressNotificationParam,
    service::{NotificationContext, PeerRequestOptions, ServiceError},
};
use std::{
    convert::Infallible,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Progress(mpsc::UnboundedSender<()>);
impl ClientHandler for Progress {
    async fn on_progress(&self, _: ProgressNotificationParam, _: NotificationContext<RoleClient>) {
        self.0.send(()).unwrap();
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("bounded raw control operation")
}

struct TailBodyDrop(Arc<Notify>);

impl Drop for TailBodyDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[tokio::test]
async fn cancellation_after_the_primary_reply_drops_the_response_tail_only() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let initializes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(Notify::new());
    let tail_entered = Arc::new(Notify::new());
    let tail_dropped = Arc::new(Notify::new());
    let tail_release = Arc::new(Semaphore::new(0));
    let init = Arc::clone(&initializes);
    let count = Arc::clone(&calls);
    let observed_cancel = Arc::clone(&cancelled);
    let entered = Arc::clone(&tail_entered);
    let dropped = Arc::clone(&tail_dropped);
    let release = Arc::clone(&tail_release);
    let route = axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let init = Arc::clone(&init);
            let count = Arc::clone(&count);
            let observed_cancel = Arc::clone(&observed_cancel);
            let entered = Arc::clone(&entered);
            let dropped = Arc::clone(&dropped);
            let release = Arc::clone(&release);
            async move {
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        init.fetch_add(1, Ordering::SeqCst);
                        (
                            [("mcp-session-id", "tail-session")],
                            axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{
                                "protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"tail-cancel","version":"1"}}})),
                        )
                            .into_response()
                    }
                    "tools/call" => {
                        let call = count.fetch_add(1, Ordering::SeqCst);
                        if call > 0 {
                            return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{
                                "content":[{"type":"text","text":"second"}]}}))
                                .into_response();
                        }
                        let response = format!(
                            "data: {}\n\n",
                            json!({"jsonrpc":"2.0","id":request["id"],"result":{
                                "content":[{"type":"text","text":"first"}]}})
                        );
                        let tail = format!(
                            "data: {}\n\n",
                            json!({"jsonrpc":"2.0","method":"notifications/progress","params":{
                                "progressToken":17,"progress":2}})
                        );
                        let body = futures::stream::once(async move {
                            Ok::<_, Infallible>(response)
                        })
                        .chain(futures::stream::once(async move {
                            let _drop = TailBodyDrop(dropped);
                            entered.notify_one();
                            release.acquire().await.unwrap().forget();
                            Ok::<_, Infallible>(tail)
                        }));
                        (
                            [("content-type", "text/event-stream")],
                            axum::body::Body::from_stream(body),
                        )
                            .into_response()
                    }
                    "notifications/cancelled" => {
                        assert_eq!(headers["mcp-session-id"], "tail-session");
                        observed_cancel.notify_one();
                        http::StatusCode::ACCEPTED.into_response()
                    }
                    "notifications/initialized" => http::StatusCode::ACCEPTED.into_response(),
                    other => panic!("unexpected method: {other}"),
                }
            }
        },
    )
    .get(|| async { http::StatusCode::METHOD_NOT_ALLOWED })
    .delete(|| async { http::StatusCode::NO_CONTENT });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    }));
    let (updates, mut progress) = mpsc::unbounded_channel();
    let mut service = bounded(Progress(updates).serve_with_lifecycle(
        PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
        ClientLifecycleMode::Initialize,
    ))
    .await
    .unwrap();
    let request: ClientRequest = serde_json::from_value(
        json!({"method":"tools/call","params":{"name":"held","arguments":{}}}),
    )
    .unwrap();
    let mut handle = service
        .peer()
        .send_request_with_option(request.clone(), PeerRequestOptions::default())
        .await
        .unwrap();
    let response = std::mem::replace(&mut handle.rx, tokio::sync::oneshot::channel().1);
    bounded(response).await.unwrap().unwrap();
    bounded(tail_entered.notified()).await;
    bounded(handle.cancel(Some("primary reply already consumed".into())))
        .await
        .unwrap();
    bounded(cancelled.notified()).await;
    bounded(tail_dropped.notified()).await;
    assert!(
        progress.try_recv().is_err(),
        "cancelled tail leaked progress"
    );

    service.peer().send_request(request).await.unwrap();
    assert_eq!(initializes.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    bounded(service.close()).await.unwrap();
}

#[tokio::test]
async fn sdk_cancellation_retains_typed_original_error_and_original_session_control() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let initializes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(Notify::new());
    let posts = Arc::new(Mutex::new(Vec::new()));
    let init = initializes.clone();
    let count = calls.clone();
    let observed = cancelled.clone();
    let controls = posts.clone();
    let route = axum::routing::post(move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
        let (init, count, observed, controls) = (init.clone(), count.clone(), observed.clone(), controls.clone());
        async move {
            match request["method"].as_str().unwrap() {
                "initialize" => {
                    init.fetch_add(1, Ordering::SeqCst);
                    ([("mcp-session-id", "control-session")], axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{
                        "protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"controls","version":"1"}}}))).into_response()
                }
                "tools/call" => {
                    count.fetch_add(1, Ordering::SeqCst);
                    let stream = futures::stream::iter([Ok::<_, Infallible>("data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":17,\"progress\":1}}\n\n")]).chain(futures::stream::pending());
                    ([("content-type", "text/event-stream")], axum::body::Body::from_stream(stream)).into_response()
                }
                "notifications/cancelled" => {
                    assert_eq!(headers["mcp-session-id"], "control-session");
                    controls.lock().unwrap().push(request["params"]["requestId"].clone());
                    observed.notify_one();
                    http::StatusCode::ACCEPTED.into_response()
                }
                "notifications/initialized" => http::StatusCode::ACCEPTED.into_response(),
                other => panic!("unexpected method: {other}"),
            }
        }
    }).get(|| async { http::StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { http::StatusCode::NO_CONTENT });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    }));
    let (updates, mut progress) = mpsc::unbounded_channel();
    let mut service = bounded(Progress(updates).serve_with_lifecycle(
        PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
        ClientLifecycleMode::Initialize,
    ))
    .await
    .unwrap();
    let request: ClientRequest = serde_json::from_value(
        json!({"method":"tools/call","params":{"name":"held","arguments":{}}}),
    )
    .unwrap();
    let mut handle = service
        .peer()
        .send_request_with_option(request, PeerRequestOptions::default())
        .await
        .unwrap();
    let id = serde_json::to_value(&handle.id).unwrap();
    assert_eq!(bounded(progress.recv()).await, Some(()));
    let response = std::mem::replace(&mut handle.rx, tokio::sync::oneshot::channel().1);
    let cancellation = tokio::spawn(handle.cancel(None));
    let error = bounded(response).await.unwrap().unwrap_err();
    let ServiceError::TransportSend(error) = error else {
        panic!("typed cancellation: {error:?}")
    };
    assert!(
        matches!(transport_error(error.error.as_ref()), Some(HttpError::Io(error))
        if error.get_ref().and_then(|error| error.downcast_ref::<OperationError>()) == Some(&OperationError::Cancelled))
    );
    bounded(cancellation).await.unwrap().unwrap();
    bounded(cancelled.notified()).await;
    assert_eq!(*posts.lock().unwrap(), [id]);
    assert_eq!(initializes.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    bounded(service.close()).await.unwrap();
}

#[tokio::test]
async fn bootstrap_control_deadline_retains_the_original_typed_timeout() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (observed, mut pongs) = mpsc::unbounded_channel();
    let route = axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let observed = observed.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fake-control-token");
                if request["method"] == "initialize" {
                    return (
                        [
                            ("mcp-session-id", "control-session"),
                            ("content-type", "text/event-stream"),
                        ],
                        "data: {\"jsonrpc\":\"2.0\",\"id\":71,\"method\":\"ping\"}\n\n",
                    )
                        .into_response();
                }
                assert_eq!(request["id"], 71);
                assert_eq!(request["result"], json!({}));
                assert!(request.get("method").is_none());
                assert_eq!(headers["mcp-session-id"], "control-session");
                assert!(!headers.contains_key("mcp-protocol-version"));
                observed.send(()).unwrap();
                std::future::pending::<axum::response::Response>().await
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    }));
    let mut config = HttpTransportConfig::with_uri(uri).custom_headers(HashMap::from([(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer fake-control-token"),
    )]));
    config.control_request_timeout = Duration::from_millis(50);
    let (common, _changes) = watch::channel(None);
    let (incoming, _messages) = mpsc::channel(1);
    let shared = Shared {
        client: PiHttpClient::new(),
        config,
        session: Arc::new(Mutex::new(Session::default())),
        cleanup: Arc::new(Mutex::new(None)),
        common,
        incoming,
    };
    let initialize: ClientJsonRpcMessage = serde_json::from_value(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"control-test","version":"1"}}})).unwrap();
    let mut session = Session::default();
    let response = bounded(shared.post(initialize, &session)).await.unwrap();
    shared.capture(&mut session, &response);
    let PostResponse::Sse(mut body, _) = response else {
        panic!("actual setup SSE required")
    };
    let frame = bounded(body.next()).await.unwrap().unwrap();
    let ping = serde_json::from_str(&frame.data.unwrap()).unwrap();
    let error = bounded(shared.bootstrap_ping(&ping, &session))
        .await
        .unwrap_err();
    assert!(matches!(error, HttpError::ControlRequestTimeout));
    assert_eq!(
        bounded(pongs.recv()).await,
        Some(()),
        "actual wire Pong preceded timeout"
    );
}
