use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

struct PingHandler(Arc<AtomicUsize>);
impl rmcp::ClientHandler for PingHandler {
    async fn ping(
        &self,
        _context: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<(), rmcp::ErrorData> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(rmcp::ErrorData::new(
            rmcp::model::ErrorCode(-32091),
            "handler-owned-ping",
            None,
        ))
    }
}

#[tokio::test]
async fn ping_after_the_initialize_response_uses_the_sdk_handler_not_bootstrap() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let answered = Arc::new(Notify::new());
    let wire = answered.clone();
    let route = axum::routing::post(
        move |headers: http::HeaderMap, axum::Json(request): axum::Json<Value>| {
            let wire = wire.clone();
            async move {
                if request["method"] == "initialize" {
                    let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                    "protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"dispatch","version":"1"}}});
                    return ([("mcp-session-id", "dispatch-session"), ("content-type", "text/event-stream")],
                    format!("data: {reply}\n\ndata: {{\"jsonrpc\":\"2.0\",\"id\":\"after-setup\",\"method\":\"ping\"}}\n\n"))
                    .into_response();
                }
                if request["id"] == "after-setup" {
                    assert_eq!(headers["mcp-session-id"], "dispatch-session");
                    assert_eq!(request["error"]["code"], -32091);
                    assert!(
                        request.get("result").is_none(),
                        "bootstrap must not generate a second reply"
                    );
                    wire.notify_one();
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
    let calls = Arc::new(AtomicUsize::new(0));
    let mut client = tokio::time::timeout(
        Duration::from_secs(2),
        PingHandler(calls.clone()).serve_with_lifecycle(
            PiHttpTransport::with_client(PiHttpClient::new(), HttpTransportConfig::with_uri(uri)),
            ClientLifecycleMode::Initialize,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), answered.notified())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    client
        .close_with_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
}
