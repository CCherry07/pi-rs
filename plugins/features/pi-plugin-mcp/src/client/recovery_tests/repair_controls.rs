//! Delayed SDK callbacks stay on their original service while admitted old work drains.
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
use tokio::sync::{Notify, Semaphore, mpsc};

#[derive(Debug)]
enum Completion {
    Cancelled,
    Posted { session: String, reply: Value },
}
struct Held {
    entered: Notify,
    release: Semaphore,
    completion: mpsc::UnboundedSender<Completion>,
}
struct PendingHandler(Option<Arc<Held>>);
impl Drop for PendingHandler {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            let _ = state.completion.send(Completion::Cancelled);
        }
    }
}
struct State {
    initializes: AtomicUsize,
    calls: Mutex<Vec<(String, String)>>,
    held_entered: Notify,
    held_release: Semaphore,
    new_reply: Notify,
    deletes: Mutex<Vec<String>>,
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("control ownership did not settle")
}

async fn control_ownership(keep_admitted: bool) {
    let (completion, mut observed) = mpsc::unbounded_channel();
    let held = Arc::new(Held {
        entered: Notify::new(),
        release: Semaphore::new(0),
        completion: completion.clone(),
    });
    let state = Arc::new(State {
        initializes: AtomicUsize::new(0),
        calls: Mutex::new(vec![]),
        held_entered: Notify::new(),
        held_release: Semaphore::new(0),
        new_reply: Notify::new(),
        deletes: Mutex::new(vec![]),
    });
    let post = state.clone();
    let delete = state.clone();
    let router = axum::Router::new().route("/mcp", axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let completion = completion.clone();
            let state = post.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fake-repair-token");
                if request["id"] == "held-old-request" {
                    assert!(request.get("method").is_none());
                    completion.send(Completion::Posted {
                        session: headers["mcp-session-id"].to_str().unwrap().into(), reply: request,
                    }).unwrap();
                    return http::StatusCode::ACCEPTED.into_response();
                }
                if request["id"] == "new-peer-request" {
                    assert!(request.get("method").is_none());
                    assert_eq!(headers["mcp-session-id"], "replacement-session");
                    assert_eq!(request["error"]["code"], -32092);
                    assert_eq!(request["error"]["message"], "new-handler-result");
                    state.new_reply.notify_one();
                    return http::StatusCode::ACCEPTED.into_response();
                }
                if request["method"] == "server/discover" {
                    return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"legacy fixture"}})).into_response();
                }
                if request["method"] == "initialize" {
                    let n = state.initializes.fetch_add(1, Ordering::SeqCst);
                    let session = if n == 0 { "old-session" } else { "replacement-session" };
                    return ([("mcp-session-id", session)], axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{
                        "protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"held-control","version":session}}}))).into_response();
                }
                if request["method"] == "tools/list" {
                    return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":repair_tools()}})).into_response();
                }
                if request["id"].is_null() { return http::StatusCode::ACCEPTED.into_response(); }
                let session = headers["mcp-session-id"].to_str().unwrap().to_owned();
                let name = request["params"]["name"].as_str().unwrap().to_owned();
                assert_eq!(request["method"], "tools/call");
                state.calls.lock().unwrap().push((session.clone(), name.clone()));
                if name == "held" {
                    assert_eq!(session, "old-session");
                    state.held_entered.notify_one();
                    let stream = futures::stream::once(async { Ok::<_, Infallible>(String::from(": admitted\n\n")) })
                        .chain(futures::stream::once(async move {
                            state.held_release.acquire().await.unwrap().forget();
                            Ok(format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"admitted success"}]}})))
                        }));
                    return ([("content-type", "text/event-stream")], axum::body::Body::from_stream(stream)).into_response();
                }
                assert_eq!(name, "echo");
                if session == "old-session" { return http::StatusCode::NOT_FOUND.into_response(); }
                axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"executed"}]}})).into_response()
            }
        }).get(|headers: http::HeaderMap| async move {
            let id = if headers["mcp-session-id"] == "old-session" { "held-old-request" } else { "new-peer-request" };
            let frame = format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":id,"method":"ping"}));
            let stream = futures::stream::once(async move { Ok::<_, Infallible>(frame) })
                .chain(futures::stream::pending());
            ([("content-type", "text/event-stream")], axum::body::Body::from_stream(stream))
        }).delete(move |headers: http::HeaderMap| { let state = delete.clone(); async move {
            state.deletes.lock().unwrap().push(headers["mcp-session-id"].to_str().unwrap().into());
            http::StatusCode::NO_CONTENT
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let _server = RepairServer(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let callback = held.clone();
    let callbacks = crate::client_handler::testing::Callbacks::new(
        move |ctx| {
            let held = callback.clone();
            async move {
                if ctx.id.to_string() == "held-old-request" {
                    assert_eq!(
                        ctx.peer
                            .peer_info()
                            .unwrap()
                            .server_info
                            .as_ref()
                            .unwrap()
                            .version,
                        "old-session"
                    );
                    let mut pending = PendingHandler(Some(held.clone()));
                    held.entered.notify_one();
                    held.release.acquire().await.unwrap().forget();
                    pending.0 = None;
                    Err(rmcp::ErrorData::new(
                        rmcp::model::ErrorCode(-32091),
                        "old-handler-result",
                        None,
                    ))
                } else {
                    assert_eq!(ctx.id.to_string(), "new-peer-request");
                    assert_eq!(
                        ctx.peer
                            .peer_info()
                            .unwrap()
                            .server_info
                            .as_ref()
                            .unwrap()
                            .version,
                        "replacement-session"
                    );
                    Err(rmcp::ErrorData::new(
                        rmcp::model::ErrorCode(-32092),
                        "new-handler-result",
                        None,
                    ))
                }
            }
            .boxed()
        },
        |_| {},
    );
    let pool = bounded(explicit_with_callbacks(uri, callbacks)).await;
    bounded(held.entered.notified()).await;
    let admitted = if keep_admitted {
        let old_pool = pool.clone();
        let call = tokio::spawn(async move { invoke(&old_pool, "held", context()).await });
        bounded(state.held_entered.notified()).await;
        Some(call)
    } else {
        None
    };
    bounded(invoke(&pool, "echo", context())).await.unwrap();
    assert_eq!(
        state.initializes.load(Ordering::SeqCst),
        2,
        "replacement really published before old handler resumes"
    );
    bounded(state.new_reply.notified()).await;
    if let Some(call) = &admitted {
        assert!(
            !call.is_finished(),
            "old admitted work must not be cancelled to isolate callbacks"
        );
    }
    held.release.add_permits(1);
    match bounded(observed.recv())
        .await
        .expect("callback cancellation or actual wire reply")
    {
        Completion::Cancelled => assert!(
            !keep_admitted,
            "old handler was cancelled despite live admitted work"
        ),
        Completion::Posted { session, reply } => {
            assert_eq!(reply["id"], "held-old-request");
            assert_eq!(reply["error"]["code"], -32091);
            assert_eq!(reply["error"]["message"], "old-handler-result");
            assert_ne!(
                session, "replacement-session",
                "old SDK callback crossed the session epoch on the wire"
            );
            assert_eq!(session, "old-session");
        }
    }
    let mut expected = Vec::new();
    if let Some(call) = admitted {
        state.held_release.add_permits(1);
        let success = bounded(call).await.unwrap().unwrap();
        assert!(
            serde_json::to_string(&success.content)
                .unwrap()
                .contains("admitted success")
        );
        expected.push(("old-session".into(), "held".into()));
    }
    expected.extend([
        ("old-session".into(), "echo".into()),
        ("replacement-session".into(), "echo".into()),
    ]);
    assert_eq!(*state.calls.lock().unwrap(), expected);
    bounded(pool.shutdown()).await.unwrap();
    let mut deletes = state.deletes.lock().unwrap().clone();
    deletes.sort();
    assert_eq!(deletes, ["old-session", "replacement-session"]);
}

#[tokio::test]
async fn held_sdk_handler_cannot_reply_on_a_newly_published_session() {
    control_ownership(false).await;
}

#[tokio::test]
async fn held_sdk_handler_and_admitted_old_success_survive_replacement() {
    control_ownership(true).await;
}
