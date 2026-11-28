//! Pi's HTTP boundary with private response envelopes and a stock SDK adapter.
//!
//! HTTP status precedes RPC decoding. This backend never refreshes, replays, negotiates,
//! or schedules. Ordered JSON member dispatch belongs to the sole private worker.
//! Remaining session-rotation races stay tracked separately from the implemented response capture.
mod error;
mod post;
mod sse;
mod stream;

pub(crate) use error::{AuthRejection, NetworkErrorFlags, PiHttpClientError};
pub(crate) use post::{JsonMessageIter, JsonMessages, PostRequest, PostResponse};
pub(crate) use stream::{HttpClient, HttpStream, StreamRequest, StreamResponse};

use std::collections::HashMap;
use std::sync::Arc;

use futures::stream::BoxStream;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, ProtocolVersion};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
    StreamableHttpPostResponse,
};

use crate::client::operation::RequestControl;

pub(crate) type HttpError = StreamableHttpError<PiHttpClientError>;

/// The sole HTTP worker shares the original typed failure among recovery waiters.
/// Accept direct backend errors too; never reconstruct status from formatted text.
pub(crate) fn transport_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a HttpError> {
    error
        .downcast_ref::<HttpError>()
        .or_else(|| error.downcast_ref::<Arc<HttpError>>().map(AsRef::as_ref))
}
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const SESSION_HEADER: &str = "mcp-session-id";

pub(crate) type DispatchProvider = Arc<dyn Fn() -> HttpDispatch + Send + Sync>;
pub(crate) type SessionObserver = Arc<dyn Fn(&str, Option<&ProtocolVersion>) + Send + Sync>;

pub(crate) struct HttpDispatch {
    pub(crate) session: Option<Arc<str>>,
    pub(crate) version: Option<ProtocolVersion>,
    pub(crate) headers: HashMap<HeaderName, HeaderValue>,
}

#[derive(Clone, Copy)]
enum RequestOrigin {
    DiscoverUnattached,
    Other,
}

#[derive(Clone, Default)]
pub(crate) struct PiHttpClient(reqwest::Client);

impl PiHttpClient {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn request(
        &self,
        method: Method,
        uri: &str,
        session: Option<&str>,
        last_event: Option<&str>,
        token: Option<&str>,
        custom: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(reqwest::RequestBuilder, bool), HttpError> {
        // Callers must fold case-duplicate input values before their order is lost.
        // This map is already normalized and may contain caller session/protocol headers.
        let mut headers: HeaderMap = custom.into_iter().collect();
        if method == Method::POST {
            headers.insert(
                header::ACCEPT,
                HeaderValue::from_static("application/json, text/event-stream"),
            );
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        } else if method == Method::GET {
            headers.insert(
                header::ACCEPT,
                HeaderValue::from_static("text/event-stream"),
            );
        }
        if let Some(session) = session.filter(|session| !session.is_empty()) {
            insert_header(&mut headers, SESSION_HEADER, session)?;
        }
        if let Some(last_event) = last_event {
            insert_header(&mut headers, "last-event-id", last_event)?;
        }
        // The SDK has already merged its owned protocol version into custom headers.
        if let Some(token) = token.filter(|token| !token.is_empty()) {
            insert_header(&mut headers, "authorization", &format!("Bearer {token}"))?;
        }
        let wire_session = headers.contains_key(SESSION_HEADER);
        Ok((self.0.request(method, uri).headers(headers), wire_session))
    }
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), HttpError> {
    let mut value = HeaderValue::from_str(value)
        .map_err(|_| client_error(PiHttpClientError::protocol("Invalid MCP HTTP header")))?;
    value.set_sensitive(true);
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

fn client_error(error: PiHttpClientError) -> HttpError {
    StreamableHttpError::Client(error)
}

fn network_error(error: reqwest::Error) -> HttpError {
    client_error(error.into())
}

/// Matches the frozen transport's case-insensitive insufficient_scope eligibility gate.
/// The OAuth challenge's error field is parsed separately below; this never parses error text.
fn scope_challenge(header: &str) -> bool {
    let lower = header.to_ascii_lowercase();
    lower.match_indices("error=").any(|(offset, _)| {
        let boundary =
            offset == 0 || lower[..offset].ends_with(|c: char| c.is_whitespace() || c == ',');
        let value = lower[offset + "error=".len()..]
            .strip_prefix('"')
            .unwrap_or(&lower[offset + "error=".len()..]);
        boundary && value.starts_with("insufficient_scope")
    })
}

// The frozen OAuth callback accepts only Bearer/DPoP and compares the first exact error
// field case-sensitively. SDK metadata/authorization handling remains outside this predicate.
fn observed_insufficient_scope(header: &str) -> bool {
    let scheme = header.split_whitespace().next().unwrap_or_default();
    if !scheme.eq_ignore_ascii_case("bearer") && !scheme.eq_ignore_ascii_case("dpop") {
        return false;
    }
    let lower = header.to_ascii_lowercase();
    for (offset, _) in lower.match_indices("error=") {
        if offset != 0 && !header[..offset].ends_with(|c: char| c.is_whitespace() || c == ',') {
            continue;
        }
        let value = &header[offset + "error=".len()..];
        if let Some(quoted) = value.strip_prefix('"')
            && let Some(end) = quoted.find('"')
        {
            return &quoted[..end] == "insufficient_scope";
        }
        let end = value
            .find(|c: char| c.is_whitespace() || c == ',')
            .unwrap_or(value.len());
        if end > 0 {
            return &value[..end] == "insufficient_scope";
        }
    }
    false
}

fn check_status(
    response: &reqwest::Response,
    sdk_session: bool,
    origin: RequestOrigin,
) -> Result<(), HttpError> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    if status == StatusCode::UNAUTHORIZED
        || (status == StatusCode::FORBIDDEN && challenge.as_deref().is_some_and(scope_challenge))
    {
        let insufficient_scope = challenge
            .as_deref()
            .is_some_and(observed_insufficient_scope);
        return Err(client_error(PiHttpClientError::auth(
            status,
            challenge,
            insufficient_scope,
        )));
    }
    if status == StatusCode::NOT_FOUND && sdk_session {
        return Err(StreamableHttpError::SessionExpired);
    }
    // Do not retain or parse rejected bodies, even if they contain a correlated RPC reply.
    // Dropping the response also prevents an unbounded diagnostic-body read.
    Err(client_error(PiHttpClientError::response(status, origin)))
}

fn content_type(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
}

async fn json_body(mut response: reqwest::Response) -> Result<serde_json::Value, HttpError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if chunk.len() > MAX_JSON_BYTES.saturating_sub(bytes.len()) {
            return Err(client_error(PiHttpClientError::limit(
                "JSON response",
                MAX_JSON_BYTES,
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    // TextDecoder in the frozen oracle accepts a BOM and replaces malformed UTF-8.
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(&text)
        .map_err(|_| client_error(PiHttpClientError::protocol("Invalid MCP JSON response")))
}

impl StreamableHttpClient for PiHttpClient {
    type Error = PiHttpClientError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_token,
            custom_headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        PostRequest {
            uri,
            message,
            session: session_id,
            token: auth_token.clone(),
            headers: custom_headers,
            max_event_bytes: max_sse_event_size,
            dispatch: None,
            session_observer: None,
        }
        .execute(self, auth_token)
        .await?
        .into_sdk()
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>, HttpError> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_token,
            custom_headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>, HttpError> {
        // Keep the public SDK stream-only contract; the product worker retains metadata.
        StreamRequest {
            uri,
            session: session_id,
            cursor: last_event_id,
            token: auth_token.clone(),
            headers: custom_headers,
            max_event_bytes: max_sse_event_size,
            control: None,
            dispatch: None,
            session_observer: None,
        }
        .execute(self, auth_token)
        .await
        .map(|response| response.body)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        // Terminal cleanup is deliberately not an application response: no classification,
        // body read, authentication reaction, or error propagation into SDK cleanup logging.
        let Ok((request, _)) = self.request(
            Method::DELETE,
            &uri,
            Some(&session_id),
            None,
            auth_token.as_deref(),
            custom_headers,
        ) else {
            return Ok(());
        };
        // Dropping either outcome discards the body/error; never wait for its contents.
        let _ = request
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
