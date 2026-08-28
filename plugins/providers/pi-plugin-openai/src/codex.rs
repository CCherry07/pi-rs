use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::responses::{input_items, stream as responses_stream, tools as response_tools};
use async_trait::async_trait;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use pi_core::{
    AbortSignal, Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderId,
    ProviderRequest, ProviderStream,
};
use pi_provider::{
    HttpBodyStream, HttpTransport, ReqwestTransport, TransportError, collect_body_limited,
    post_json_with_provider_hooks,
};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message as WebSocketMessage};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const PROVIDER_ID: &str = "openai-codex";
const API_NAME: &str = "openai-codex-responses";
const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_WEBSOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const SESSION_WEBSOCKET_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const SESSION_WEBSOCKET_MAX_AGE: Duration = Duration::from_secs(55 * 60);
const OPENAI_BETA_RESPONSES_WEBSOCKETS: &str = "responses_websockets=2026-02-06";

type CodexWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodexTransport {
    Sse,
    Websocket,
    WebsocketCached,
    #[default]
    Auto,
}

/// OpenAI Codex transport policy for one immutable runtime generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexTransportOptions {
    pub transport: CodexTransport,
    /// `None` disables the connect timeout. The default matches current Pi.
    pub websocket_connect_timeout: Option<Duration>,
    /// `None` disables the per-frame idle timeout. The default matches current Pi.
    pub websocket_idle_timeout: Option<Duration>,
    /// WebSocket proxy tunnelling is not provided by tokio-tungstenite. When an
    /// HTTP proxy is configured, Codex safely uses the proxied SSE path.
    pub http_proxy_configured: bool,
    /// Optional Codex base URL, primarily for compatible/self-hosted endpoints.
    pub base_url: Option<String>,
}

impl Default for CodexTransportOptions {
    fn default() -> Self {
        Self {
            transport: CodexTransport::Auto,
            websocket_connect_timeout: Some(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT),
            websocket_idle_timeout: Some(DEFAULT_WEBSOCKET_IDLE_TIMEOUT),
            http_proxy_configured: false,
            base_url: None,
        }
    }
}

struct CachedWebSocket {
    socket: CodexWebSocket,
    created_at: Instant,
    last_used_at: Instant,
    continuation: Option<WebSocketContinuation>,
}

#[derive(Clone)]
struct WebSocketContinuation {
    last_request_body: Value,
    last_response_id: String,
    last_response_items: Vec<Value>,
}

#[derive(Default)]
struct WebSocketSessionState {
    connections: tokio::sync::Mutex<HashMap<(String, String), CachedWebSocket>>,
    sse_fallback_sessions: Mutex<HashSet<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct CodexCredentials {
    access_token: Option<String>,
    account_id: Option<String>,
    source: Option<PathBuf>,
}

impl CodexCredentials {
    pub fn discover() -> Self {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Self::default();
        };
        Self::discover_from([
            home.join(".codex/auth.json"),
            home.join(".config/codex/auth.json"),
        ])
    }

    pub fn discover_from(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        paths
            .into_iter()
            .find_map(|path| Self::read(&path))
            .unwrap_or_default()
    }

    fn read(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() > 1024 * 1024 {
            return None;
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let access_token = token_candidate(&value, &["tokens", "access_token"])
            .or_else(|| token_candidate(&value, &["tokens", "accessToken"]))
            .or_else(|| token_candidate(&value, &["access_token"]))
            .or_else(|| token_candidate(&value, &["accessToken"]))?;
        let account_id = chatgpt_account_id(&access_token)?;
        Some(Self {
            access_token: Some(access_token),
            account_id: Some(account_id),
            source: Some(path.to_path_buf()),
        })
    }

    pub fn from_access_token(access_token: impl Into<String>) -> Self {
        let access_token = access_token.into();
        let account_id = chatgpt_account_id(&access_token);
        Self {
            access_token: Some(access_token),
            account_id,
            source: None,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.access_token.is_some() && self.account_id.is_some()
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

fn token_candidate(value: &Value, path: &[&str]) -> Option<String> {
    let token = path
        .iter()
        .try_fold(value, |value, key| value.get(*key))?
        .as_str()?
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .or_else(|| claims.get("chatgpt_account_id"))
        .or_else(|| claims.pointer("/https://api.openai.com/auth/chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub struct OpenAiCodexProvider {
    credentials: CodexCredentials,
    transport: Arc<dyn HttpTransport>,
    transport_options: CodexTransportOptions,
    websocket_sessions: Arc<WebSocketSessionState>,
}

impl OpenAiCodexProvider {
    pub fn discover() -> Self {
        Self::new(CodexCredentials::discover())
    }

    pub fn new(credentials: CodexCredentials) -> Self {
        Self::with_transport(credentials, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        credentials: CodexCredentials,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self::with_transport_options(credentials, transport, CodexTransportOptions::default())
    }

    pub fn with_transport_options(
        credentials: CodexCredentials,
        transport: Arc<dyn HttpTransport>,
        transport_options: CodexTransportOptions,
    ) -> Self {
        Self {
            credentials,
            transport,
            transport_options,
            websocket_sessions: Arc::new(WebSocketSessionState::default()),
        }
    }

    fn headers(
        &self,
        request: &ProviderRequest,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let token = self.credentials.access_token.as_deref().ok_or_else(|| {
            ProviderError::Failure(
                "OpenAI Codex requires a login in ~/.codex/auth.json".to_string(),
            )
        })?;
        let account_id = self.credentials.account_id.as_deref().ok_or_else(|| {
            ProviderError::Failure(
                "OpenAI Codex token is missing chatgpt_account_id; run codex login again"
                    .to_string(),
            )
        })?;
        let mut headers = BTreeMap::from([
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {token}")),
            ("chatgpt-account-id".to_string(), account_id.to_string()),
            (
                "OpenAI-Beta".to_string(),
                "responses=experimental".to_string(),
            ),
            ("originator".to_string(), "pi".to_string()),
            ("User-Agent".to_string(), "pi-rs".to_string()),
        ]);
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        Ok(headers)
    }

    fn websocket_headers(
        &self,
        request: &ProviderRequest,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let mut headers = self.headers(request)?;
        remove_header(&mut headers, "Accept");
        remove_header(&mut headers, "Content-Type");
        remove_header(&mut headers, "OpenAI-Beta");
        insert_header(
            &mut headers,
            "OpenAI-Beta",
            OPENAI_BETA_RESPONSES_WEBSOCKETS,
        );
        let request_id = request
            .session_id
            .clone()
            .unwrap_or_else(next_websocket_request_id);
        insert_header(&mut headers, "x-client-request-id", &request_id);
        insert_header(&mut headers, "session-id", &request_id);
        Ok(headers)
    }

    fn responses_url(&self) -> String {
        resolve_codex_responses_url(self.transport_options.base_url.as_deref())
    }

    fn should_attempt_websocket(&self, request: &ProviderRequest) -> bool {
        self.transport_options.transport != CodexTransport::Sse
            && !self.transport_options.http_proxy_configured
            && !request.session_id.as_deref().is_some_and(|session_id| {
                self.websocket_sessions
                    .sse_fallback_sessions
                    .lock()
                    .expect("Codex WebSocket fallback lock poisoned")
                    .contains(session_id)
            })
    }

    fn record_websocket_failure(&self, session_id: Option<&str>) {
        if let Some(session_id) = session_id {
            self.websocket_sessions
                .sse_fallback_sessions
                .lock()
                .expect("Codex WebSocket fallback lock poisoned")
                .insert(session_id.to_string());
        }
    }
}

#[async_trait]
impl Provider for OpenAiCodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn availability(&self) -> ProviderAvailability {
        if self.credentials.is_configured() {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let payload = context
            .before_provider_request(&signal, responses_request_body(&request))
            .await?;
        let responses_url = self.responses_url();

        if self.should_attempt_websocket(&request) {
            let headers = context
                .before_provider_headers(&signal, self.websocket_headers(&request)?)
                .await;
            let account_id = self
                .credentials
                .account_id
                .as_deref()
                .expect("configured Codex credentials have an account id");
            match open_websocket_body(WebSocketBodyRequest {
                state: Arc::clone(&self.websocket_sessions),
                options: self.transport_options.clone(),
                url: websocket_url(&responses_url)?,
                headers,
                payload: payload.clone(),
                session_id: request.session_id.clone(),
                account_id: account_id.to_string(),
                signal: signal.clone(),
            })
            .await
            {
                Ok(body) => {
                    return Ok(responses_stream(
                        ProviderId::new(PROVIDER_ID),
                        request.model,
                        API_NAME,
                        body,
                        signal,
                    ));
                }
                Err(TransportError::Aborted) => return Err(ProviderError::Aborted),
                Err(_) => self.record_websocket_failure(request.session_id.as_deref()),
            }
        }

        let headers = self.headers(&request)?;
        let response = post_json_with_provider_hooks(
            self.transport.as_ref(),
            &context,
            &responses_url,
            headers,
            &payload,
            signal.clone(),
        )
        .await
        .map_err(map_transport_error)?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!("HTTP {status}: {body}")));
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().contains("text/event-stream"))
        {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected text/event-stream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }

        Ok(responses_stream(
            ProviderId::new(PROVIDER_ID),
            request.model,
            API_NAME,
            response.body,
            signal,
        ))
    }
}

struct WebSocketBodyRequest {
    state: Arc<WebSocketSessionState>,
    options: CodexTransportOptions,
    url: String,
    headers: BTreeMap<String, String>,
    payload: Value,
    session_id: Option<String>,
    account_id: String,
    signal: AbortSignal,
}

struct AcquiredWebSocket {
    socket: CodexWebSocket,
    created_at: Instant,
    continuation: Option<WebSocketContinuation>,
}

struct WebSocketData {
    text: String,
    parsed: Option<Value>,
}

async fn open_websocket_body(
    request: WebSocketBodyRequest,
) -> Result<HttpBodyStream, TransportError> {
    let cache_key = request
        .session_id
        .as_ref()
        .map(|session_id| (session_id.clone(), request.account_id.clone()));
    let acquired = acquire_websocket(
        &request.state,
        cache_key.as_ref(),
        &request.url,
        &request.headers,
        &request.signal,
        request.options.websocket_connect_timeout,
    )
    .await?;
    let mut socket = acquired.socket;
    let use_cached_context = matches!(
        request.options.transport,
        CodexTransport::Auto | CodexTransport::WebsocketCached
    );
    let wire_payload = if use_cached_context {
        acquired.continuation.as_ref().map_or_else(
            || request.payload.clone(),
            |continuation| cached_websocket_request_body(&request.payload, continuation),
        )
    } else {
        request.payload.clone()
    };
    let envelope = websocket_request_envelope(wire_payload);
    let serialized = serde_json::to_string(&envelope).map_err(|error| {
        TransportError::InvalidConfiguration(format!(
            "failed to serialize Codex WebSocket request: {error}"
        ))
    })?;
    send_websocket_message(
        &mut socket,
        WebSocketMessage::Text(serialized.into()),
        &request.signal,
    )
    .await?;

    // Waiting for the first actual provider event here preserves Pi's fallback
    // boundary: connection/read failures before an event use SSE, while later
    // failures are surfaced to the normal assistant retry loop.
    let first = next_websocket_data(
        &mut socket,
        &request.signal,
        request.options.websocket_idle_timeout,
    )
    .await?;

    let state = request.state;
    let session_id = request.session_id;
    let signal = request.signal;
    let idle_timeout = request.options.websocket_idle_timeout;
    let full_payload = request.payload;
    let created_at = acquired.created_at;
    let body = async_stream::stream! {
        let mut next = Some(first);
        loop {
            let data = match next.take() {
                Some(data) => Ok(data),
                None => next_websocket_data(&mut socket, &signal, idle_timeout).await,
            };
            let data = match data {
                Ok(data) => data,
                Err(error) => {
                    if !matches!(error, TransportError::Aborted) {
                        record_sse_fallback(&state, session_id.as_deref());
                    }
                    yield Err(error);
                    return;
                }
            };
            let outcome = websocket_event_outcome(data.parsed.as_ref(), &full_payload);
            let bytes = format!("data: {}\n\n", data.text).into_bytes();
            match outcome {
                WebSocketEventOutcome::Continue => yield Ok(bytes),
                WebSocketEventOutcome::Failed => {
                    yield Ok(bytes);
                    return;
                }
                WebSocketEventOutcome::Completed(continuation) => {
                    if let Some(cache_key) = cache_key {
                        state.connections.lock().await.insert(
                            cache_key,
                            CachedWebSocket {
                                socket,
                                created_at,
                                last_used_at: Instant::now(),
                                continuation: use_cached_context.then_some(continuation).flatten(),
                            },
                        );
                    }
                    yield Ok(bytes);
                    return;
                }
            }
        }
    };
    Ok(Box::pin(body))
}

async fn acquire_websocket(
    state: &WebSocketSessionState,
    cache_key: Option<&(String, String)>,
    url: &str,
    headers: &BTreeMap<String, String>,
    signal: &AbortSignal,
    connect_timeout: Option<Duration>,
) -> Result<AcquiredWebSocket, TransportError> {
    if let Some(cache_key) = cache_key
        && let Some(cached) = state.connections.lock().await.remove(cache_key)
    {
        let now = Instant::now();
        if now.duration_since(cached.created_at) < SESSION_WEBSOCKET_MAX_AGE
            && now.duration_since(cached.last_used_at) < SESSION_WEBSOCKET_CACHE_TTL
        {
            return Ok(AcquiredWebSocket {
                socket: cached.socket,
                created_at: cached.created_at,
                continuation: cached.continuation,
            });
        }
    }

    Ok(AcquiredWebSocket {
        socket: connect_websocket(url, headers, signal, connect_timeout).await?,
        created_at: Instant::now(),
        continuation: None,
    })
}

async fn connect_websocket(
    url: &str,
    headers: &BTreeMap<String, String>,
    signal: &AbortSignal,
    connect_timeout: Option<Duration>,
) -> Result<CodexWebSocket, TransportError> {
    let mut request = url.into_client_request().map_err(|error| {
        TransportError::InvalidConfiguration(format!("invalid Codex WebSocket URL: {error}"))
    })?;
    for (name, value) in headers {
        if value.contains(['\r', '\n', '\0']) {
            return Err(TransportError::InvalidConfiguration(format!(
                "header {name:?} contains a forbidden control character"
            )));
        }
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            TransportError::InvalidConfiguration(format!("invalid header name: {error}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            TransportError::InvalidConfiguration(format!("invalid header value: {error}"))
        })?;
        request.headers_mut().insert(name, value);
    }

    let connect = connect_async(request);
    let result = match connect_timeout {
        Some(timeout) => tokio::select! {
            () = signal.wait() => return Err(TransportError::Aborted),
            result = tokio::time::timeout(timeout, connect) => result.map_err(|_| {
                TransportError::Request(format!(
                    "WebSocket connect timeout after {}ms",
                    timeout.as_millis()
                ))
            })?,
        },
        None => tokio::select! {
            () = signal.wait() => return Err(TransportError::Aborted),
            result = connect => result,
        },
    };
    result
        .map(|(socket, _)| socket)
        .map_err(map_websocket_error)
}

async fn send_websocket_message(
    socket: &mut CodexWebSocket,
    message: WebSocketMessage,
    signal: &AbortSignal,
) -> Result<(), TransportError> {
    tokio::select! {
        () = signal.wait() => Err(TransportError::Aborted),
        result = socket.send(message) => result.map_err(map_websocket_error),
    }
}

async fn next_websocket_data(
    socket: &mut CodexWebSocket,
    signal: &AbortSignal,
    idle_timeout: Option<Duration>,
) -> Result<WebSocketData, TransportError> {
    loop {
        let next = match idle_timeout {
            Some(timeout) => tokio::select! {
                () = signal.wait() => return Err(TransportError::Aborted),
                result = tokio::time::timeout(timeout, socket.next()) => result.map_err(|_| {
                    TransportError::Request(format!(
                        "WebSocket idle timeout after {}ms",
                        timeout.as_millis()
                    ))
                })?,
            },
            None => tokio::select! {
                () = signal.wait() => return Err(TransportError::Aborted),
                result = socket.next() => result,
            },
        };
        match next {
            Some(Ok(WebSocketMessage::Text(text))) => {
                let text = text.to_string();
                return Ok(WebSocketData {
                    parsed: serde_json::from_str(&text).ok(),
                    text,
                });
            }
            Some(Ok(WebSocketMessage::Binary(bytes))) => {
                let text = String::from_utf8(bytes.to_vec()).map_err(|error| {
                    TransportError::InvalidSse(format!(
                        "invalid Codex WebSocket UTF-8 payload: {error}"
                    ))
                })?;
                return Ok(WebSocketData {
                    parsed: serde_json::from_str(&text).ok(),
                    text,
                });
            }
            Some(Ok(WebSocketMessage::Ping(payload))) => {
                send_websocket_message(socket, WebSocketMessage::Pong(payload), signal).await?;
            }
            Some(Ok(WebSocketMessage::Pong(_))) | Some(Ok(WebSocketMessage::Frame(_))) => {}
            Some(Ok(WebSocketMessage::Close(frame))) => {
                let detail = frame.map_or_else(String::new, |frame| {
                    let reason = frame.reason.trim();
                    if reason.is_empty() {
                        format!(" {}", u16::from(frame.code))
                    } else {
                        format!(" {} {reason}", u16::from(frame.code))
                    }
                });
                return Err(TransportError::Request(format!(
                    "WebSocket closed before response.completed{detail}"
                )));
            }
            Some(Err(error)) => return Err(map_websocket_error(error)),
            None => {
                return Err(TransportError::Request(
                    "WebSocket stream closed before response.completed".to_string(),
                ));
            }
        }
    }
}

enum WebSocketEventOutcome {
    Continue,
    Completed(Option<WebSocketContinuation>),
    Failed,
}

fn websocket_event_outcome(event: Option<&Value>, request_body: &Value) -> WebSocketEventOutcome {
    let Some(event) = event else {
        return WebSocketEventOutcome::Continue;
    };
    match event.get("type").and_then(Value::as_str) {
        Some("response.completed" | "response.done" | "response.incomplete") => {
            let response = event.get("response").unwrap_or(&Value::Null);
            let continuation = response
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(|last_response_id| WebSocketContinuation {
                    last_request_body: request_body.clone(),
                    last_response_id: last_response_id.to_string(),
                    last_response_items: normalized_response_output(response),
                });
            WebSocketEventOutcome::Completed(continuation)
        }
        Some("response.failed" | "error") => WebSocketEventOutcome::Failed,
        _ => WebSocketEventOutcome::Continue,
    }
}

fn normalized_response_output(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let content = item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|content| {
                        content.get("type").and_then(Value::as_str) == Some("output_text")
                    })
                    .map(|content| {
                        json!({
                            "type": "output_text",
                            "text": content.get("text").and_then(Value::as_str).unwrap_or_default()
                        })
                    })
                    .collect::<Vec<_>>();
                (!content.is_empty())
                    .then(|| json!({"type": "message", "role": "assistant", "content": content}))
            }
            Some("function_call") => Some(json!({
                "type": "function_call",
                "call_id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or_default()
            })),
            _ => None,
        })
        .collect()
}

fn cached_websocket_request_body(body: &Value, continuation: &WebSocketContinuation) -> Value {
    if !request_bodies_match_except_input(body, &continuation.last_request_body) {
        return body.clone();
    }
    let Some(current_input) = body.get("input").and_then(Value::as_array) else {
        return body.clone();
    };
    let mut baseline = continuation
        .last_request_body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    baseline.extend(continuation.last_response_items.clone());
    if current_input.len() < baseline.len() || current_input[..baseline.len()] != baseline {
        return body.clone();
    }

    let mut body = body.clone();
    if let Value::Object(object) = &mut body {
        object.insert(
            "previous_response_id".to_string(),
            Value::String(continuation.last_response_id.clone()),
        );
        object.insert(
            "input".to_string(),
            Value::Array(current_input[baseline.len()..].to_vec()),
        );
    }
    body
}

fn request_bodies_match_except_input(left: &Value, right: &Value) -> bool {
    fn without_input(value: &Value) -> Value {
        let mut value = value.clone();
        if let Value::Object(object) = &mut value {
            object.remove("input");
            object.remove("previous_response_id");
        }
        value
    }
    without_input(left) == without_input(right)
}

fn websocket_request_envelope(payload: Value) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "type".to_string(),
        Value::String("response.create".to_string()),
    );
    if let Value::Object(payload) = payload {
        envelope.extend(payload);
    }
    Value::Object(envelope)
}

fn record_sse_fallback(state: &WebSocketSessionState, session_id: Option<&str>) {
    if let Some(session_id) = session_id {
        state
            .sse_fallback_sessions
            .lock()
            .expect("Codex WebSocket fallback lock poisoned")
            .insert(session_id.to_string());
    }
}

fn map_websocket_error(error: WebSocketError) -> TransportError {
    TransportError::Request(format!("WebSocket request failed: {error}"))
}

fn resolve_codex_responses_url(base_url: Option<&str>) -> String {
    let Some(base_url) = base_url
        .map(str::trim)
        .filter(|base_url| !base_url.is_empty())
    else {
        return CODEX_RESPONSES_URL.to_string();
    };
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

fn websocket_url(responses_url: &str) -> Result<String, ProviderError> {
    let mut url = reqwest::Url::parse(responses_url)
        .map_err(|error| ProviderError::Failure(format!("invalid Codex URL: {error}")))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        scheme => {
            return Err(ProviderError::Failure(format!(
                "Codex WebSocket requires an HTTP(S) base URL, got {scheme:?}"
            )));
        }
    };
    url.set_scheme(scheme)
        .map_err(|()| ProviderError::Failure("failed to build Codex WebSocket URL".to_string()))?;
    Ok(url.to_string())
}

fn next_websocket_request_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("pi-rs-{timestamp:x}-{sequence:x}")
}

pub(crate) fn responses_request_body(request: &ProviderRequest) -> Value {
    let input = input_items(&request.messages);
    let tools = response_tools(&request.tools);
    let effort = match request.thinking_level {
        pi_core::ThinkingLevel::Off => "low",
        level => level.as_str(),
    };
    let mut body = json!({
        "model": request.model.as_str(),
        "input": input,
        "instructions": request.system_prompt,
        "stream": true,
        "store": false,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "text": {"verbosity": "medium"},
        "include": ["reasoning.encrypted_content"],
        "reasoning": {"effort": effort, "summary": "auto"}
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Value::Object(body) = &mut body {
        body.extend(request.sampling_params.clone());
    }
    body
}

fn insert_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    remove_header(headers, name);
    headers.insert(name.to_string(), value.to_string());
}

fn remove_header(headers: &mut BTreeMap<String, String>, name: &str) {
    if let Some(existing) = headers
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        other => ProviderError::Failure(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use pi_core::{
        AssistantMessage, ContentBlock, Message, StopReason, StreamEvent, TextContent, Usage,
    };
    use pi_provider::HttpResponse;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    use tokio_tungstenite::{accept_async, accept_hdr_async};

    use super::*;

    fn jwt(account_id: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({"https://api.openai.com/auth": {"chatgpt_account_id": account_id}}).to_string(),
        );
        format!("header.{payload}.signature")
    }

    #[test]
    fn discovers_codex_cli_token_and_account_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        std::fs::write(
            &path,
            json!({"tokens": {"access_token": jwt("acct-1")}}).to_string(),
        )
        .unwrap();

        let credentials = CodexCredentials::discover_from([path.clone()]);

        assert!(credentials.is_configured());
        assert_eq!(credentials.account_id.as_deref(), Some("acct-1"));
        assert_eq!(credentials.source(), Some(path.as_path()));
    }

    #[test]
    fn builds_codex_responses_payload() {
        let request = ProviderRequest {
            model: pi_core::ModelId::new("gpt-5.5"),
            model_spec: None,
            system_prompt: "system".to_string(),
            messages: vec![Message::User(pi_core::UserMessage {
                content: vec![ContentBlock::Text(pi_core::TextContent::new("hello"))],
                timestamp_ms: 0,
            })],
            tools: Vec::new(),
            thinking_level: pi_core::ThinkingLevel::High,
            thinking_budgets: None,
            max_output_tokens: Some(123),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let body = responses_request_body(&request);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("max_output_tokens").is_none());
    }

    // The fixed tungstenite handshake callback contract owns a large HTTP
    // error response; this test only returns the supplied success response.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn cached_websocket_reuses_connection_and_sends_context_delta() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_hdr_async(stream, |request: &Request, response: Response| {
                assert_eq!(request.uri().path(), "/backend-api/codex/responses");
                assert_eq!(
                    request
                        .headers()
                        .get("openai-beta")
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    OPENAI_BETA_RESPONSES_WEBSOCKETS
                );
                assert_eq!(
                    request
                        .headers()
                        .get("session-id")
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    "session-1"
                );
                Ok(response)
            })
            .await
            .unwrap();
            let mut requests = Vec::new();
            for (index, text) in ["first", "second"].into_iter().enumerate() {
                let message = socket.next().await.unwrap().unwrap();
                let request: Value =
                    serde_json::from_str(message.into_text().unwrap().as_str()).unwrap();
                requests.push(request);
                socket
                    .send(WebSocketMessage::Text(
                        json!({
                            "type": "response.created",
                            "response": {"id": format!("response-{}", index + 1), "status": "in_progress"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WebSocketMessage::Text(
                        json!({"type": "response.output_text.delta", "delta": text})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WebSocketMessage::Text(
                        json!({
                            "type": "response.completed",
                            "response": {
                                "id": format!("response-{}", index + 1),
                                "status": "completed",
                                "output": [{
                                    "id": format!("message-{}", index + 1),
                                    "status": "completed",
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": text, "annotations": []}]
                                }],
                                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            requests
        });

        let provider = OpenAiCodexProvider::with_transport_options(
            CodexCredentials::from_access_token(jwt("acct-1")),
            Arc::new(ReqwestTransport::new()),
            CodexTransportOptions {
                transport: CodexTransport::WebsocketCached,
                websocket_connect_timeout: Some(Duration::from_secs(2)),
                websocket_idle_timeout: Some(Duration::from_secs(2)),
                base_url: Some(format!("http://{address}/backend-api")),
                ..CodexTransportOptions::default()
            },
        );
        let context = ProviderCallContext::without_plugins(
            "/workspace",
            ProviderId::new(PROVIDER_ID),
            pi_core::ModelId::new("gpt-5.5"),
        );

        let first_events = collect_provider_events(
            &provider,
            request_with_messages(vec![user_message("hello")]),
            context.clone(),
        )
        .await;
        assert!(first_events.iter().any(
            |event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "first")
        ));

        let second_events = collect_provider_events(
            &provider,
            request_with_messages(vec![
                user_message("hello"),
                assistant_message("first", "response-1"),
                user_message("next"),
            ]),
            context,
        )
        .await;
        assert!(second_events.iter().any(
            |event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "second")
        ));

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].get("previous_response_id").is_none());
        assert_eq!(requests[0]["input"].as_array().unwrap().len(), 1);
        assert_eq!(requests[1]["previous_response_id"], "response-1");
        assert_eq!(requests[1]["input"].as_array().unwrap().len(), 1);
        assert_eq!(requests[1]["input"][0]["role"], "user");
        assert_eq!(requests[1]["input"][0]["content"][0]["text"], "next");
    }

    struct SseFallbackTransport {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl HttpTransport for SseFallbackTransport {
        async fn post_json(
            &self,
            _url: &str,
            _headers: &BTreeMap<String, String>,
            _body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let body = concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"fallback\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"sse-1\",\"status\":\"completed\",\"usage\":{}}}\n\n"
            )
            .as_bytes()
            .to_vec();
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/event-stream".to_string()),
                headers: Vec::new(),
                body: Box::pin(futures::stream::once(async move { Ok(body) })),
            })
        }
    }

    #[tokio::test]
    async fn websocket_failure_before_first_event_falls_back_to_sse_for_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _request = socket.next().await.unwrap().unwrap();
            socket.close(None).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "the second request should honor the session's sticky SSE fallback"
            );
        });
        let transport = Arc::new(SseFallbackTransport {
            calls: AtomicUsize::new(0),
        });
        let provider = OpenAiCodexProvider::with_transport_options(
            CodexCredentials::from_access_token(jwt("acct-1")),
            transport.clone(),
            CodexTransportOptions {
                transport: CodexTransport::Websocket,
                websocket_connect_timeout: Some(Duration::from_secs(2)),
                websocket_idle_timeout: Some(Duration::from_secs(2)),
                base_url: Some(format!("http://{address}/backend-api")),
                ..CodexTransportOptions::default()
            },
        );
        let context = ProviderCallContext::without_plugins(
            "/workspace",
            ProviderId::new(PROVIDER_ID),
            pi_core::ModelId::new("gpt-5.5"),
        );

        for prompt in ["first", "second"] {
            let events = collect_provider_events(
                &provider,
                request_with_messages(vec![user_message(prompt)]),
                context.clone(),
            )
            .await;
            assert!(events.iter().any(
                |event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "fallback")
            ));
        }

        server.await.unwrap();
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    }

    async fn collect_provider_events(
        provider: &OpenAiCodexProvider,
        request: ProviderRequest,
        context: ProviderCallContext,
    ) -> Vec<StreamEvent> {
        let (_abort, signal) = pi_core::AbortHandle::new();
        let mut stream = provider.stream(request, context, signal).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        events
    }

    fn request_with_messages(messages: Vec<Message>) -> ProviderRequest {
        ProviderRequest {
            model: pi_core::ModelId::new("gpt-5.5"),
            model_spec: None,
            system_prompt: "system".to_string(),
            messages,
            tools: Vec::new(),
            thinking_level: pi_core::ThinkingLevel::High,
            thinking_budgets: None,
            max_output_tokens: None,
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: Some("session-1".to_string()),
        }
    }

    fn user_message(text: &str) -> Message {
        Message::User(pi_core::UserMessage {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            timestamp_ms: 0,
        })
    }

    fn assistant_message(text: &str, response_id: &str) -> Message {
        Message::Assistant(Arc::new(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            api: API_NAME.to_string(),
            provider: ProviderId::new(PROVIDER_ID),
            model: pi_core::ModelId::new("gpt-5.5"),
            response_model: None,
            response_id: Some(response_id.to_string()),
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: Some("completed".to_string()),
            end_turn: None,
            timestamp_ms: 0,
        }))
    }
}
