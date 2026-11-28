use super::*;
use axum::response::IntoResponse;
use futures::StreamExt;
use serde_json::json;
use std::error::Error;

fn install_test_crypto_provider() {
    // Set up each independently selected test without replacing an existing provider.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

struct Server {
    uri: Arc<str>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(
    status: StatusCode,
    challenge: Option<&'static str>,
    content_type: &'static str,
    body: String,
) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = Arc::from(format!("http://{}/mcp", listener.local_addr().unwrap()));
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::any(move || {
            let body = body.clone();
            async move {
                let mut response =
                    (status, [(header::CONTENT_TYPE, content_type)], body).into_response();
                if let Some(challenge) = challenge {
                    response.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        HeaderValue::from_static(challenge),
                    );
                }
                response
            }
        }),
    );
    Server {
        uri,
        task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    }
}

fn message(method: &str) -> ClientJsonRpcMessage {
    let mut value = json!({"jsonrpc":"2.0","id":1,"method":method});
    if method == "server/discover" {
        value["params"] = json!({});
    }
    serde_json::from_value(value).unwrap()
}

async fn post(
    server: &Server,
    method: &str,
    session: Option<&str>,
    headers: HashMap<HeaderName, HeaderValue>,
) -> Result<StreamableHttpPostResponse, HttpError> {
    PiHttpClient::new()
        .post_message(
            server.uri.clone(),
            message(method),
            session.map(Arc::from),
            None,
            headers,
        )
        .await
}

fn http_error(result: Result<StreamableHttpPostResponse, HttpError>) -> PiHttpClientError {
    match result {
        Err(StreamableHttpError::Client(error)) => error,
        _ => panic!("expected private HTTP error"),
    }
}

#[test]
fn owned_headers_replace_custom_values_but_absent_owned_values_preserve_them() {
    install_test_crypto_provider();
    let client = PiHttpClient::new();
    let custom = HashMap::from([
        (
            header::AUTHORIZATION,
            HeaderValue::from_static("custom-one, custom-two"),
        ),
        (header::ACCEPT, HeaderValue::from_static("wrong")),
        (header::CONTENT_TYPE, HeaderValue::from_static("wrong")),
        (
            HeaderName::from_static(SESSION_HEADER),
            HeaderValue::from_static("custom-session"),
        ),
        (
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static("2025-03-26"),
        ),
    ]);
    let (request, attached) = client
        .request(
            Method::POST,
            "http://localhost/mcp",
            Some("owned-session"),
            None,
            Some("owned-token"),
            custom.clone(),
        )
        .unwrap();
    assert!(attached);
    let request = request.build().unwrap();
    assert_eq!(
        request.headers()[header::AUTHORIZATION],
        "Bearer owned-token"
    );
    assert_eq!(request.headers()[SESSION_HEADER], "owned-session");
    assert_eq!(
        request.headers()[header::ACCEPT],
        "application/json, text/event-stream"
    );
    assert_eq!(request.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(request.headers()["mcp-protocol-version"], "2025-03-26");
    let (request, attached) = client
        .request(
            Method::GET,
            "http://localhost/mcp",
            None,
            None,
            Some(""),
            custom,
        )
        .unwrap();
    let request = request.build().unwrap();
    assert!(attached);
    assert_eq!(
        request.headers()[header::AUTHORIZATION],
        "custom-one, custom-two"
    );
    assert_eq!(request.headers()[SESSION_HEADER], "custom-session");
    assert_eq!(request.headers()[header::ACCEPT], "text/event-stream");
}

#[test]
fn empty_owned_session_preserves_custom_headers_for_every_method() {
    install_test_crypto_provider();
    let client = PiHttpClient::new();
    for method in [Method::POST, Method::GET, Method::DELETE] {
        for custom_session in [None, Some(""), Some("custom-session")] {
            let custom = custom_session
                .map(|session| {
                    (
                        HeaderName::from_bytes(b"Mcp-Session-Id").unwrap(),
                        HeaderValue::from_str(session).unwrap(),
                    )
                })
                .into_iter()
                .collect();
            let (request, attached) = client
                .request(
                    method.clone(),
                    "http://localhost/mcp",
                    Some(""),
                    None,
                    None,
                    custom,
                )
                .unwrap();
            let request = request.build().unwrap();
            assert_eq!(
                request
                    .headers()
                    .get(SESSION_HEADER)
                    .map(|value| value.to_str().unwrap()),
                custom_session
            );
            assert_eq!(attached, custom_session.is_some());
        }
    }
}

#[tokio::test]
async fn empty_owned_session_does_not_turn_custom_session_404_into_expiry() {
    install_test_crypto_provider();
    let server = server(StatusCode::NOT_FOUND, None, "application/json", "{}".into()).await;
    let client = PiHttpClient::new();
    for custom_session in [None, Some(""), Some("custom-session")] {
        let custom: HashMap<_, _> = custom_session
            .map(|session| {
                (
                    HeaderName::from_bytes(b"Mcp-Session-Id").unwrap(),
                    HeaderValue::from_str(session).unwrap(),
                )
            })
            .into_iter()
            .collect();
        let results = [
            client
                .post_message(
                    server.uri.clone(),
                    message("ping"),
                    Some(Arc::from("")),
                    None,
                    custom.clone(),
                )
                .await
                .map(|_| ()),
            client
                .get_stream(server.uri.clone(), Some(Arc::from("")), None, None, custom)
                .await
                .map(|_| ()),
        ];
        for result in results {
            let Err(StreamableHttpError::Client(error)) = result else {
                panic!("empty owned session must leave HTTP 404 ordinary")
            };
            assert_eq!(error.status(), Some(StatusCode::NOT_FOUND));
            assert!(error.discover_rejection().is_none());
        }
    }
}

#[test]
fn oauth_scope_observation_is_distinct_from_http_reaction_eligibility() {
    assert!(observed_insufficient_scope(
        "  DPoP error=\"insufficient_scope\""
    ));
    assert!(!observed_insufficient_scope(
        "Basic error=insufficient_scope"
    ));
    assert!(!observed_insufficient_scope(
        "Bearer error=INSUFFICIENT_SCOPE"
    ));
    assert!(!observed_insufficient_scope(
        "Bearer error=\"\", error=insufficient_scope"
    ));
    assert!(!observed_insufficient_scope(
        "Bearer x-error=insufficient_scope"
    ));
    assert!(scope_challenge("Basic error=insufficient_scope"));
    assert!(scope_challenge("Bearer error=INSUFFICIENT_SCOPE"));
    assert!(!scope_challenge("Bearer x-error=insufficient_scope"));
}

#[tokio::test]
async fn status_precedes_correlated_rpc_and_rejected_body_never_enters_diagnostics() {
    install_test_crypto_provider();
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::NOT_FOUND,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::NOT_IMPLEMENTED,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        for body in [
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"private-secret"}}"#,
            "private-secret invalid json",
        ] {
            let server = server(status, None, "application/json", body.into()).await;
            let error = http_error(post(&server, "ping", None, HashMap::new()).await);
            assert_eq!(error.status(), Some(status));
            assert!(error.auth_rejection().is_none());
            assert!(error.discover_rejection().is_none());
            assert!(error.network_flags().is_none());
            assert!(error.source().is_none());
            assert!(!format!("{error} {error:?}").contains("private-secret"));
        }
    }
}

#[tokio::test]
async fn raw_auth_matrix_has_no_provider_or_signin_claim_and_no_secret_sources() {
    install_test_crypto_provider();
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        for challenge in [
            None,
            Some("Bearer error=invalid_token, private=credential"),
            Some("Bearer error=insufficient_scope, scope=admin, private=credential"),
            Some("DPoP error=insufficient_scope"),
            Some("Basic error=insufficient_scope"),
            Some("Bearer error=INSUFFICIENT_SCOPE"),
            Some("Bearer error=\"\", error=insufficient_scope"),
        ] {
            let server = server(status, challenge, "application/json", "private-body".into()).await;
            for token in [None, Some("static-secret".to_owned())] {
                let error = http_error(
                    PiHttpClient::new()
                        .post_message(
                            server.uri.clone(),
                            message("ping"),
                            None,
                            token,
                            HashMap::new(),
                        )
                        .await,
                );
                assert_eq!(error.status(), Some(status));
                let eligible =
                    status == StatusCode::UNAUTHORIZED || challenge.is_some_and(scope_challenge);
                assert_eq!(error.auth_rejection().is_some(), eligible);
                if let Some(auth) = error.auth_rejection() {
                    assert_eq!(auth.challenge(), challenge);
                    assert!(!auth.has_auth_provider());
                    assert!(!auth.requires_sign_in());
                    assert_eq!(
                        auth.is_insufficient_scope(),
                        challenge.is_some_and(observed_insufficient_scope)
                    );
                    assert!(!format!("{auth:?}").contains("credential"));
                }
                assert!(error.source().is_none());
                assert!(!format!("{error} {error:?}").contains("private"));
            }
        }
    }
}

#[tokio::test]
async fn discover_fallback_requires_actual_discover_without_any_effective_session_header() {
    install_test_crypto_provider();
    for status in [StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND] {
        let server = server(status, None, "application/json", "{}".into()).await;
        let error = http_error(post(&server, "server/discover", None, HashMap::new()).await);
        assert_eq!(error.discover_rejection(), Some(status));
        // SDK Some("") is not the strictly unattached None required for this fallback,
        // even though it contributes no owned header and cannot establish session expiry.
        let error = http_error(post(&server, "server/discover", Some(""), HashMap::new()).await);
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.discover_rejection(), None);
        for session in ["", "custom-session"] {
            let custom = HashMap::from([(
                HeaderName::from_static(SESSION_HEADER),
                HeaderValue::from_str(session).unwrap(),
            )]);
            for owned in [None, Some("")] {
                let error =
                    http_error(post(&server, "server/discover", owned, custom.clone()).await);
                assert_eq!(error.discover_rejection(), None);
                assert_eq!(error.status(), Some(status));
            }
        }
        if status == StatusCode::NOT_FOUND {
            assert!(matches!(
                post(
                    &server,
                    "server/discover",
                    Some("owned-session"),
                    HashMap::new()
                )
                .await,
                Err(StreamableHttpError::SessionExpired)
            ));
        }
    }
}

#[tokio::test]
async fn successful_control_discards_body_but_requests_need_a_reply_and_bounded_json() {
    install_test_crypto_provider();
    for status in [StatusCode::OK, StatusCode::ACCEPTED, StatusCode::NO_CONTENT] {
        let server = server(status, None, "text/plain", "not an RPC response".into()).await;
        let notification: ClientJsonRpcMessage =
            serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .unwrap();
        assert!(matches!(
            PiHttpClient::new()
                .post_message(server.uri.clone(), notification, None, None, HashMap::new())
                .await,
            Ok(StreamableHttpPostResponse::Accepted)
        ));
        let error = http_error(post(&server, "ping", None, HashMap::new()).await);
        assert_eq!(
            error.is_missing_reply(),
            matches!(status, StatusCode::ACCEPTED | StatusCode::NO_CONTENT)
        );
    }
    let server = server(
        StatusCode::OK,
        None,
        "application/json; charset=utf-8",
        " ".repeat(MAX_JSON_BYTES + 1),
    )
    .await;
    let error = http_error(post(&server, "ping", None, HashMap::new()).await);
    assert!(error.to_string().contains("exceeded 16777216 bytes"));
    assert!(error.source().is_none());
}

struct CleanupBodyLifetime(Arc<tokio::sync::Semaphore>);

impl Drop for CleanupBodyLifetime {
    fn drop(&mut self) {
        self.0.add_permits(1);
    }
}

#[tokio::test]
async fn terminal_cleanup_discards_rejected_streaming_body_and_ignores_invalid_headers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    install_test_crypto_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri: Arc<str> = Arc::from(format!("http://{}/mcp", listener.local_addr().unwrap()));
    let requests = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(tokio::sync::Semaphore::new(0));
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::delete({
            let requests = requests.clone();
            let dropped = dropped.clone();
            move |headers: HeaderMap| {
                requests.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers[header::AUTHORIZATION], "static-cleanup-token");
                let lifetime = CleanupBodyLifetime(dropped.clone());
                async move {
                    let prefix = futures::stream::once(async {
                        Ok::<_, std::convert::Infallible>("cleanup-private".to_owned())
                    });
                    let tail = futures::stream::once(async move {
                        let _lifetime = lifetime;
                        std::future::pending::<Result<String, std::convert::Infallible>>().await
                    });
                    (
                        StatusCode::FORBIDDEN,
                        [(
                            header::WWW_AUTHENTICATE,
                            "Bearer error=insufficient_scope, scope=cleanup-private",
                        )],
                        axum::body::Body::from_stream(prefix.chain(tail)),
                    )
                        .into_response()
                }
            }
        }),
    );
    let server = Server {
        uri,
        task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    };
    let client = PiHttpClient::new();
    client
        .delete_session(
            server.uri.clone(),
            Arc::from("owned"),
            Some("invalid\nbearer".into()),
            HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "invalid cleanup headers fail closed and silently"
    );
    let custom = HashMap::from([(
        header::AUTHORIZATION,
        HeaderValue::from_static("static-cleanup-token"),
    )]);
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client.delete_session(server.uri.clone(), Arc::from("owned"), None, custom),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    tokio::time::timeout(std::time::Duration::from_secs(3), dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}

#[tokio::test]
async fn terminal_cleanup_absorbs_its_one_second_network_timeout() {
    install_test_crypto_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri: Arc<str> = Arc::from(format!("http://{}/mcp", listener.local_addr().unwrap()));
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::delete({
            let entered = entered.clone();
            move || {
                entered.add_permits(1);
                async { std::future::pending::<StatusCode>().await }
            }
        }),
    );
    let server = Server {
        uri,
        task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    };
    let start = std::time::Instant::now();
    let cleanup = tokio::spawn({
        let uri = server.uri.clone();
        async move {
            PiHttpClient::new()
                .delete_session(uri, Arc::from("owned"), None, HashMap::new())
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    tokio::time::timeout(std::time::Duration::from_secs(3), cleanup)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(start.elapsed() >= std::time::Duration::from_millis(750));
}

#[tokio::test]
async fn successful_json_and_post_get_sse_overflow_use_the_local_boundary() {
    install_test_crypto_provider();
    let json = server(
        StatusCode::OK,
        None,
        "application/json",
        "\u{feff}{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}".into(),
    )
    .await;
    assert!(matches!(
        post(&json, "ping", None, HashMap::new()).await,
        Ok(StreamableHttpPostResponse::Json(_, _))
    ));
    let server = server(
        StatusCode::OK,
        None,
        "text/event-stream",
        "data: 123456789012345678901234567890\n\n".into(),
    )
    .await;
    let client = PiHttpClient::new();
    let result = client
        .post_message_with_max_sse_event_size(
            server.uri.clone(),
            message("ping"),
            None,
            None,
            HashMap::new(),
            16,
        )
        .await
        .unwrap();
    let StreamableHttpPostResponse::Sse(mut stream, _) = result else {
        panic!("expected SSE response")
    };
    assert!(stream.next().await.unwrap().is_err());
    assert!(stream.next().await.is_none());
    let mut stream = client
        .get_stream_with_max_sse_event_size(
            server.uri.clone(),
            None,
            None,
            None,
            HashMap::new(),
            16,
        )
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_err());
    assert!(stream.next().await.is_none());
}
