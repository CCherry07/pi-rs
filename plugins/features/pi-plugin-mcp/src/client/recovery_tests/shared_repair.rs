//! Migrated from the removed in-place worker: shared fresh setup, cancellation and failure.
use super::*;
use axum::response::IntoResponse;
use std::sync::{Mutex, atomic::AtomicUsize};
use tokio::sync::{Barrier, Notify, Semaphore};

const CHALLENGE: &str = "Bearer error=\"insufficient_scope\", scope=\"secret-marker\"";
struct State {
    initializes: AtomicUsize,
    rejected: Barrier,
    repairing: Notify,
    release: Semaphore,
    fail: bool,
    posts: Mutex<Vec<(String, String)>>,
}

async fn start(waiters: usize, fail: bool) -> (RepairServer, Arc<State>, McpToolSet) {
    let state = Arc::new(State {
        initializes: AtomicUsize::new(0),
        rejected: Barrier::new(waiters),
        repairing: Notify::new(),
        release: Semaphore::new(0),
        fail,
        posts: Mutex::new(Vec::new()),
    });
    let handler = state.clone();
    let route = axum::routing::post(move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
        let state = handler.clone();
        async move {
            let method = request["method"].as_str().unwrap();
            let session = headers.get("mcp-session-id").and_then(|value| value.to_str().ok()).unwrap_or("");
            state.posts.lock().unwrap().push((method.to_owned(), session.to_owned()));
            if method == "server/discover" {
                return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"legacy fixture"}})).into_response();
            }
            if method == "initialize" {
                let number = state.initializes.fetch_add(1, Ordering::SeqCst) + 1;
                if number >= 2 {
                    state.repairing.notify_one();
                    state.release.acquire().await.unwrap().forget();
                    if state.fail {
                        return (http::StatusCode::FORBIDDEN, [("www-authenticate", CHALLENGE)], "secret-marker").into_response();
                    }
                }
                return ([("mcp-session-id", format!("session-{number}"))], axum::Json(json!({
                    "jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-03-26",
                    "capabilities":{"tools":{}},"serverInfo":{"name":"shared-repair","version":"1"}}}))).into_response();
            }
            if method == "tools/list" {
                return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":repair_tools()}})).into_response();
            }
            if request["id"].is_null() { return http::StatusCode::ACCEPTED.into_response(); }
            assert_eq!(method, "tools/call");
            if session == "session-1" {
                state.rejected.wait().await;
                return http::StatusCode::NOT_FOUND.into_response();
            }
            axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"executed"}]}})).into_response()
        }
    }).get(|| async { http::StatusCode::METHOD_NOT_ALLOWED })
        .delete(|| async { http::StatusCode::NO_CONTENT });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = RepairServer(tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    }));
    // Default auth uses only an in-memory store; no credentials or discovery endpoints.
    let pool = McpToolSet::connect_with_auth(
        vec![McpServerConfig::http("fixture", uri)],
        McpClientAuth::default(),
    )
    .await
    .unwrap();
    (server, state, pool)
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("bounded repair operation")
}

fn failure(pool: &McpToolSet) -> Arc<McpError> {
    let state = pool.inner.servers[0]
        .binding
        .as_ref()
        .unwrap()
        .owned()
        .unwrap()
        .readiness();
    let crate::supervisor::ConnectionReadiness::Failed(error) = state else {
        panic!("failed owner required")
    };
    error
}

fn calls(state: &State) -> usize {
    state
        .posts
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "tools/call")
        .count()
}

#[tokio::test]
async fn concurrent_definitive_rejections_share_one_replacement() {
    let (_server, state, pool) = start(2, false).await;
    let first_pool = pool.clone();
    let first = tokio::spawn(async move { invoke(&first_pool, "echo", context()).await });
    let second_pool = pool.clone();
    let second = tokio::spawn(async move { invoke(&second_pool, "echo", context()).await });
    bounded(state.repairing.notified()).await;
    state.release.add_permits(8);
    bounded(first).await.unwrap().unwrap();
    bounded(second).await.unwrap().unwrap();
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(calls(&state), 4);
    bounded(pool.shutdown()).await.unwrap();
}

#[tokio::test]
async fn cancellation_of_the_initiating_waiter_does_not_abandon_session_repair() {
    let (_server, state, pool) = start(1, false).await;
    let (abort, signal) = pi_core::AbortHandle::new();
    let first_pool = pool.clone();
    let first = tokio::spawn(async move {
        invoke(
            &first_pool,
            "echo",
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
        )
        .await
    });
    bounded(state.repairing.notified()).await;
    abort.abort();
    assert!(matches!(
        bounded(first).await.unwrap(),
        Err(ToolError::Aborted)
    ));
    let next = invoke(&pool, "echo", context());
    tokio::pin!(next);
    assert!(futures::poll!(next.as_mut()).is_pending());
    state.release.add_permits(8);
    bounded(next).await.unwrap();
    assert_eq!(
        state.initializes.load(Ordering::SeqCst),
        2,
        "setup belongs to the exact owner, not its cancelled waiter"
    );
    assert_eq!(calls(&state), 2);
    assert!(
        !state
            .posts
            .lock()
            .unwrap()
            .iter()
            .any(|(method, session)| method == "notifications/cancelled" && session == "session-2"),
        "old controls cannot enter the replacement session"
    );
    bounded(pool.shutdown()).await.unwrap();
}

#[tokio::test]
async fn failed_repair_is_shared_as_typed_auth_evidence_without_duplicate_initialization() {
    let (_server, state, pool) = start(2, true).await;
    let first_pool = pool.clone();
    let first = tokio::spawn(async move { invoke(&first_pool, "echo", context()).await });
    let second_pool = pool.clone();
    let second = tokio::spawn(async move { invoke(&second_pool, "echo", context()).await });
    bounded(state.repairing.notified()).await;
    state.release.add_permits(8);
    for request in [first, second] {
        let error = bounded(request).await.unwrap().unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret-marker"));
    }
    // Fresh setup's typed product error belongs to the owner, not a fabricated reply on
    // the failed application's old SDK service. Lower HTTP/auth tests retain raw status.
    let error = failure(&pool);
    let McpError::AuthorizationRequired(required) = error.as_ref() else {
        panic!("typed setup auth evidence: {error:?}")
    };
    assert_eq!(required.challenge.as_deref(), Some(CHALLENGE));
    assert!(!required.static_authorization);
    assert!(!format!("{error:?} {error}").contains("secret-marker"));
    assert!(
        pool.connection_error("fixture")
            .unwrap()
            .contains("pi mcp login fixture")
    );
    assert!(bounded(invoke(&pool, "echo", context())).await.is_err());
    assert!(
        Arc::ptr_eq(&error, &failure(&pool)),
        "a later caller observes the same failed attempt"
    );
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(calls(&state), 2);
    bounded(pool.shutdown()).await.unwrap();
}

#[tokio::test]
async fn explicit_fresh_setup_retains_the_five_second_recovery_bound() {
    let (_server, state, pool) = start(1, false).await;
    let task_pool = pool.clone();
    let task = tokio::spawn(async move { invoke(&task_pool, "echo", context()).await });
    bounded(state.repairing.notified()).await;
    // New timing regression: the old three migrated cases retain their two-second bounds.
    let error = tokio::time::timeout(Duration::from_secs(6), task)
        .await
        .expect("five-second setup bound")
        .unwrap()
        .unwrap_err();
    assert!(!format!("{error:?}").contains("secret-marker"));
    assert!(matches!(failure(&pool).as_ref(), McpError::Timeout(name) if name == "fixture"));
    assert_eq!(state.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(
        calls(&state),
        1,
        "no application replay after setup timeout"
    );
    state.release.add_permits(8);
    bounded(pool.shutdown()).await.unwrap();
}
