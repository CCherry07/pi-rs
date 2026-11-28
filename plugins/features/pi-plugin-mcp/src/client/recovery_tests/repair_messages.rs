//! Fresh explicit services retain genuine prefixes and SDK-owned post-reply messages.
use super::*;
use axum::response::IntoResponse;
use futures::{FutureExt, StreamExt};
use std::{
    convert::Infallible,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, mpsc};

struct State {
    initializes: AtomicUsize,
    rejections: AtomicUsize,
    executed: AtomicUsize,
    pong: Notify,
    tail_reply: Notify,
    replies: Mutex<Vec<Value>>,
    deletes: Mutex<Vec<String>>,
}

fn progress(number: u64) -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":17,"progress":number}})
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("repair messages did not settle")
}

async fn repair_messages(json_response: bool) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state = Arc::new(State {
        initializes: AtomicUsize::new(0),
        rejections: AtomicUsize::new(0),
        executed: AtomicUsize::new(0),
        pong: Notify::new(),
        tail_reply: Notify::new(),
        replies: Mutex::new(Vec::new()),
        deletes: Mutex::new(Vec::new()),
    });
    let post = state.clone();
    let delete = state.clone();
    let router = axum::Router::new().route("/mcp", axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let state = post.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fake-repair-token");
                if request["id"] == "before-repair" || request["id"] == "after-repair" {
                    assert!(request.get("method").is_none());
                    assert_eq!(headers["mcp-session-id"], "candidate-session");
                    let before = request["id"] == "before-repair";
                    if before {
                        // A fresh transport has not negotiated yet: never inherit the old
                        // peer's version (frozen client.ts:239-243 / HTTP headers:391).
                        assert!(!headers.contains_key("mcp-protocol-version"));
                        assert_eq!(request["result"], json!({}));
                        assert!(request.get("error").is_none());
                    } else {
                        assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
                        assert_eq!(request["error"]["code"], -32091);
                        assert_eq!(request["error"]["message"], "sdk-repair-tail");
                        assert!(request.get("result").is_none());
                    }
                    state.replies.lock().unwrap().push(request);
                    if before { state.pong.notify_one(); } else { state.tail_reply.notify_one(); }
                    return http::StatusCode::ACCEPTED.into_response();
                }
                if request["method"] == "server/discover" {
                    return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"legacy fixture"}})).into_response();
                }
                if request["method"] == "tools/list" {
                    return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":repair_tools()}})).into_response();
                }
                if request["method"] == "initialize" {
                    let n = state.initializes.fetch_add(1, Ordering::SeqCst);
                    let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                        "protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"repair-messages","version":"1"}}});
                    if n == 0 { return ([("mcp-session-id", "old-session")], axum::Json(reply)).into_response(); }
                    let prefix = vec![progress(1), json!({"jsonrpc":"2.0","id":"before-repair","method":"ping"})];
                    let tail = vec![reply, progress(2), json!({"jsonrpc":"2.0","id":"after-repair","method":"ping"})];
                    if json_response {
                        return ([("mcp-session-id", "candidate-session")], axum::Json(prefix.into_iter().chain(tail).collect::<Vec<_>>())).into_response();
                    }
                    let frames = |messages: Vec<Value>| messages.into_iter().map(|message| format!("data: {message}\n\n")).collect::<String>();
                    let body = futures::stream::iter([Ok::<_, Infallible>(frames(prefix))])
                        .chain(futures::stream::once(async move {
                            // The server cannot finish initialization until the genuine Pong.
                            state.pong.notified().await;
                            Ok(frames(tail))
                        }));
                    return ([("mcp-session-id", "candidate-session"), ("content-type", "text/event-stream")], axum::body::Body::from_stream(body)).into_response();
                }
                if request["id"].is_null() { return http::StatusCode::ACCEPTED.into_response(); }
                if headers["mcp-session-id"] == "old-session" {
                    state.rejections.fetch_add(1, Ordering::SeqCst);
                    return http::StatusCode::NOT_FOUND.into_response();
                }
                state.executed.fetch_add(1, Ordering::SeqCst);
                assert_eq!(request["method"], "tools/call");
                assert_eq!(request["params"]["name"], "echo");
                axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"executed"}]}})).into_response()
            }
        }).get(|| async { http::StatusCode::METHOD_NOT_ALLOWED })
        .delete(move |headers: http::HeaderMap| { let state = delete.clone(); async move {
            state.deletes.lock().unwrap().push(headers["mcp-session-id"].to_str().unwrap().into());
            http::StatusCode::NO_CONTENT
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let _server = RepairServer(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let (progress, mut events) = mpsc::unbounded_channel();
    let pings = Arc::new(AtomicUsize::new(0));
    let calls = pings.clone();
    let callbacks = crate::client_handler::testing::Callbacks::new(
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Err(rmcp::ErrorData::new(
                    rmcp::model::ErrorCode(-32091),
                    "sdk-repair-tail",
                    None,
                ))
            }
            .boxed()
        },
        move |value| {
            progress.send(value).unwrap();
        },
    );
    let pool = bounded(explicit_with_callbacks(uri, callbacks)).await;
    bounded(invoke(&pool, "echo", context())).await.unwrap();
    assert_eq!(bounded(events.recv()).await, Some(1.0));
    assert_eq!(bounded(events.recv()).await, Some(2.0));
    bounded(state.tail_reply.notified()).await;
    assert_eq!(
        pings.load(Ordering::SeqCst),
        1,
        "pre-final Ping must not reach the SDK handler"
    );
    assert_eq!(
        state.replies.lock().unwrap().len(),
        2,
        "exactly one reply per genuine Ping"
    );
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(state.rejections.load(Ordering::SeqCst), 1);
    assert_eq!(state.executed.load(Ordering::SeqCst), 1);
    bounded(pool.shutdown()).await.unwrap();
    let mut deletes = state.deletes.lock().unwrap().clone();
    deletes.sort();
    assert_eq!(deletes, ["candidate-session", "old-session"]);
}

#[tokio::test]
async fn json_repair_preserves_prefix_and_routes_sdk_tail_controls_to_the_new_epoch() {
    repair_messages(true).await;
}

#[tokio::test]
async fn sse_repair_preserves_prefix_and_routes_sdk_tail_controls_to_the_new_epoch() {
    repair_messages(false).await;
}
