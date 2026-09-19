use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use async_stream::stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use pi_core::AbortSignal;
use pi_plugin::{ProviderCallContext, ProviderError};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

const DEFAULT_REMOTE_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_LOCAL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_REQUEST_HEADERS: usize = 100;
pub const REQUEST_TIMEOUT_ENV: &str = "PI_HTTP_REQUEST_TIMEOUT_SECS";

pub type HttpBodyStream =
    Pin<Box<dyn Stream<Item = Result<Vec<u8>, TransportError>> + Send + 'static>>;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("request aborted")]
    Aborted,
    #[error("request timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("invalid HTTP configuration: {0}")]
    InvalidConfiguration(String),
    #[error("HTTP request failed: {0}")]
    Request(String),
    #[error("response body exceeds the {limit}-byte limit")]
    BodyTooLarge { limit: usize },
    #[error("invalid SSE stream: {0}")]
    InvalidSse(String),
}

impl TransportError {
    /// Converts shared HTTP/SSE failures into the provider error contract
    /// without making each provider flatten or reclassify them independently.
    pub fn into_provider_error(self) -> ProviderError {
        match self {
            Self::Aborted => ProviderError::Aborted,
            Self::InvalidConfiguration(message) | Self::InvalidSse(message) => {
                ProviderError::Protocol(message)
            }
            Self::Timeout { seconds } => {
                ProviderError::Failure(format!("request timed out after {seconds}s"))
            }
            Self::Request(message) => ProviderError::Failure(message),
            Self::BodyTooLarge { limit } => {
                ProviderError::Failure(format!("response body exceeds the {limit}-byte limit"))
            }
        }
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: HttpBodyStream,
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &Value,
        signal: AbortSignal,
    ) -> Result<HttpResponse, TransportError>;
}

/// Executes one JSON HTTP request through the provider wire-hook lifecycle.
///
/// Providers hand this function their fully assembled headers and payload. It
/// guarantees that header hooks run immediately before transport and response
/// observers run before any caller can consume the returned body stream.
pub async fn post_json_with_provider_hooks(
    transport: &dyn HttpTransport,
    context: &ProviderCallContext,
    url: &str,
    headers: BTreeMap<String, String>,
    body: &Value,
    signal: AbortSignal,
) -> Result<HttpResponse, TransportError> {
    let headers = context.before_provider_headers(&signal, headers).await;
    let response = transport
        .post_json(url, &headers, body, signal.clone())
        .await?;
    context
        .after_provider_response(
            &signal,
            response.status,
            response.headers.iter().cloned().collect(),
        )
        .await;
    Ok(response)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReqwestTransportConfig {
    /// Overrides provider-aware defaults. `Some(Duration::ZERO)` disables timeouts.
    pub timeout: Option<Duration>,
    pub user_agent: Option<String>,
    /// Explicit proxy used for both HTTP and HTTPS requests. When absent,
    /// reqwest's normal environment proxy discovery remains active.
    pub proxy: Option<String>,
    /// Fresh request attempts after a retryable transport or HTTP failure.
    pub max_retries: u32,
    /// Maximum provider-requested retry delay. Zero disables the limit.
    pub max_retry_delay: Duration,
}

impl Default for ReqwestTransportConfig {
    fn default() -> Self {
        Self {
            timeout: None,
            user_agent: None,
            proxy: None,
            max_retries: 0,
            max_retry_delay: Duration::from_secs(60),
        }
    }
}

#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    config: ReqwestTransportConfig,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self::with_config(ReqwestTransportConfig::default())
            .expect("default reqwest transport configuration must be valid")
    }

    pub fn with_config(config: ReqwestTransportConfig) -> Result<Self, TransportError> {
        let mut builder = reqwest::Client::builder();
        if let Some(user_agent) = &config.user_agent {
            builder = builder.user_agent(user_agent);
        }
        if let Some(proxy) = config
            .proxy
            .as_deref()
            .map(str::trim)
            .filter(|proxy| !proxy.is_empty())
        {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|error| {
                TransportError::InvalidConfiguration(format!("invalid HTTP proxy: {error}"))
            })?);
        }
        let client = builder
            .build()
            .map_err(|error| TransportError::InvalidConfiguration(error.to_string()))?;
        Ok(Self { client, config })
    }

    fn timeout_for(&self, url: &str) -> Option<Duration> {
        if let Some(timeout) = self.config.timeout {
            return (!timeout.is_zero()).then_some(timeout);
        }
        if let Ok(value) = std::env::var(REQUEST_TIMEOUT_ENV)
            && let Ok(seconds) = value.trim().parse::<u64>()
        {
            return (seconds != 0).then(|| Duration::from_secs(seconds));
        }
        Some(if url_is_local(url) {
            DEFAULT_LOCAL_TIMEOUT
        } else {
            DEFAULT_REMOTE_TIMEOUT
        })
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &Value,
        signal: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        if headers.len() > MAX_REQUEST_HEADERS {
            return Err(TransportError::InvalidConfiguration(format!(
                "request exceeds the {MAX_REQUEST_HEADERS}-header limit"
            )));
        }
        let mut request_headers = HeaderMap::new();
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
            request_headers.insert(name, value);
        }

        let mut retry_index = 0_u32;
        let response = loop {
            let send = self
                .client
                .post(url)
                .headers(request_headers.clone())
                .json(body)
                .send();
            let result = if let Some(timeout) = self.timeout_for(url) {
                tokio::select! {
                    _ = signal.wait() => return Err(TransportError::Aborted),
                    result = tokio::time::timeout(timeout, send) => match result {
                        Ok(result) => result.map_err(reqwest_request_error),
                        Err(_) => Err(TransportError::Timeout { seconds: timeout.as_secs() }),
                    }
                }
            } else {
                tokio::select! {
                    _ = signal.wait() => return Err(TransportError::Aborted),
                    result = send => result.map_err(reqwest_request_error),
                }
            };
            match result {
                Ok(response)
                    if retry_index < self.config.max_retries
                        && response_is_retryable(
                            response.status().as_u16(),
                            response.headers(),
                        ) =>
                {
                    let delay =
                        retry_delay(response.headers(), retry_index, self.config.max_retry_delay)?;
                    retry_index = retry_index.saturating_add(1);
                    wait_for_retry(delay, &signal).await?;
                }
                Ok(response) => break response,
                Err(error)
                    if retry_index < self.config.max_retries
                        && !matches!(error, TransportError::Aborted) =>
                {
                    let delay = exponential_retry_delay(retry_index);
                    retry_index = retry_index.saturating_add(1);
                    wait_for_retry(delay, &signal).await?;
                }
                Err(error) => return Err(error),
            }
        };

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let response_headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect();
        let mut bytes = response.bytes_stream();
        let body_timeout = self.timeout_for(url);
        let body_signal = signal;
        let output = stream! {
            loop {
                let next = match body_timeout {
                    Some(timeout) => tokio::select! {
                        _ = body_signal.wait() => {
                            yield Err(TransportError::Aborted);
                            return;
                        }
                        result = tokio::time::timeout(timeout, bytes.next()) => match result {
                            Ok(next) => next,
                            Err(_) => {
                                yield Err(TransportError::Timeout { seconds: timeout.as_secs() });
                                return;
                            }
                        }
                    },
                    None => tokio::select! {
                        _ = body_signal.wait() => {
                            yield Err(TransportError::Aborted);
                            return;
                        }
                        next = bytes.next() => next,
                    },
                };
                match next {
                    Some(Ok(chunk)) => yield Ok(chunk.to_vec()),
                    Some(Err(error)) => {
                        yield Err(reqwest_request_error(error));
                        return;
                    }
                    None => return,
                }
            }
        };
        Ok(HttpResponse {
            status,
            content_type,
            headers: response_headers,
            body: Box::pin(output),
        })
    }
}

fn reqwest_request_error(error: reqwest::Error) -> TransportError {
    let request_url = error.url().map(|url| url.as_str().to_string());
    let origin = error.url().and_then(|url| {
        let origin = url.origin().ascii_serialization();
        (origin != "null").then_some(origin)
    });
    let redact_url = |message: String| {
        request_url.as_deref().map_or(message.clone(), |url| {
            message.replace(url, origin.as_deref().unwrap_or("<request URL>"))
        })
    };
    let mut messages = vec![redact_url(error.to_string())];
    let mut source = StdError::source(&error);
    while let Some(cause) = source {
        let message = redact_url(cause.to_string());
        if !message.is_empty() && messages.last() != Some(&message) {
            messages.push(message);
        }
        source = StdError::source(cause);
    }
    TransportError::Request(sanitize_diagnostic_text(&messages.join(": ")))
}

fn response_is_retryable(status: u16, headers: &HeaderMap) -> bool {
    match headers
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok())
    {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    matches!(status, 408 | 409 | 429 | 500..=599)
}

fn retry_delay(
    headers: &HeaderMap,
    retry_index: u32,
    max_retry_delay: Duration,
) -> Result<Duration, TransportError> {
    let requested = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|milliseconds| milliseconds.is_finite())
        .map(|milliseconds| Duration::from_secs_f64((milliseconds.max(0.0)) / 1_000.0))
        .or_else(|| {
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| {
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|seconds| seconds.is_finite())
                        .map(|seconds| Duration::from_secs_f64(seconds.max(0.0)))
                        .or_else(|| {
                            httpdate::parse_http_date(value).ok().map(|date| {
                                date.duration_since(SystemTime::now()).unwrap_or_default()
                            })
                        })
                })
        });
    let delay = requested.unwrap_or_else(|| exponential_retry_delay(retry_index));
    if !max_retry_delay.is_zero() && delay > max_retry_delay {
        return Err(TransportError::Request(format!(
            "server requested {}s retry delay (max: {}s)",
            delay.as_secs_f64().ceil(),
            max_retry_delay.as_secs_f64().ceil()
        )));
    }
    Ok(delay)
}

fn exponential_retry_delay(retry_index: u32) -> Duration {
    let exponent = retry_index.min(4);
    Duration::from_millis(500_u64.saturating_mul(1_u64 << exponent).min(8_000))
}

async fn wait_for_retry(delay: Duration, signal: &AbortSignal) -> Result<(), TransportError> {
    tokio::select! {
        () = tokio::time::sleep(delay) => Ok(()),
        () = signal.wait() => Err(TransportError::Aborted),
    }
}

fn url_is_local(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.eq_ignore_ascii_case("localhost.localdomain")
            || host == "0.0.0.0"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

pub async fn collect_body_limited(
    mut body: HttpBodyStream,
    limit: usize,
) -> Result<String, TransportError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(TransportError::BodyTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads a non-success provider response without interpreting its vendor-owned
/// payload. The HTTP status is prepended and the complete raw body is retained.
pub async fn read_error_response(response: HttpResponse) -> Result<String, TransportError> {
    let status = response.status;
    let body = match collect_body(response.body).await {
        Ok(body) => body,
        Err(TransportError::Aborted) => return Err(TransportError::Aborted),
        Err(error) => return Ok(format!("{}\n{error}", http_status_line(status))),
    };
    let mut message = http_status_line(status);
    if !body.is_empty() {
        message.push('\n');
        message.push_str(&body);
    }
    Ok(message)
}

async fn collect_body(mut body: HttpBodyStream) -> Result<String, TransportError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn http_status_line(status: u16) -> String {
    reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .map_or_else(
            || format!("HTTP {status}"),
            |reason| format!("HTTP {status} {reason}"),
        )
}

fn sanitize_diagnostic_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Poll;
    use std::time::{Duration, SystemTime};

    use async_trait::async_trait;
    use futures::StreamExt;
    use pi_core::{AbortSignal, ModelId, PluginId, ProviderId};
    use pi_plugin::{
        AfterProviderResponseEvent, BeforeProviderHeadersEvent, PluginError, ProviderCallContext,
        ProviderPlugin, ProviderPluginContext, ProviderPluginDriver,
    };
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::{Value, json};

    use super::{
        DEFAULT_REMOTE_TIMEOUT, HttpResponse, HttpTransport, ReqwestTransport,
        ReqwestTransportConfig, TransportError, post_json_with_provider_hooks, read_error_response,
        response_is_retryable, retry_delay,
    };

    struct CapturingTransport {
        headers: Mutex<Option<BTreeMap<String, String>>>,
        body_polls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            _url: &str,
            headers: &BTreeMap<String, String>,
            _body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self.headers.lock().unwrap() = Some(headers.clone());
            let body_polls = Arc::clone(&self.body_polls);
            Ok(HttpResponse {
                status: 429,
                content_type: Some("text/event-stream".to_string()),
                headers: vec![("retry-after".to_string(), "2".to_string())],
                body: Box::pin(futures::stream::poll_fn(move |_context| {
                    body_polls.fetch_add(1, Ordering::SeqCst);
                    Poll::Ready(None)
                })),
            })
        }
    }

    struct WireObserver {
        observations: Arc<Mutex<Vec<String>>>,
    }

    #[pi_plugin::provider_plugin]
    impl ProviderPlugin for WireObserver {
        fn id(&self) -> PluginId {
            PluginId::new("wire-observer")
        }

        async fn before_provider_headers(
            &self,
            _context: ProviderPluginContext,
            event: BeforeProviderHeadersEvent,
        ) -> Result<Option<BTreeMap<String, Option<String>>>, PluginError> {
            assert_eq!(event.headers["Existing"].as_deref(), Some("yes"));
            let mut headers = event.headers;
            headers.insert("X-Trace".to_string(), Some("trace-1".to_string()));
            headers.insert("X-Remove".to_string(), None);
            Ok(Some(headers))
        }

        async fn after_provider_response(
            &self,
            _context: ProviderPluginContext,
            event: AfterProviderResponseEvent,
        ) -> Result<(), PluginError> {
            self.observations
                .lock()
                .unwrap()
                .push(format!("{}:{}", event.status, event.headers["retry-after"]));
            Ok(())
        }
    }

    #[test]
    fn remote_provider_default_idle_timeout_matches_pi() {
        assert_eq!(DEFAULT_REMOTE_TIMEOUT, Duration::from_secs(300));
        assert_eq!(
            ReqwestTransport::new().timeout_for("https://api.example.test/v1/chat/completions"),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn provider_retry_policy_honors_status_overrides_and_delay_cap() {
        let mut headers = HeaderMap::new();
        assert!(response_is_retryable(503, &headers));
        assert!(!response_is_retryable(400, &headers));

        headers.insert("x-should-retry", HeaderValue::from_static("true"));
        assert!(response_is_retryable(400, &headers));
        headers.insert("x-should-retry", HeaderValue::from_static("false"));
        assert!(!response_is_retryable(503, &headers));

        headers.remove("x-should-retry");
        headers.insert("retry-after-ms", HeaderValue::from_static("1500"));
        assert!(matches!(
            retry_delay(&headers, 0, Duration::from_secs(1)),
            Err(TransportError::Request(message)) if message.contains("retry delay")
        ));
        assert_eq!(
            retry_delay(&headers, 0, Duration::ZERO).unwrap(),
            Duration::from_millis(1_500)
        );

        headers.remove("retry-after-ms");
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_str(&httpdate::fmt_http_date(
                SystemTime::now() + Duration::from_secs(120),
            ))
            .unwrap(),
        );
        assert!(matches!(
            retry_delay(&headers, 0, Duration::from_secs(60)),
            Err(TransportError::Request(message)) if message.contains("retry delay")
        ));
        let date_delay = retry_delay(&headers, 0, Duration::ZERO).unwrap();
        assert!(date_delay >= Duration::from_secs(118));
        assert!(date_delay <= Duration::from_secs(120));
    }

    #[tokio::test]
    async fn provider_hooks_wrap_transport_without_consuming_the_response_body() {
        let body_polls = Arc::new(AtomicUsize::new(0));
        let transport = CapturingTransport {
            headers: Mutex::new(None),
            body_polls: Arc::clone(&body_polls),
        };
        let observations = Arc::new(Mutex::new(Vec::new()));
        let plugins = Arc::new(
            ProviderPluginDriver::new(vec![Arc::new(WireObserver {
                observations: Arc::clone(&observations),
            })])
            .unwrap(),
        );
        let context = ProviderCallContext::new(
            9,
            "/workspace",
            ProviderId::new("provider"),
            ModelId::new("model"),
            plugins,
        );
        let (_, signal) = pi_core::AbortHandle::new();

        let mut response = post_json_with_provider_hooks(
            &transport,
            &context,
            "https://example.test/v1/messages",
            BTreeMap::from([
                ("Existing".to_string(), "yes".to_string()),
                ("X-Remove".to_string(), "remove-me".to_string()),
            ]),
            &json!({"prompt": "hello"}),
            signal,
        )
        .await
        .unwrap();

        let sent_headers = transport.headers.lock().unwrap().clone().unwrap();
        assert_eq!(sent_headers["Existing"], "yes");
        assert_eq!(sent_headers["X-Trace"], "trace-1");
        assert!(!sent_headers.contains_key("X-Remove"));
        assert_eq!(*observations.lock().unwrap(), vec!["429:2"]);
        assert_eq!(body_polls.load(Ordering::SeqCst), 0);

        assert!(response.body.next().await.is_none());
        assert_eq!(body_polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_error_response_preserves_the_raw_body() {
        let body = r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error","code":"rate_limit_exceeded","unknown":{"scope":"tokens_per_minute"}}}"#;
        let response = HttpResponse {
            status: 429,
            content_type: Some("application/json".to_string()),
            headers: vec![
                ("x-request-id".to_string(), "req_123".to_string()),
                ("retry-after".to_string(), "15".to_string()),
            ],
            body: Box::pin(futures::stream::iter(vec![Ok(body.as_bytes().to_vec())])),
        };

        let message = read_error_response(response).await.unwrap();

        assert_eq!(message, format!("HTTP 429 Too Many Requests\n{body}"));
    }

    #[tokio::test]
    async fn provider_error_response_keeps_status_when_body_read_fails() {
        let response = HttpResponse {
            status: 502,
            content_type: None,
            headers: vec![("request-id".to_string(), "gateway_456".to_string())],
            body: Box::pin(futures::stream::iter(vec![Err(TransportError::Request(
                "connection reset".to_string(),
            ))])),
        };

        let message = read_error_response(response).await.unwrap();

        assert_eq!(
            message,
            "HTTP 502 Bad Gateway\nHTTP request failed: connection reset"
        );
    }

    #[tokio::test]
    async fn transport_error_keeps_cause_without_exposing_request_query() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let transport = ReqwestTransport::with_config(ReqwestTransportConfig {
            timeout: Some(Duration::from_secs(2)),
            ..ReqwestTransportConfig::default()
        })
        .unwrap();
        let (_, signal) = pi_core::AbortHandle::new();

        let result = transport
            .post_json(
                &format!("http://{address}/v1/messages?key=super-secret"),
                &BTreeMap::new(),
                &json!({}),
                signal,
            )
            .await;
        let Err(error) = result else {
            panic!("request to a closed local port unexpectedly succeeded");
        };
        let message = error.to_string();

        assert!(message.contains("error sending request for url (http://127.0.0.1"));
        assert!(message.contains("client error"));
        assert!(!message.contains("super-secret"));
    }
}
