//! Genuine GET response headers must remain local to their existing SDK transport.
use super::*;
use futures::StreamExt;
use rmcp::{ClientHandler, model::ProgressNotificationParam, service::NotificationContext};
use std::convert::Infallible;
use tokio::sync::{Notify, mpsc};

const AUTH: &str = "Bearer fixture-get-capture-token";
const BOUND: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
enum Kind {
    Common,
    Resumed,
}

struct Observed {
    kind: Kind,
    posts: Mutex<Vec<(String, Option<String>)>>,
    gets: Mutex<Vec<(Option<String>, Option<String>)>>,
    deletes: Mutex<Vec<Option<String>>>,
    application_id: Mutex<Option<Value>>,
    common_seen: Notify,
}

struct Progress(mpsc::UnboundedSender<Value>);
impl ClientHandler for Progress {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        let _ = self.0.send(serde_json::to_value(params).unwrap());
    }
}

fn wire_session(headers: &http::HeaderMap) -> Option<String> {
    assert_eq!(headers["authorization"], AUTH);
    headers
        .get("mcp-session-id")
        .map(|value| value.to_str().unwrap().to_owned())
}

fn progress(number: u8) -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"capture","progress":number}})
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("bounded GET capture operation")
}

async fn exercise(kind: Kind) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let observed = Arc::new(Observed {
        kind,
        posts: Mutex::new(Vec::new()),
        gets: Mutex::new(Vec::new()),
        deletes: Mutex::new(Vec::new()),
        application_id: Mutex::new(None),
        common_seen: Notify::new(),
    });
    let route = axum::routing::post(
        |axum::extract::State(observed): axum::extract::State<Arc<Observed>>,
         headers: http::HeaderMap, axum::Json(message): axum::Json<Value>| async move {
            let method = message["method"].as_str().unwrap();
            observed.posts.lock().unwrap().push((method.to_owned(), wire_session(&headers)));
            if method == "initialize" {
                return ([("mcp-session-id", "base")], axum::Json(json!({"jsonrpc":"2.0","id":message["id"],"result":{
                    "protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"get-capture","version":"1"}
                }}))).into_response();
            }
            assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
            match method {
                "notifications/initialized" => http::StatusCode::ACCEPTED.into_response(),
                "tools/call" => {
                    assert!(observed.application_id.lock().unwrap().replace(message["id"].clone()).is_none(), "original RPC must not replay");
                    ([("content-type","text/event-stream")], format!("id: first\nretry: 0\ndata: {}\n\n", progress(2))).into_response()
                }
                "ping" => axum::Json(json!({"jsonrpc":"2.0","id":message["id"],"result":{}})).into_response(),
                _ => panic!("unexpected POST"),
            }
        },
    ).get(|axum::extract::State(observed): axum::extract::State<Arc<Observed>>,
           headers: http::HeaderMap| async move {
        assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
        let cursor = headers.get("last-event-id").map(|value| value.to_str().unwrap().to_owned());
        observed.gets.lock().unwrap().push((wire_session(&headers), cursor.clone()));
        match cursor.as_deref() {
            None => {
                observed.common_seen.notify_one();
                if matches!(observed.kind, Kind::Resumed) {
                    // A rejected/unsupported opening must not adopt even a nonempty header.
                    return (http::StatusCode::METHOD_NOT_ALLOWED, [("mcp-session-id","ignored-405")]).into_response();
                }
                let body = futures::stream::iter([Ok::<_, Infallible>(format!("data: {}\n\n", progress(1)))])
                    .chain(futures::stream::pending());
                ([("content-type","text/event-stream"),("mcp-session-id","common-session")], axum::body::Body::from_stream(body)).into_response()
            }
            Some("first") => (
                [("content-type","text/event-stream"),("mcp-session-id","resume-session")],
                format!("id: second\nretry: 0\ndata: {}\n\n", progress(3)),
            ).into_response(),
            Some("second") => {
                let id = observed.application_id.lock().unwrap().clone().unwrap();
                ([("content-type","text/event-stream")], format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"served once"}]}}))).into_response()
            }
            _ => panic!("unexpected GET cursor"),
        }
    }).delete(|axum::extract::State(observed): axum::extract::State<Arc<Observed>>,
               headers: http::HeaderMap| async move {
        assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
        observed.deletes.lock().unwrap().push(wire_session(&headers));
        http::StatusCode::NO_CONTENT
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/mcp", route)
        .with_state(observed.clone());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let mut config = HttpTransportConfig::with_uri(uri);
    config
        .custom_headers
        .insert(http::header::AUTHORIZATION, AUTH.parse().unwrap());
    let (updates, mut received) = mpsc::unbounded_channel();
    let mut client = bounded(Progress(updates).serve_with_lifecycle(
        PiHttpTransport::with_client(PiHttpClient::new(), config),
        ClientLifecycleMode::Initialize,
    ))
    .await
    .unwrap();
    bounded(observed.common_seen.notified()).await;
    let expected = match kind {
        Kind::Common => {
            assert_eq!(
                bounded(received.recv()).await.unwrap()["progress"].as_f64(),
                Some(1.0)
            );
            "common-session"
        }
        Kind::Resumed => {
            let result = bounded(
                client.peer().send_request(
                    serde_json::from_value(
                        json!({"method":"tools/call","params":{"name":"echo","arguments":{}}}),
                    )
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap()["content"][0]["text"],
                "served once"
            );
            for value in [2.0, 3.0] {
                assert_eq!(
                    bounded(received.recv()).await.unwrap()["progress"].as_f64(),
                    Some(value)
                );
            }
            "resume-session"
        }
    };
    let result = bounded(
        client
            .peer()
            .send_request(serde_json::from_value(json!({"method":"ping"})).unwrap()),
    )
    .await
    .unwrap();
    assert!(matches!(result, ServerResult::EmptyResult(_)));
    client.close_with_timeout(BOUND).await.unwrap().unwrap();
    let mut posts = vec![
        ("initialize".to_owned(), None),
        (
            "notifications/initialized".to_owned(),
            Some("base".to_owned()),
        ),
    ];
    let mut gets = vec![(Some("base".to_owned()), None)];
    if matches!(kind, Kind::Resumed) {
        posts.push(("tools/call".to_owned(), Some("base".to_owned())));
        gets.push((Some("base".to_owned()), Some("first".to_owned())));
        gets.push((Some(expected.to_owned()), Some("second".to_owned())));
    }
    posts.push(("ping".to_owned(), Some(expected.to_owned())));
    assert_eq!(*observed.gets.lock().unwrap(), gets);
    assert_eq!(*observed.posts.lock().unwrap(), posts);
    assert_eq!(
        *observed.deletes.lock().unwrap(),
        [Some(expected.to_owned())]
    );
}

#[tokio::test]
async fn successful_common_get_header_updates_the_same_service_and_cleanup() {
    exercise(Kind::Common).await;
}

#[tokio::test]
async fn successful_resumed_get_header_updates_following_resume_without_rpc_replay() {
    exercise(Kind::Resumed).await;
}
