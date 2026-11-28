use super::*;
use futures::StreamExt;
use std::convert::Infallible;
use tokio::sync::Notify;

#[tokio::test]
async fn setup_peer_ping_makes_progress_before_the_initialize_reply() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pong = Arc::new(Notify::new());
    let route = axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let pong = pong.clone();
            async move {
                if request["method"] == "initialize" {
                    assert!(headers.get("mcp-protocol-version").is_none());
                    let final_reply = format!(
                        "data: {}\n\n",
                        json!({"jsonrpc":"2.0","id":request["id"],
                    "result":{"protocolVersion":"2025-03-26","capabilities":{},
                    "serverInfo":{"name":"ping-setup","version":"1"}}})
                    );
                    let stream = futures::stream::iter([Ok::<_, Infallible>(
                    "data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\n\n".to_owned())])
                    .chain(futures::stream::once(async move { pong.notified().await; Ok(final_reply) }));
                    return (
                        [
                            ("content-type", "text/event-stream"),
                            ("mcp-session-id", "setup-session"),
                        ],
                        axum::body::Body::from_stream(stream),
                    )
                        .into_response();
                }
                if request["id"] == "server-ping" {
                    assert_eq!(headers["mcp-session-id"], "setup-session");
                    assert!(headers.get("mcp-protocol-version").is_none());
                    assert_eq!(request["result"], json!({}));
                    pong.notify_one();
                }
                http::StatusCode::ACCEPTED.into_response()
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
    let mut config = HttpTransportConfig::with_uri(uri);
    config.channel_buffer_capacity = 1;
    let mut client = tokio::time::timeout(
        Duration::from_secs(2),
        ().serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), config),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .expect("setup ping deadlock")
    .unwrap();
    client
        .close_with_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn bootstrap_ping_keeps_its_opening_stream_session_and_protocol_authority() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let route = axum::routing::post(
        |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| async move {
            assert_eq!(headers["mcp-session-id"], "opening-session");
            assert!(headers.get("mcp-protocol-version").is_none());
            assert_eq!(request["id"], "stream-ping");
            assert_eq!(request["result"], json!({}));
            http::StatusCode::ACCEPTED
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", route))
            .await
            .unwrap();
    }));
    let (incoming, _messages) = mpsc::channel(1);
    let (common, _changes) = watch::channel(None);
    let shared = Shared {
        client: PiHttpClient::new(),
        config: HttpTransportConfig::with_uri(uri),
        session: Arc::new(Mutex::new(Session {
            id: Some("newer-unrelated-session".into()),
            version: Some(ProtocolVersion::V_2026_07_28),
            ..Session::default()
        })),
        cleanup: Arc::new(Mutex::new(None)),
        common,
        incoming,
    };
    let opening = Session {
        id: Some("opening-session".into()),
        ..Session::default()
    };
    let ping = serde_json::from_value(json!({
        "jsonrpc":"2.0", "id":"stream-ping", "method":"ping"
    }))
    .unwrap();

    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            shared.bootstrap_ping(&ping, &opening)
        )
        .await
        .expect("bounded bootstrap Pong")
        .unwrap()
    );
    assert_eq!(
        shared.snapshot().id.as_deref(),
        Some("newer-unrelated-session")
    );
}

#[tokio::test]
async fn modern_tool_schema_validation_and_parameter_headers_reach_the_wire() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let wire = Arc::new(Mutex::new(None::<http::HeaderMap>));
    let observed = wire.clone();
    let route = axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let observed = observed.clone();
            async move {
                let result = match request["method"].as_str().unwrap() {
                    "server/discover" => serde_json::to_value(rmcp::model::DiscoverResult::new(
                        vec![ProtocolVersion::STANDARD_HEADERS],
                        serde_json::from_value(json!({"tools":{}})).unwrap(),
                    ))
                    .unwrap(),
                    "tools/list" => {
                        json!({"tools":[{"name":"echo","inputSchema":{"type":"object","properties":{
                    "tenant":{"type":"string","x-mcp-header":"Tenant"}}}},
                    {"name":"invalid","inputSchema":{"type":"object","properties":{
                    "tenant":{"type":"string","x-mcp-header":"bad name"}}}}]})
                    }
                    "tools/call" => {
                        *observed.lock().unwrap() = Some(headers);
                        json!({"content":[]})
                    }
                    method => panic!("unexpected modern method {method}"),
                };
                axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
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
    let mut client = ()
        .serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::STANDARD_HEADERS],
            },
        )
        .await
        .unwrap();
    let result = client
        .peer()
        .send_request(serde_json::from_value(json!({"method":"tools/list"})).unwrap())
        .await
        .unwrap();
    let ServerResult::ListToolsResult(result) = result else {
        panic!("tool listing expected")
    };
    assert_eq!(result.tools.len(), 1);
    assert_eq!(result.tools[0].name, "echo");
    client
        .peer()
        .send_request(
            serde_json::from_value(json!({"method":"tools/call","params":{
        "name":"echo","arguments":{"tenant":"日本語"}}}))
            .unwrap(),
        )
        .await
        .unwrap();
    let headers = wire.lock().unwrap().clone().unwrap();
    assert_eq!(headers["mcp-method"], "tools/call");
    assert_eq!(headers["mcp-name"], "echo");
    assert_eq!(headers["mcp-param-tenant"], "=?base64?5pel5pys6Kqe?=");
    client
        .close_with_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
}
