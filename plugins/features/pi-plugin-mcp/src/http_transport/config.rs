//! Only settings implemented by the private worker, not a stock SDK retry-policy facade.
use super::*;

pub(crate) struct HttpTransportConfig {
    pub(crate) uri: Arc<str>,
    pub(crate) channel_buffer_capacity: usize,
    pub(crate) max_concurrent_requests: usize,
    pub(crate) control_request_timeout: Duration,
    pub(crate) custom_headers: HashMap<HeaderName, HeaderValue>,
    pub(crate) max_sse_event_size: usize,
}

impl HttpTransportConfig {
    pub(crate) fn with_uri(uri: impl Into<Arc<str>>) -> Self {
        Self {
            uri: uri.into(),
            channel_buffer_capacity: 16,
            max_concurrent_requests: 16,
            control_request_timeout: Duration::from_secs(5),
            custom_headers: HashMap::new(),
            max_sse_event_size: 16 * 1024 * 1024,
        }
    }

    pub(crate) fn custom_headers(mut self, headers: HashMap<HeaderName, HeaderValue>) -> Self {
        self.custom_headers = headers;
        self
    }
}
