//! Numeric bootstrap Ping, bounded control and candidate cleanup through the fresh owner.
use super::*;
use axum::response::IntoResponse;
use futures::{FutureExt, StreamExt};
use std::{
    convert::Infallible,
    sync::{Mutex, atomic::AtomicUsize},
};
use tokio::sync::{Notify, Semaphore};

struct State {
    initializes: AtomicUsize,
    executed: AtomicUsize,
    pong_seen: Notify,
    pong_accepted: Notify,
    release_pong: Semaphore,
    deleted: Mutex<Vec<String>>,
    deleted_changed: Notify,
    hold_pong: bool,
}

async fn start(
    hold_pong: bool,
    control_timeout: Duration,
) -> (RepairServer, Arc<State>, McpToolSet) {
    let state = Arc::new(State {
        initializes: AtomicUsize::new(0),
        executed: AtomicUsize::new(0),
        pong_seen: Notify::new(),
        pong_accepted: Notify::new(),
        release_pong: Semaphore::new(0),
        deleted: Mutex::new(Vec::new()),
        deleted_changed: Notify::new(),
        hold_pong,
    });
    let post = state.clone();
    let delete = state.clone();
    let route = axum::routing::post(move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
        let state = post.clone();
        async move {
            assert_eq!(headers["authorization"], "Bearer fake-repair-token");
            if request["id"] == 71 {
                assert!(request.get("method").is_none());
                assert_eq!(request["result"], json!({}));
                assert_eq!(headers["mcp-session-id"], "candidate-session");
                assert!(!headers.contains_key("mcp-protocol-version"), "a fresh candidate has not negotiated yet");
                state.pong_seen.notify_one();
                if state.hold_pong { state.release_pong.acquire().await.unwrap().forget(); }
                state.pong_accepted.notify_one();
                return http::StatusCode::ACCEPTED.into_response();
            }
            if request["method"] == "server/discover" {
                return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"legacy fixture"}})).into_response();
            }
            if request["method"] == "tools/list" {
                return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":repair_tools()}})).into_response();
            }
            if request["method"] == "initialize" {
                let number = state.initializes.fetch_add(1, Ordering::SeqCst) + 1;
                let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                    "protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"bootstrap","version":"1"}}});
                if number == 1 {
                    return ([("mcp-session-id", "old-session")], axum::Json(response)).into_response();
                }
                let stream = futures::stream::iter([Ok::<_, Infallible>(
                    "data: {\"jsonrpc\":\"2.0\",\"id\":71,\"method\":\"ping\"}\n\n".to_owned())])
                    .chain(futures::stream::once(async move {
                        state.pong_accepted.notified().await;
                        Ok(format!("data: {response}\n\n"))
                    }));
                return ([("mcp-session-id", "candidate-session"), ("content-type", "text/event-stream")],
                    axum::body::Body::from_stream(stream)).into_response();
            }
            if request["id"].is_null() { return http::StatusCode::ACCEPTED.into_response(); }
            assert_eq!(request["method"], "tools/call");
            if headers["mcp-session-id"] == "old-session" { return http::StatusCode::NOT_FOUND.into_response(); }
            state.executed.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"executed"}]}})).into_response()
        }
    }).get(|| async { http::StatusCode::METHOD_NOT_ALLOWED })
        .delete(move |headers: http::HeaderMap| {
            let state = delete.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fake-repair-token");
                state.deleted.lock().unwrap().push(headers["mcp-session-id"].to_str().unwrap().to_owned());
                state.deleted_changed.notify_one();
                http::StatusCode::NO_CONTENT
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = RepairServer(tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    }));
    let callbacks = crate::client_handler::testing::Callbacks::new(
        |_| async { panic!("pre-final Ping must not enter the post-setup handler") }.boxed(),
        |_| {},
    )
    .with_control_timeout(control_timeout);
    let pool = explicit_with_callbacks(uri, callbacks).await;
    (server, state, pool)
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("bounded bootstrap operation")
}

async fn cleanup(pool: &McpToolSet, state: &State) {
    bounded(async {
        pool.shutdown().await.unwrap();
        // SDK cancellation of an incomplete fresh handshake owns its transport teardown.
        // Require actual DELETEs, not a quiet interval or an inferred cancellation.
        loop {
            let changed = state.deleted_changed.notified();
            if state.deleted.lock().unwrap().len() >= 2 {
                break;
            }
            changed.await;
        }
    })
    .await;
    let mut deleted = state.deleted.lock().unwrap().clone();
    deleted.sort();
    assert_eq!(deleted, ["candidate-session", "old-session"]);
}

#[tokio::test]
async fn repair_ping_uses_original_numeric_id_auth_and_candidate_session() {
    let (_server, state, pool) = start(false, Duration::from_secs(5)).await;
    bounded(invoke(&pool, "echo", context())).await.unwrap();
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(state.executed.load(Ordering::SeqCst), 1);
    cleanup(&pool, &state).await;
}

#[tokio::test]
async fn stalled_repair_pong_obeys_the_control_deadline_without_application_replay() {
    let (_server, state, pool) = start(true, Duration::from_millis(50)).await;
    let request_pool = pool.clone();
    let request = tokio::spawn(async move { invoke(&request_pool, "echo", context()).await });
    bounded(state.pong_seen.notified()).await;
    bounded(request).await.unwrap().unwrap_err();
    let readiness = pool.inner.servers[0]
        .binding
        .as_ref()
        .unwrap()
        .owned()
        .unwrap()
        .readiness();
    assert!(
        matches!(readiness, crate::supervisor::ConnectionReadiness::Failed(error)
        if matches!(error.as_ref(), McpError::Initialize { .. })),
        "fresh SDK setup fails, never a fabricated application response"
    );
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(state.executed.load(Ordering::SeqCst), 0);
    cleanup(&pool, &state).await;
}

#[tokio::test]
async fn close_cancels_a_held_repair_pong_and_cleans_up_the_acquired_candidate() {
    let (_server, state, pool) = start(true, Duration::from_secs(5)).await;
    let request_pool = pool.clone();
    let request = tokio::spawn(async move { invoke(&request_pool, "echo", context()).await });
    bounded(state.pong_seen.notified()).await;
    cleanup(&pool, &state).await;
    assert!(bounded(request).await.unwrap().is_err());
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(state.executed.load(Ordering::SeqCst), 0);
}
