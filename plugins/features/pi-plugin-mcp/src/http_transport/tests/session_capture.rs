//! Successful non-request POST metadata is captured before ignoring the response body.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

const AUTH: &str = "Bearer fixture-capture-token";
const BOUND: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Observed {
    posts: Mutex<Vec<(String, Option<String>)>>,
    gets: Mutex<Vec<Option<String>>>,
    deletes: Mutex<Vec<Option<String>>>,
    notifications: AtomicUsize,
    common_seen: Notify,
}

fn session(headers: &http::HeaderMap) -> Option<String> {
    assert_eq!(headers["authorization"], AUTH);
    headers
        .get("mcp-session-id")
        .map(|value| value.to_str().unwrap().to_owned())
}

#[derive(Clone)]
struct CredentialWaitClient {
    inner: PiHttpClient,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl CredentialWaitClient {
    fn new() -> Self {
        Self {
            inner: PiHttpClient::new(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }

    async fn wait(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

impl crate::http_client::HttpClient for CredentialWaitClient {
    async fn post(
        &self,
        request: crate::http_client::PostRequest,
    ) -> Result<crate::http_client::PostResponse, HttpError> {
        self.wait().await;
        request.execute(&self.inner, None).await
    }

    async fn open_stream(
        &self,
        request: crate::http_client::StreamRequest,
    ) -> Result<crate::http_client::StreamResponse, HttpError> {
        self.wait().await;
        request.execute(&self.inner, None).await
    }
}

impl rmcp::transport::streamable_http_client::StreamableHttpClient for CredentialWaitClient {
    type Error = PiHttpClientError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<rmcp::transport::streamable_http_client::StreamableHttpPostResponse, HttpError>
    {
        self.inner
            .post_message(uri, message, session, token, headers)
            .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> Result<rmcp::transport::streamable_http_client::StreamableHttpPostResponse, HttpError>
    {
        self.inner
            .post_message_with_max_sse_event_size(uri, message, session, token, headers, limit)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        cursor: Option<String>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>,
        HttpError,
    > {
        self.inner
            .get_stream(uri, session, cursor, token, headers)
            .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        cursor: Option<String>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>,
        HttpError,
    > {
        self.inner
            .get_stream_with_max_sse_event_size(uri, session, cursor, token, headers, limit)
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        self.inner
            .delete_session(uri, session, token, headers)
            .await
    }
}

async fn fixture(app: axum::Router) -> (Server, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    (
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
        uri,
    )
}

#[tokio::test]
async fn accepted_notification_headers_update_session_before_followup_and_cleanup() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let observed = Arc::new(Observed::default());
    let route = axum::routing::post(
        |axum::extract::State(observed): axum::extract::State<Arc<Observed>>,
         headers: http::HeaderMap,
         axum::Json(message): axum::Json<Value>| async move {
            let method = message["method"].as_str().unwrap();
            observed
                .posts
                .lock()
                .unwrap()
                .push((method.to_owned(), session(&headers)));
            if method == "initialize" {
                return (
                    [("mcp-session-id", "initial")],
                    axum::Json(json!({
                        "jsonrpc":"2.0", "id":message["id"], "result":{
                            "protocolVersion":"2025-03-26", "capabilities":{},
                            "serverInfo":{"name":"capture-fixture","version":"1"}
                        }
                    })),
                )
                    .into_response();
            }
            assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
            if method == "ping" {
                return axum::Json(json!({"jsonrpc":"2.0","id":message["id"],"result":{}}))
                    .into_response();
            }
            let (status, returned) = match method {
                "notifications/initialized" => (http::StatusCode::ACCEPTED, Some("ready")),
                "notifications/roots/list_changed" => {
                    match observed.notifications.fetch_add(1, Ordering::SeqCst) {
                        0 => (http::StatusCode::OK, Some("control")),
                        1 => (http::StatusCode::NO_CONTENT, Some("")),
                        2 => (http::StatusCode::FORBIDDEN, Some("rejected")),
                        3 => (http::StatusCode::ACCEPTED, None),
                        _ => panic!("unexpected notification replay"),
                    }
                }
                _ => panic!("unexpected method"),
            };
            // All successful controls ignore this invalid body rather than decode it.
            let mut response =
                (status, [("content-type", "application/json")], "not JSON").into_response();
            if let Some(returned) = returned {
                response
                    .headers_mut()
                    .insert("mcp-session-id", returned.parse().unwrap());
            }
            response
        },
    )
    .get(
        |axum::extract::State(observed): axum::extract::State<Arc<Observed>>,
         headers: http::HeaderMap| async move {
            observed.gets.lock().unwrap().push(session(&headers));
            observed.common_seen.notify_one();
            http::StatusCode::METHOD_NOT_ALLOWED
        },
    )
    .delete(
        |axum::extract::State(observed): axum::extract::State<Arc<Observed>>,
         headers: http::HeaderMap| async move {
            assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
            observed.deletes.lock().unwrap().push(session(&headers));
            http::StatusCode::NO_CONTENT
        },
    );
    let (_server, uri) = fixture(
        axum::Router::new()
            .route("/mcp", route)
            .with_state(observed.clone()),
    )
    .await;
    let mut config = HttpTransportConfig::with_uri(uri);
    config
        .custom_headers
        .insert(http::header::AUTHORIZATION, AUTH.parse().unwrap());
    let mut client = tokio::time::timeout(
        BOUND,
        ().serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), config),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .expect("bounded setup")
    .unwrap();
    tokio::time::timeout(BOUND, observed.common_seen.notified())
        .await
        .expect("actual common GET");
    assert_eq!(*observed.gets.lock().unwrap(), [Some("ready".to_owned())]);
    for index in 0..5 {
        let reply = tokio::time::timeout(
            BOUND,
            client
                .peer()
                .send_request(serde_json::from_value(json!({"method":"ping"})).unwrap()),
        )
        .await
        .expect("bounded independent reply")
        .unwrap();
        assert!(matches!(reply, ServerResult::EmptyResult(_)));
        if index == 4 {
            break;
        }
        let result = tokio::time::timeout(
            BOUND,
            client.peer().send_notification(
                serde_json::from_value(json!({"method":"notifications/roots/list_changed"}))
                    .unwrap(),
            ),
        )
        .await
        .expect("bounded notification ACK");
        if index == 2 {
            let Err(rmcp::ServiceError::TransportSend(error)) = result else {
                panic!("original rejected-control error expected")
            };
            let Some(HttpError::Client(error)) =
                crate::http_client::transport_error(error.error.as_ref())
            else {
                panic!("typed HTTP rejection expected")
            };
            assert_eq!(error.status(), Some(http::StatusCode::FORBIDDEN));
        } else {
            result.unwrap();
        }
    }
    client.close_with_timeout(BOUND).await.unwrap().unwrap();
    let mut expected = vec![
        ("initialize".to_owned(), None),
        (
            "notifications/initialized".to_owned(),
            Some("initial".to_owned()),
        ),
    ];
    for index in 0..5 {
        let current = if index == 0 { "ready" } else { "control" };
        expected.push(("ping".to_owned(), Some(current.to_owned())));
        if index < 4 {
            expected.push((
                "notifications/roots/list_changed".to_owned(),
                Some(current.to_owned()),
            ));
        }
    }
    assert_eq!(*observed.posts.lock().unwrap(), expected);
    assert_eq!(*observed.gets.lock().unwrap(), [Some("ready".to_owned())]);
    assert_eq!(
        *observed.deletes.lock().unwrap(),
        [Some("control".to_owned())]
    );
}

#[tokio::test]
async fn successful_response_headers_are_captured_before_reply_validation_errors() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let route = axum::routing::post(
        |headers: http::HeaderMap, axum::Json(message): axum::Json<Value>| async move {
            assert_eq!(headers["authorization"], AUTH);
            let id = message["id"].as_str().unwrap();
            let mut response = match id {
                "missing" => http::StatusCode::NO_CONTENT.into_response(),
                "content" => {
                    ([("content-type", "text/plain")], "unsupported response").into_response()
                }
                "body" => ([("content-type", "application/json")], "{").into_response(),
                "protocol" => ([("content-type", "application/json")], "{}").into_response(),
                "repeat" => http::StatusCode::NO_CONTENT.into_response(),
                "rejected" => http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                _ => panic!("unexpected request"),
            };
            let returned = if id == "repeat" { "protocol" } else { id };
            response
                .headers_mut()
                .insert("mcp-session-id", returned.parse().unwrap());
            response
        },
    );
    let (_server, uri) = fixture(axum::Router::new().route("/mcp", route)).await;
    let mut config = HttpTransportConfig::with_uri(uri);
    config
        .custom_headers
        .insert(http::header::AUTHORIZATION, AUTH.parse().unwrap());
    let (incoming, _messages) = mpsc::channel(1);
    let (common, _changes) = watch::channel(None);
    let shared = Shared {
        client: PiHttpClient::new(),
        config,
        session: Arc::new(Mutex::new(Session {
            id: Some("previous".into()),
            ..Session::default()
        })),
        cleanup: Arc::new(Mutex::new(None)),
        common,
        incoming,
    };

    for id in ["missing", "content", "body"] {
        let session = shared.snapshot();
        let message = serde_json::from_value(json!({
            "jsonrpc":"2.0", "id":id, "method":"ping"
        }))
        .unwrap();
        assert!(
            tokio::time::timeout(BOUND, shared.post(message, &session))
                .await
                .expect("bounded invalid response")
                .is_err()
        );
        assert_eq!(shared.snapshot().id.as_deref(), Some(id));
        assert_eq!(shared.cleanup_snapshot().unwrap().id.as_deref(), Some(id));
    }

    let message = serde_json::from_value(json!({
        "jsonrpc":"2.0", "id":"protocol", "method":"ping"
    }))
    .unwrap();
    let mut responder = None;
    assert!(
        tokio::time::timeout(BOUND, shared.send(&message, &mut responder, None))
            .await
            .expect("bounded protocol response")
            .is_err()
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("protocol"));

    let session = shared.snapshot();
    let message = serde_json::from_value(json!({
        "jsonrpc":"2.0", "id":"repeat", "method":"ping"
    }))
    .unwrap();
    assert!(
        tokio::time::timeout(BOUND, shared.post(message, &session))
            .await
            .expect("bounded repeated session response")
            .is_err()
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("protocol"));

    let session = shared.snapshot();
    let message = serde_json::from_value(json!({
        "jsonrpc":"2.0", "id":"rejected", "method":"ping"
    }))
    .unwrap();
    assert!(
        tokio::time::timeout(BOUND, shared.post(message, &session))
            .await
            .expect("bounded rejected response")
            .is_err()
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("protocol"));
    assert_eq!(
        shared.cleanup_snapshot().unwrap().id.as_deref(),
        Some("protocol")
    );
}

#[tokio::test]
async fn credential_wait_uses_latest_session_and_get_errors_still_capture_headers() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    #[derive(Default)]
    struct Requests {
        post: Mutex<Option<(Option<String>, Option<String>)>>,
        get: Mutex<Option<(Option<String>, Option<String>)>>,
    }
    let requests = Arc::new(Requests::default());
    let route = axum::routing::post(
        |axum::extract::State(requests): axum::extract::State<Arc<Requests>>,
         headers: http::HeaderMap,
         axum::Json(_message): axum::Json<Value>| async move {
            *requests.post.lock().unwrap() = Some((
                session(&headers),
                headers
                    .get("mcp-protocol-version")
                    .map(|value| value.to_str().unwrap().to_owned()),
            ));
            (
                http::StatusCode::NO_CONTENT,
                [("mcp-session-id", "post-returned")],
            )
                .into_response()
        },
    )
    .get(
        |axum::extract::State(requests): axum::extract::State<Arc<Requests>>,
         headers: http::HeaderMap| async move {
            *requests.get.lock().unwrap() = Some((
                session(&headers),
                headers
                    .get("mcp-protocol-version")
                    .map(|value| value.to_str().unwrap().to_owned()),
            ));
            (
                [
                    ("mcp-session-id", "get-returned"),
                    ("content-type", "text/plain"),
                ],
                "unsupported",
            )
                .into_response()
        },
    );
    let (_server, uri) = fixture(
        axum::Router::new()
            .route("/mcp", route)
            .with_state(Arc::clone(&requests)),
    )
    .await;
    let mut config = HttpTransportConfig::with_uri(uri);
    config
        .custom_headers
        .insert(http::header::AUTHORIZATION, AUTH.parse().unwrap());
    let waiting = CredentialWaitClient::new();
    let (incoming, _messages) = mpsc::channel(1);
    let (common, _changes) = watch::channel(None);
    let shared = Arc::new(Shared {
        client: waiting.clone(),
        config,
        session: Arc::new(Mutex::new(Session {
            id: Some("post-before".into()),
            version: Some(ProtocolVersion::V_2024_11_05),
            ..Session::default()
        })),
        cleanup: Arc::new(Mutex::new(None)),
        common,
        incoming,
    });

    let stale_post = shared.snapshot();
    let post = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move {
            let message = serde_json::from_value(json!({
                "jsonrpc":"2.0", "id":"wait-post", "method":"ping"
            }))
            .unwrap();
            shared.post(message, &stale_post).await
        }
    });
    tokio::time::timeout(BOUND, waiting.entered.notified())
        .await
        .expect("credential wait entered for POST");
    {
        let mut current = shared
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.id = Some("post-latest".into());
        current.version = Some(ProtocolVersion::V_2026_07_28);
    }
    waiting.release.notify_one();
    assert!(
        tokio::time::timeout(BOUND, post)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        *requests.post.lock().unwrap(),
        Some((
            Some("post-latest".to_owned()),
            Some("2026-07-28".to_owned())
        ))
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("post-returned"));

    {
        let mut current = shared
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.id = Some("get-before".into());
        current.version = Some(ProtocolVersion::V_2025_03_26);
    }
    let stale_get = shared.snapshot();
    let get = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move { shared.common_stream(stale_get).await }
    });
    tokio::time::timeout(BOUND, waiting.entered.notified())
        .await
        .expect("credential wait entered for GET");
    shared
        .session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .id = Some("get-latest".into());
    waiting.release.notify_one();
    assert!(
        tokio::time::timeout(BOUND, get)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        *requests.get.lock().unwrap(),
        Some((Some("get-latest".to_owned()), Some("2025-03-26".to_owned())))
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("get-returned"));
    assert_eq!(
        shared.cleanup_snapshot().unwrap().id.as_deref(),
        Some("get-returned")
    );
}

#[tokio::test]
async fn delayed_old_body_cannot_overwrite_a_newer_response_header_capture() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    #[derive(Default)]
    struct Rotation {
        slow_returned: Notify,
        release_slow: Notify,
    }
    let rotation = Arc::new(Rotation::default());
    let route = axum::routing::post(
        |axum::extract::State(rotation): axum::extract::State<Arc<Rotation>>,
         headers: http::HeaderMap,
         axum::Json(message): axum::Json<Value>| async move {
            assert_eq!(headers["authorization"], AUTH);
            let id = message["id"].as_str().unwrap();
            if id == "fast" {
                return (
                    [("content-type", "text/plain"), ("mcp-session-id", "fast")],
                    "unsupported",
                )
                    .into_response();
            }
            assert_eq!(id, "slow");
            rotation.slow_returned.notify_one();
            let body = axum::body::Body::from_stream(futures::stream::once(async move {
                rotation.release_slow.notified().await;
                Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b"{"))
            }));
            (
                [
                    ("content-type", "application/json"),
                    ("mcp-session-id", "slow"),
                ],
                body,
            )
                .into_response()
        },
    );
    let (_server, uri) = fixture(
        axum::Router::new()
            .route("/mcp", route)
            .with_state(Arc::clone(&rotation)),
    )
    .await;
    let mut config = HttpTransportConfig::with_uri(uri);
    config
        .custom_headers
        .insert(http::header::AUTHORIZATION, AUTH.parse().unwrap());
    let (incoming, _messages) = mpsc::channel(1);
    let (common, _changes) = watch::channel(None);
    let shared = Arc::new(Shared {
        client: PiHttpClient::new(),
        config,
        session: Arc::new(Mutex::new(Session::default())),
        cleanup: Arc::new(Mutex::new(None)),
        common,
        incoming,
    });

    let slow = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move {
            let message = serde_json::from_value(json!({
                "jsonrpc":"2.0", "id":"slow", "method":"ping"
            }))
            .unwrap();
            shared.post(message, &shared.snapshot()).await
        }
    });
    tokio::time::timeout(BOUND, rotation.slow_returned.notified())
        .await
        .expect("slow response headers");
    tokio::time::timeout(BOUND, async {
        while shared.snapshot().id.as_deref() != Some("slow") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("slow session capture before body completion");

    let fast = serde_json::from_value(json!({
        "jsonrpc":"2.0", "id":"fast", "method":"ping"
    }))
    .unwrap();
    assert!(
        tokio::time::timeout(BOUND, shared.post(fast, &shared.snapshot()))
            .await
            .expect("bounded fast response")
            .is_err()
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("fast"));

    rotation.release_slow.notify_one();
    assert!(
        tokio::time::timeout(BOUND, slow)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(shared.snapshot().id.as_deref(), Some("fast"));
    assert_eq!(
        shared.cleanup_snapshot().unwrap().id.as_deref(),
        Some("fast")
    );
}

#[tokio::test]
async fn accepted_response_headers_survive_the_private_envelope_not_the_public_sdk_type() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for status in [
        http::StatusCode::OK,
        http::StatusCode::ACCEPTED,
        http::StatusCode::NO_CONTENT,
    ] {
        let route = axum::routing::post(
            move |headers: http::HeaderMap, axum::Json(message): axum::Json<Value>| async move {
                assert_eq!(session(&headers).as_deref(), Some("previous"));
                assert_eq!(
                    message,
                    json!({"jsonrpc":"2.0","id":"server-request","result":{}})
                );
                (
                    status,
                    [
                        ("mcp-session-id", "returned"),
                        ("content-type", "application/json"),
                    ],
                    "not JSON",
                )
                    .into_response()
            },
        );
        let (_server, uri) = fixture(axum::Router::new().route("/mcp", route)).await;
        let response = tokio::time::timeout(
            BOUND,
            PiHttpClient::new().post(PostRequest {
                uri: uri.into(),
                message: serde_json::from_value(
                    json!({"jsonrpc":"2.0","id":"server-request","result":{}}),
                )
                .unwrap(),
                session: Some("previous".into()),
                token: None,
                headers: HashMap::from([(http::header::AUTHORIZATION, AUTH.parse().unwrap())]),
                max_event_bytes: 1024,
                dispatch: None,
                session_observer: None,
            }),
        )
        .await
        .expect("bounded control response")
        .unwrap();
        assert_eq!(
            Shared::<PiHttpClient>::returned_session(&response),
            Some("returned")
        );
        assert!(matches!(
            response.into_sdk().unwrap(),
            rmcp::transport::streamable_http_client::StreamableHttpPostResponse::Accepted
        ));
    }
}
