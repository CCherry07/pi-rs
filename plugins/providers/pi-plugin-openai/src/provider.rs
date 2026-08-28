use std::collections::BTreeMap;
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{
    AbortSignal, Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderId,
    ProviderRequest, ProviderStream, ResponseMetadata, StreamEvent,
};
use pi_provider::{
    HttpTransport, ReqwestTransport, SseDecoder, TransportError, collect_body_limited,
    post_json_with_provider_hooks,
};

use crate::config::{OpenAiCompatibleConfig, OpenAiConfig, validate_config};
use crate::request::{ResolvedOpenAiCompletionsCompat, affinity_headers, request_body};
use crate::stream::{ChunkState, consume_json};

const API_NAME: &str = "openai-completions";

pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
    transport: Arc<dyn HttpTransport>,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: OpenAiCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        validate_config(&config)?;
        Ok(Self { config, transport })
    }

    pub(crate) fn endpoint(&self) -> String {
        completions_endpoint(&self.config.base_url)
    }

    fn headers(&self, request: &ProviderRequest) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        if let Some(key) = &self.config.api_key {
            insert_header(&mut headers, "Authorization", format!("Bearer {key}"));
        }
        for (name, value) in affinity_headers(request) {
            insert_header(&mut headers, name, value);
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id.clone()
    }

    fn availability(&self) -> ProviderAvailability {
        if self.config.api_key.is_some() {
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
        let headers = self.headers(&request);
        let supports_finish_reason =
            ResolvedOpenAiCompletionsCompat::for_request(&request).supports_finish_reason;
        let endpoint = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .map(completions_endpoint)
            .unwrap_or_else(|| self.endpoint());
        let payload = context
            .before_provider_request(&signal, request_body(&request))
            .await?;
        let response = post_json_with_provider_hooks(
            self.transport.as_ref(),
            &context,
            &endpoint,
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
        if !response
            .content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected text/event-stream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }

        let provider_id = self.config.provider_id.clone();
        let model = request.model;
        let mut body = response.body;
        let output = stream! {
            yield Ok(StreamEvent::Start {
                metadata: ResponseMetadata::new(provider_id, model.clone(), API_NAME, now_ms()),
            });
            let mut decoder = SseDecoder::new();
            let mut state = ChunkState::for_model(supports_finish_reason, &model);
            loop {
                let next = tokio::select! {
                    _ = signal.wait() => {
                        yield Err(ProviderError::Aborted);
                        return;
                    }
                    next = body.next() => next,
                };
                match next {
                    Some(Ok(bytes)) => {
                        let decoded = match decoder.push(&bytes) {
                            Ok(events) => events,
                            Err(error) => {
                                yield Err(map_transport_error(error));
                                return;
                            }
                        };
                        for event in decoded {
                            if event.data == "[DONE]" {
                                match state.finish() {
                                    Ok(events) => for event in events { yield Ok(event); },
                                    Err(error) => yield Err(error),
                                }
                                return;
                            }
                            match consume_json(&mut state, &event.data) {
                                Ok(events) => for event in events { yield Ok(event); },
                                Err(error) => { yield Err(error); return; }
                            }
                        }
                    }
                    Some(Err(error)) => {
                        yield Err(map_transport_error(error));
                        return;
                    }
                    None => {
                        match decoder.finish() {
                            Ok(Some(event)) if event.data != "[DONE]" => {
                                match consume_json(&mut state, &event.data) {
                                    Ok(events) => for event in events { yield Ok(event); },
                                    Err(error) => { yield Err(error); return; }
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                yield Err(map_transport_error(error));
                                return;
                            }
                        }
                        match state.finish() {
                            Ok(events) => for event in events { yield Ok(event); },
                            Err(error) => yield Err(error),
                        }
                        return;
                    }
                }
            }
        };
        Ok(Box::pin(output))
    }
}

fn completions_endpoint(base: &str) -> String {
    let base = base.trim();
    let suffix_start = [base.find('?'), base.find('#')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(base.len());
    let (path, suffix) = base.split_at(suffix_start);
    let path = path.trim_end_matches('/');
    if path.ends_with("/chat/completions") {
        format!("{path}{suffix}")
    } else {
        format!("{path}/chat/completions{suffix}")
    }
}

fn insert_header(
    headers: &mut BTreeMap<String, String>,
    name: impl AsRef<str>,
    value: impl Into<String>,
) {
    let name = name.as_ref();
    if let Some(existing) = headers
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.into());
}

pub struct OpenAiProvider {
    inner: OpenAiCompatibleProvider,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: OpenAiCompatibleProvider::new(config.compatible_config())?,
        })
    }

    pub fn with_transport(
        config: OpenAiConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: OpenAiCompatibleProvider::with_transport(config.compatible_config(), transport)?,
        })
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("openai")
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        self.inner.stream(request, context, signal).await
    }
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        TransportError::InvalidConfiguration(message) | TransportError::InvalidSse(message) => {
            ProviderError::Protocol(message)
        }
        TransportError::Timeout { seconds } => {
            ProviderError::Failure(format!("request timed out after {seconds}s"))
        }
        TransportError::Request(message) => ProviderError::Failure(message),
        TransportError::BodyTooLarge { limit } => {
            ProviderError::Failure(format!("response body exceeds the {limit}-byte limit"))
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pi_core::{
        AfterProviderResponseEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
        PluginError, PluginId, ProviderPlugin, ProviderPluginContext, ProviderPluginDriver,
    };
    use pi_provider::{HttpResponse, TransportError};
    use serde_json::{Value, json};

    use super::*;
    use pi_core::{ModelId, ThinkingLevel};

    struct CapturingTransport {
        body: Mutex<Option<Value>>,
        headers: Mutex<Option<BTreeMap<String, String>>>,
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            _url: &str,
            headers: &BTreeMap<String, String>,
            body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self
                .body
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(body.clone());
            *self
                .headers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(headers.clone());
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/event-stream".to_string()),
                headers: vec![("x-response-id".to_string(), "response-1".to_string())],
                body: Box::pin(futures::stream::empty()),
            })
        }
    }

    struct PayloadPlugin {
        responses: Arc<Mutex<Vec<String>>>,
    }

    #[pi_core::provider_plugin]
    impl ProviderPlugin for PayloadPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("payload-hook")
        }

        async fn before_provider_request(
            &self,
            context: ProviderPluginContext,
            event: BeforeProviderRequestEvent,
        ) -> std::result::Result<Option<Value>, PluginError> {
            assert_eq!(context.generation, 3);
            assert_eq!(context.provider_id, ProviderId::new("custom"));
            assert_eq!(context.model_id, ModelId::new("model"));
            let mut payload = event.payload;
            payload["hooked"] = json!(true);
            Ok(Some(payload))
        }

        async fn before_provider_headers(
            &self,
            _context: ProviderPluginContext,
            event: BeforeProviderHeadersEvent,
        ) -> std::result::Result<Option<BTreeMap<String, Option<String>>>, PluginError> {
            assert_eq!(
                event.headers["Accept"].as_deref(),
                Some("text/event-stream")
            );
            assert_eq!(
                event.headers["Content-Type"].as_deref(),
                Some("application/json")
            );
            let mut headers = event.headers;
            headers.insert("X-Hooked".to_string(), Some("yes".to_string()));
            Ok(Some(headers))
        }

        async fn after_provider_response(
            &self,
            _context: ProviderPluginContext,
            event: AfterProviderResponseEvent,
        ) -> std::result::Result<(), PluginError> {
            self.responses.lock().unwrap().push(format!(
                "{}:{}",
                event.status, event.headers["x-response-id"]
            ));
            Ok(())
        }
    }

    #[test]
    fn endpoint_accepts_root_or_full_url() {
        let root = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::without_api_key(
            "http://localhost:11434/v1",
        ))
        .unwrap();
        assert_eq!(
            root.endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
        let full = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::without_api_key(
            "https://api.openai.com/v1/chat/completions",
        ))
        .unwrap();
        assert_eq!(
            full.endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );

        let full_with_query = OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::without_api_key(
                "https://gateway.example/openai/deployments/chat/completions?api-version=2024-02-01",
            ),
        )
        .unwrap();
        assert_eq!(
            full_with_query.endpoint(),
            "https://gateway.example/openai/deployments/chat/completions?api-version=2024-02-01"
        );
    }

    #[test]
    fn request_headers_override_static_credentials_case_insensitively() {
        let provider = OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::new("https://example.test/v1", "static")
                .header("X-Static", "yes"),
        )
        .unwrap();
        let request = ProviderRequest {
            model: ModelId::new("model"),
            model_spec: None,
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Off,
            thinking_budgets: None,
            max_output_tokens: None,
            headers: BTreeMap::from([("authorization".to_string(), "Bearer request".to_string())]),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let headers = provider.headers(&request);

        assert_eq!(headers["authorization"], "Bearer request");
        assert!(!headers.contains_key("Authorization"));
        assert_eq!(headers["X-Static"], "yes");
    }

    #[tokio::test]
    async fn request_hook_runs_after_serialization_and_before_transport() {
        let transport = Arc::new(CapturingTransport {
            body: Mutex::new(None),
            headers: Mutex::new(None),
        });
        let provider = OpenAiCompatibleProvider::with_transport(
            OpenAiCompatibleConfig::without_api_key("https://example.test/v1")
                .provider_id("custom"),
            transport.clone(),
        )
        .unwrap();
        let responses = Arc::new(Mutex::new(Vec::new()));
        let provider_plugins = Arc::new(
            ProviderPluginDriver::new(vec![Arc::new(PayloadPlugin {
                responses: Arc::clone(&responses),
            })])
            .unwrap(),
        );
        let call_context = ProviderCallContext::new(
            3,
            "/project",
            ProviderId::new("custom"),
            ModelId::new("model"),
            provider_plugins,
        );
        let request = ProviderRequest {
            model: ModelId::new("model"),
            model_spec: None,
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Off,
            thinking_budgets: None,
            max_output_tokens: None,
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let (_, signal) = pi_core::AbortHandle::new();

        let _stream = provider
            .stream(request, call_context, signal)
            .await
            .unwrap();

        let body = transport
            .body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap();
        assert_eq!(body["model"], "model");
        assert_eq!(body["hooked"], true);
        let headers = transport
            .headers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap();
        assert_eq!(headers["Accept"], "text/event-stream");
        assert_eq!(headers["X-Hooked"], "yes");
        assert_eq!(*responses.lock().unwrap(), vec!["200:response-1"]);
    }
}
