//! Frozen consumeSseStream's nonfatal UTF-8 decoding must reach real SDK dispatch.
use super::*;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};

const SESSION: &str = "utf8-session";
const AUTH: &str = "Bearer fixture-utf8-token";
const BOUND: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Observed {
    initializes: AtomicUsize,
    calls: Mutex<Vec<String>>,
    deletes: AtomicUsize,
}

#[tokio::test]
async fn malformed_utf8_tool_reply_reaches_the_sdk_without_replay() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state = Arc::new(Observed::default());
    let route = axum::routing::post(
        |axum::extract::State(state): axum::extract::State<Arc<Observed>>,
         headers: http::HeaderMap,
         axum::Json(request): axum::Json<Value>| async move {
            assert_eq!(headers["authorization"], AUTH);
            if request["method"] == "initialize" {
                state.initializes.fetch_add(1, Ordering::SeqCst);
                return (
                    [("mcp-session-id", SESSION)],
                    axum::Json(json!({
                        "jsonrpc":"2.0", "id":request["id"], "result":{
                            "protocolVersion":"2025-03-26", "capabilities":{"tools":{}},
                            "serverInfo":{"name":"utf8-fixture","version":"1"}
                        }
                    })),
                )
                    .into_response();
            }
            assert_eq!(headers["mcp-session-id"], SESSION);
            assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
            if request["method"] == "notifications/initialized" {
                return http::StatusCode::ACCEPTED.into_response();
            }
            assert_eq!(request["method"], "tools/call");
            let name = request["params"]["name"].as_str().unwrap();
            state.calls.lock().unwrap().push(name.to_owned());
            let result = json!({"jsonrpc":"2.0", "id":request["id"], "result":{
                "content":[{"type":"text", "text":if name == "malformed" {
                    "before:BAD_UTF8:日本"
                } else { "independent success" }}]
            }});
            if name != "malformed" {
                return axum::Json(result).into_response();
            }
            let wire = format!("data: {result}\n\n");
            let (prefix, suffix) = wire.split_once("BAD_UTF8").unwrap();
            let bytes = [prefix.as_bytes(), b"\xff", suffix.as_bytes()].concat();
            let chunks = futures::stream::iter(
                bytes
                    .into_iter()
                    .map(|byte| Ok::<_, Infallible>(axum::body::Bytes::from(vec![byte]))),
            );
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(chunks),
            )
                .into_response()
        },
    )
    .get(|| async { http::StatusCode::METHOD_NOT_ALLOWED })
    .delete(
        |axum::extract::State(state): axum::extract::State<Arc<Observed>>,
         headers: http::HeaderMap| async move {
            assert_eq!(headers["authorization"], AUTH);
            assert_eq!(headers["mcp-session-id"], SESSION);
            state.deletes.fetch_add(1, Ordering::SeqCst);
            http::StatusCode::NO_CONTENT
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/mcp", route)
        .with_state(state.clone());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
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
    for (name, expected) in [
        ("malformed", "before:�:日本"),
        ("independent", "independent success"),
    ] {
        let reply = tokio::time::timeout(
            BOUND,
            client.peer().send_request(
                serde_json::from_value(
                    json!({"method":"tools/call", "params":{"name":name,"arguments":{}}}),
                )
                .unwrap(),
            ),
        )
        .await
        .expect("bounded SDK reply")
        .expect("lossy decoding must preserve the actual reply");
        let ServerResult::CallToolResult(result) = reply else {
            panic!("tool reply expected")
        };
        assert_eq!(
            serde_json::to_value(result).unwrap()["content"][0]["text"],
            expected
        );
    }
    client.close_with_timeout(BOUND).await.unwrap().unwrap();
    assert_eq!(state.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(*state.calls.lock().unwrap(), ["malformed", "independent"]);
    assert_eq!(state.deletes.load(Ordering::SeqCst), 1);
}
