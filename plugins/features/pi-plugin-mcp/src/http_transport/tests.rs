mod controls;
mod dispatch;
mod get_session_capture;
mod json;
mod overflow;
mod protocol;
mod session_capture;
mod utf8;
use super::*;
use crate::http_client::PiHttpClient;
use axum::response::IntoResponse;
use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn setup_sse(response: bool) -> (Server, String) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |axum::Json(message): axum::Json<Value>| async move {
            if message["method"] != "initialize" {
                return http::StatusCode::ACCEPTED.into_response();
            }
            let mut body = String::new();
            for number in 0..64 {
                body += &format!(
                    "data: {}\n\n",
                    json!({"jsonrpc":"2.0","method":"notifications/progress",
                    "params":{"progressToken":0,"progress":number}})
                );
            }
            if response {
                body += &format!(
                    "data: {}\n\n",
                    json!({"jsonrpc":"2.0","id":message["id"],"result":{
                    "protocolVersion":"2025-03-26","capabilities":{},
                    "serverInfo":{"name":"stream-setup","version":"1"}}})
                );
            }
            ([("content-type", "text/event-stream")], body).into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    (server, uri)
}

#[tokio::test]
async fn setup_sse_notification_burst_does_not_block_the_send_ack() {
    let (_server, uri) = setup_sse(true).await;
    let mut config = HttpTransportConfig::with_uri(uri);
    config.channel_buffer_capacity = 1;
    let transport = PiHttpTransport::with_client(PiHttpClient::new(), config);
    let mut service = tokio::time::timeout(
        Duration::from_secs(2),
        ().serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
    )
    .await
    .expect("setup send/receive backpressure deadlock")
    .unwrap();
    service
        .close_with_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn setup_sse_without_a_reply_closes_instead_of_leaving_initialization_pending() {
    let (_server, uri) = setup_sse(false).await;
    let mut config = HttpTransportConfig::with_uri(uri);
    config.channel_buffer_capacity = 1;
    let transport = PiHttpTransport::with_client(PiHttpClient::new(), config);
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        ().serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
    )
    .await
    .expect("unterminated setup after SSE EOF");
    assert!(result.is_err());
}
