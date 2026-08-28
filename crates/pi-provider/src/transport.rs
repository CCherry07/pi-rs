use std::collections::BTreeMap;
use std::pin::Pin;
use std::time::Duration;

use async_stream::stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use pi_core::{AbortSignal, ProviderCallContext};
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

#[derive(Debug, Clone, Default)]
pub struct ReqwestTransportConfig {
    /// Overrides provider-aware defaults. `Some(Duration::ZERO)` disables timeouts.
    pub timeout: Option<Duration>,
    pub user_agent: Option<String>,
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

        let send = self
            .client
            .post(url)
            .headers(request_headers)
            .json(body)
            .send();
        let response = if let Some(timeout) = self.timeout_for(url) {
            tokio::select! {
                _ = signal.wait() => return Err(TransportError::Aborted),
                result = tokio::time::timeout(timeout, send) => match result {
                    Ok(result) => result.map_err(|error| TransportError::Request(error.to_string()))?,
                    Err(_) => return Err(TransportError::Timeout { seconds: timeout.as_secs() }),
                }
            }
        } else {
            tokio::select! {
                _ = signal.wait() => return Err(TransportError::Aborted),
                result = send => result.map_err(|error| TransportError::Request(error.to_string()))?,
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
                        yield Err(TransportError::Request(error.to_string()));
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Poll;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::StreamExt;
    use pi_core::{
        AbortSignal, AfterProviderResponseEvent, BeforeProviderHeadersEvent, ModelId, PluginError,
        PluginId, ProviderCallContext, ProviderId, ProviderPlugin, ProviderPluginContext,
        ProviderPluginDriver,
    };
    use serde_json::{Value, json};

    use super::{
        DEFAULT_REMOTE_TIMEOUT, HttpResponse, HttpTransport, ReqwestTransport, TransportError,
        post_json_with_provider_hooks,
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

    #[pi_core::provider_plugin]
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
}
