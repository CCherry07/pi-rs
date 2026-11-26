use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use ::http::{HeaderName, HeaderValue};
use axum::{Json, Router, extract::State, routing::post};
use futures::stream::BoxStream;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpPostResponse,
};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Capture {
    controls: Arc<Mutex<Vec<Option<RequestControl>>>>,
    expected_wire: Arc<Mutex<Vec<Value>>>,
    actual_wire: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone)]
struct RecordingClient {
    http: reqwest::Client,
    capture: Capture,
}

impl StreamableHttpClient for RecordingClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        if matches!(&message, ClientJsonRpcMessage::Request(request) if matches!(request.request, ClientRequest::CallToolRequest(_)))
        {
            let control = RequestControl::from_message(&message);
            self.capture.controls.lock().unwrap().push(control);
            let mut ordinary = message.clone();
            if let ClientJsonRpcMessage::Request(request) = &mut ordinary {
                request.request.extensions_mut().remove::<RequestControl>();
            }
            let expected = serde_json::to_value(ordinary).unwrap();
            assert_eq!(
                serde_json::to_value(&message).unwrap(),
                expected,
                "private control must not alter the SDK wire message"
            );
            self.capture.expected_wire.lock().unwrap().push(expected);
        }
        self.http
            .post_message(uri, message, session_id, auth_header, headers)
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.http
            .delete_session(uri, session_id, auth, headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<
        BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>,
        StreamableHttpError<Self::Error>,
    > {
        self.http
            .get_stream(uri, session_id, last, auth, headers)
            .await
    }
}

async fn endpoint(State(capture): State<Capture>, Json(message): Json<Value>) -> Json<Value> {
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => serde_json::to_value(
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_protocol_version(ProtocolVersion::V_2025_03_26),
        )
        .unwrap(),
        "tools/call" => {
            capture.actual_wire.lock().unwrap().push(message.clone());
            json!({"content":[{"type":"text","text":"ok"}]})
        }
        _ => json!({}),
    };
    Json(json!({"jsonrpc":"2.0","id":message["id"],"result":result}))
}

#[tokio::test]
async fn sdk_channel_preserves_private_control_without_changing_http_wire() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let capture = Capture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/mcp", post(endpoint))
        .with_state(capture.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let transport = StreamableHttpClientTransport::with_client(
        RecordingClient {
            http: reqwest::Client::new(),
            capture: capture.clone(),
        },
        StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp")),
    );
    let mut service = tokio::time::timeout(
        Duration::from_secs(3),
        ().serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
    )
    .await
    .unwrap()
    .unwrap();
    let guard = OperationGuard::new(pi_core::AbortHandle::new().1, None);
    let gate = Arc::new(DispatchGate::default());
    let control = RequestControl::new(guard.operation(), &gate);
    let mut request =
        ClientRequest::CallToolRequest(CallToolRequest::new(CallToolRequestParams::new("echo")));
    control.attach(&mut request);
    let handle = service
        .peer()
        .send_cancellable_request(request, Default::default())
        .await
        .unwrap();
    let request_id = handle.id.clone();
    tokio::time::timeout(Duration::from_secs(3), handle.await_response())
        .await
        .unwrap()
        .unwrap();
    let observed = capture.controls.lock().unwrap()[0]
        .clone()
        .expect("typed control must survive the real SDK service and transport worker channels");
    assert!(Arc::ptr_eq(&observed.operation.0, &control.operation.0));
    assert!(observed.try_recover().unwrap());
    assert!(
        !control.try_recover().unwrap(),
        "clones must share one allowance"
    );
    assert_eq!(
        *capture.expected_wire.lock().unwrap(),
        *capture.actual_wire.lock().unwrap()
    );
    assert_eq!(
        capture.actual_wire.lock().unwrap()[0]["id"],
        serde_json::to_value(request_id).unwrap()
    );
    drop(guard);
    assert_eq!(observed.check_dispatch(), Err(OperationError::Cancelled));
    service
        .close_with_timeout(Duration::from_secs(1))
        .await
        .unwrap();
    server.abort();
}

#[test]
fn completion_allows_only_supersession_without_rearming_dispatch_or_recovery() {
    let (abort, signal) = pi_core::AbortHandle::new();
    let guard = OperationGuard::new(signal, None);
    let gate = Arc::new(DispatchGate::default());
    let control = RequestControl::new(guard.operation(), &gate);
    assert_eq!(control.check_completion(), Ok(()));
    gate.supersede();
    assert_eq!(control.check_completion(), Ok(()));
    assert_eq!(control.check_dispatch(), Err(OperationError::TransportSuperseded));
    assert_eq!(control.try_recover(), Err(OperationError::TransportSuperseded));
    assert!(guard.operation().try_recover().unwrap(), "completion does not spend allowance");
    gate.retire();
    assert_eq!(control.check_completion(), Err(OperationError::AuthorityRetired));
    drop(gate);
    assert_eq!(control.check_completion(), Err(OperationError::AuthorityRetired));
    let gate = Arc::new(DispatchGate::default());
    let control = RequestControl::new(guard.operation(), &gate);
    gate.supersede();
    abort.abort();
    assert_eq!(control.check_completion(), Err(OperationError::Cancelled));
    let guard = OperationGuard::new(pi_core::AbortHandle::new().1, None);
    let control = RequestControl::new(guard.operation(), &gate);
    drop(guard);
    assert_eq!(control.check_completion(), Err(OperationError::Cancelled));
}

#[test]
fn replacement_never_rearms_old_dispatch_and_operation_drop_cancels_every_clone() {
    let guard = OperationGuard::new(pi_core::AbortHandle::new().1, None);
    let old_gate = Arc::new(DispatchGate::default());
    let old = RequestControl::new(guard.operation(), &old_gate);
    assert!(old.try_recover().unwrap());
    old_gate.supersede();
    let next_gate = Arc::new(DispatchGate::default());
    let next = RequestControl::new(guard.operation(), &next_gate);
    assert_eq!(
        old.check_dispatch(),
        Err(OperationError::TransportSuperseded)
    );
    assert_eq!(next.try_recover(), Ok(false));
    next_gate.retire();
    assert_eq!(next.check_dispatch(), Err(OperationError::AuthorityRetired));
    let typed: StreamableHttpError<reqwest::Error> = OperationError::InventoryChanged.into_http();
    let StreamableHttpError::Io(error) = typed else {
        panic!("private cause must remain typed")
    };
    assert_eq!(
        error.get_ref().unwrap().downcast_ref::<OperationError>(),
        Some(&OperationError::InventoryChanged)
    );
    drop(guard);
    assert_eq!(old.check_dispatch(), Err(OperationError::Cancelled));
}
