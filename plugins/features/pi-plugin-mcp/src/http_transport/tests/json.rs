//! Real JSON arrays retain member order and prefix delivery without a synthetic SSE bridge.
use super::*;
use rmcp::{
    model::{CallToolRequestParams, ProgressNotificationParam},
    service::{NotificationContext, RequestContext},
};
use std::sync::atomic::{AtomicUsize, Ordering};

const BOUND: Duration = Duration::from_secs(2);

struct Handler {
    progress: mpsc::UnboundedSender<f64>,
    pings: Arc<AtomicUsize>,
}
impl rmcp::ClientHandler for Handler {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        self.progress.send(params.progress).unwrap();
    }
    async fn ping(&self, _: RequestContext<RoleClient>) -> Result<(), rmcp::ErrorData> {
        self.pings.fetch_add(1, Ordering::SeqCst);
        Err(rmcp::ErrorData::new(
            rmcp::model::ErrorCode(-32091),
            "sdk-json-ping",
            None,
        ))
    }
}

fn progress(number: u64) -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/progress",
        "params":{"progressToken":0,"progress":number}})
}

async fn fixture(setup_batch: bool) -> (Server, String, watch::Receiver<Vec<Value>>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (wire, replies) = watch::channel(Vec::<Value>::new());
    let router = axum::Router::new().route("/mcp", axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let replies = wire.clone();
            async move {
                if request["method"] == "initialize" {
                    let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                        "protocolVersion":"2025-03-26","capabilities":{"tools":{}},
                        "serverInfo":{"name":"json-array","version":"1"}}});
                    let response = if setup_batch {
                        let mut messages = vec![json!({"jsonrpc":"2.0","id":"before-json-init","method":"ping"})];
                        messages.extend((0..64).map(progress));
                        messages.push(response);
                        messages.push(json!({"jsonrpc":"2.0","id":"after-json-init","method":"ping"}));
                        Value::Array(messages)
                    } else { response };
                    return ([("mcp-session-id", "json-session")], axum::Json(response)).into_response();
                }
                assert_eq!(headers["mcp-session-id"], "json-session");
                if request.get("method").is_none() {
                    replies.send_modify(|replies| replies.push(request));
                    return http::StatusCode::ACCEPTED.into_response();
                }
                if request["id"].is_null() { return http::StatusCode::ACCEPTED.into_response(); }
                let result = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                    "content":[{"type":"text","text":"json-result"}]}});
                let response = match request["params"]["name"].as_str().unwrap() {
                    "batch" => json!([progress(1), progress(2), result,
                        {"jsonrpc":"2.0","id":"array-tail","method":"ping"}]),
                    "invalid" => json!([progress(7), {"invalid":"private-json-marker"}, progress(8), result]),
                    "invalid-tail" => json!([result, progress(9), {"invalid":"private-tail-marker"}, progress(10)]),
                    "probe" => result,
                    _ => panic!("unexpected test tool"),
                };
                axum::Json(response).into_response()
            }
        }).get(|| async { http::StatusCode::METHOD_NOT_ALLOWED })
        .delete(|| async { http::StatusCode::NO_CONTENT }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    (server, uri, replies)
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("JSON array did not settle")
}

#[tokio::test]
async fn application_array_delivers_ordered_notifications_reply_and_sdk_owned_tail_ping() {
    let (_server, uri, mut replies) = fixture(false).await;
    let (progress, mut events) = mpsc::unbounded_channel();
    let pings = Arc::new(AtomicUsize::new(0));
    let mut service = bounded(
        Handler {
            progress,
            pings: pings.clone(),
        }
        .serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .unwrap();
    let result = bounded(service.call_tool(CallToolRequestParams::new("batch")))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["content"][0]["text"],
        "json-result"
    );
    assert_eq!(bounded(events.recv()).await, Some(1.0));
    assert_eq!(bounded(events.recv()).await, Some(2.0));
    // A subsequent real request provides progress while the standard SDK handles the tail.
    bounded(service.call_tool(CallToolRequestParams::new("probe")))
        .await
        .unwrap();
    let messages = bounded(replies.wait_for(|messages| !messages.is_empty()))
        .await
        .unwrap()
        .clone();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], "array-tail");
    assert_eq!(messages[0]["error"]["code"], -32091);
    assert!(messages[0].get("result").is_none());
    assert_eq!(pings.load(Ordering::SeqCst), 1);
    bounded(service.close()).await.unwrap();
}

#[tokio::test]
async fn invalid_array_member_preserves_only_the_valid_prefix_and_does_not_poison_service() {
    let (_server, uri, _replies) = fixture(false).await;
    let (progress, mut events) = mpsc::unbounded_channel();
    let mut service = bounded(
        Handler {
            progress,
            pings: Arc::new(AtomicUsize::new(0)),
        }
        .serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .unwrap();
    let error = bounded(service.call_tool(CallToolRequestParams::new("invalid")))
        .await
        .unwrap_err();
    assert!(!format!("{error} {error:?}").contains("private-json-marker"));
    assert_eq!(bounded(events.recv()).await, Some(7.0));
    let result = bounded(service.call_tool(CallToolRequestParams::new("probe")))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["content"][0]["text"],
        "json-result"
    );
    bounded(service.close()).await.unwrap();
    drop(service);
    assert_eq!(
        bounded(events.recv()).await,
        None,
        "members after the malformed one must not be emitted"
    );
}

#[tokio::test]
async fn invalid_array_tail_cannot_fail_the_completed_reply_or_reach_later_members() {
    let (_server, uri, _replies) = fixture(false).await;
    let (progress, mut events) = mpsc::unbounded_channel();
    let mut service = bounded(
        Handler {
            progress,
            pings: Arc::new(AtomicUsize::new(0)),
        }
        .serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .unwrap();
    let result = bounded(service.call_tool(CallToolRequestParams::new("invalid-tail")))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["content"][0]["text"],
        "json-result"
    );
    assert_eq!(bounded(events.recv()).await, Some(9.0));
    bounded(service.call_tool(CallToolRequestParams::new("probe")))
        .await
        .unwrap();
    bounded(service.close()).await.unwrap();
    drop(service);
    assert_eq!(bounded(events.recv()).await, None);
}

#[tokio::test]
async fn setup_error_correlation_remains_sdk_owned_for_single_and_array_documents() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for batch in [false, true] {
        let router = axum::Router::new().route(
            "/mcp",
            axum::routing::post(move |axum::Json(request): axum::Json<Value>| async move {
                assert_eq!(request["method"], "initialize");
                assert_ne!(request["id"], "another-request");
                let error = json!({"jsonrpc":"2.0","id":"another-request","error":{
                    "code":-32601,"message":"fixture setup rejection"}});
                axum::Json(if batch { json!([error]) } else { error })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
        let _server = Server(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        let error = bounded(().serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
            ClientLifecycleMode::Initialize,
        ))
        .await
        .expect_err("SDK must reject the uncorrelated response");
        assert!(
            matches!(error, rmcp::service::ClientInitializeError::UncorrelatedErrorResponse { ref received, .. }
            if received == &RequestId::String("another-request".into())),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn setup_array_makes_progress_through_bounded_channel_and_respects_ping_boundary() {
    let (_server, uri, mut replies) = fixture(true).await;
    let (progress, mut events) = mpsc::unbounded_channel();
    let pings = Arc::new(AtomicUsize::new(0));
    let mut config = HttpTransportConfig::with_uri(uri);
    config.channel_buffer_capacity = 1;
    let mut service = bounded(
        Handler {
            progress,
            pings: pings.clone(),
        }
        .serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), config),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .unwrap();
    for number in 0..64 {
        assert_eq!(bounded(events.recv()).await, Some(f64::from(number)));
    }
    let replies = bounded(replies.wait_for(|messages| messages.len() == 2))
        .await
        .unwrap()
        .clone();
    assert_eq!(replies[0]["id"], "before-json-init");
    assert_eq!(replies[0]["result"], json!({}));
    assert!(replies[0].get("error").is_none());
    assert_eq!(replies[1]["id"], "after-json-init");
    assert_eq!(replies[1]["error"]["code"], -32091);
    assert!(replies[1].get("result").is_none());
    assert_eq!(pings.load(Ordering::SeqCst), 1);
    bounded(service.close()).await.unwrap();
}
