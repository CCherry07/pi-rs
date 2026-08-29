use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ModelCost, ModelId, Provider, ProviderAvailability, ProviderCallContext,
    ProviderError, ProviderId, ProviderRequest, ProviderStream, StreamEvent,
};
use pi_plugin_anthropic::{
    ANTHROPIC_MESSAGES_API, AnthropicCompatibleConfig, AnthropicCompatibleProvider,
};
use pi_plugin_azure_openai::{
    AZURE_OPENAI_RESPONSES_API, AzureOpenAiResponsesConfig, AzureOpenAiResponsesProvider,
};
use pi_plugin_bedrock::{AmazonBedrockProvider, BEDROCK_CONVERSE_STREAM_API, BedrockConfig};
use pi_plugin_google::{
    GOOGLE_GENERATIVE_AI_API, GOOGLE_VERTEX_API, GoogleCompatibleConfig, GoogleCompatibleProvider,
    GoogleVertexCompatibleConfig, GoogleVertexCompatibleProvider,
};
use pi_plugin_mistral::{
    MISTRAL_CONVERSATIONS_API, MistralCompatibleConfig, MistralCompatibleProvider,
};
use pi_plugin_openai::{
    OPENAI_RESPONSES_API, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    OpenAiResponsesCompatibleProvider,
};
use pi_provider::HttpTransport;
#[cfg(test)]
use pi_provider::ReqwestTransport;

use crate::config::{PreparedModel, PreparedOverride, PreparedProvider};
use crate::resolver::{ConfigValueResolver, ResolveError};

pub(crate) struct ModelsJsonProvider {
    configured: PreparedProvider,
    fallback: Option<Arc<dyn Provider>>,
    fallback_apis: HashSet<String>,
    resolver: Arc<ConfigValueResolver>,
    transport: Arc<dyn HttpTransport>,
}

struct ResolvedRequestConfig {
    api_key: Option<String>,
    headers: BTreeMap<String, String>,
}

impl ModelsJsonProvider {
    #[cfg(test)]
    pub fn new(
        configured: PreparedProvider,
        fallback: Option<Arc<dyn Provider>>,
        fallback_apis: HashSet<String>,
        resolver: Arc<ConfigValueResolver>,
    ) -> Self {
        Self::with_transport(
            configured,
            fallback,
            fallback_apis,
            resolver,
            Arc::new(ReqwestTransport::new()),
        )
    }

    pub(crate) fn with_transport(
        configured: PreparedProvider,
        fallback: Option<Arc<dyn Provider>>,
        fallback_apis: HashSet<String>,
        resolver: Arc<ConfigValueResolver>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            configured,
            fallback,
            fallback_apis,
            resolver,
            transport,
        }
    }

    fn model(&self, id: &ModelId) -> Option<&PreparedModel> {
        self.configured.models.iter().find(|model| &model.id == id)
    }

    fn model_override(&self, id: &ModelId) -> Option<&PreparedOverride> {
        self.configured.model_overrides.get(id)
    }

    async fn resolve_request_config(
        &self,
        model_id: &ModelId,
        model: Option<&PreparedModel>,
        model_override: Option<&PreparedOverride>,
        signal: &AbortSignal,
    ) -> Result<ResolvedRequestConfig, ProviderError> {
        let mut headers = self
            .resolve_header_set(
                &self.configured.headers,
                &format!("provider {}", self.configured.id),
                signal,
            )
            .await?;
        let api_key = match &self.configured.runtime_api_key {
            Some(api_key) => Some(api_key.clone()),
            None => match &self.configured.api_key {
                Some(configured) => Some(
                    self.resolver
                        .resolve(
                            configured,
                            &format!("API key for provider {}", self.configured.id),
                            signal,
                        )
                        .await
                        .map_err(map_resolve_error)?,
                ),
                None => None,
            },
        };
        if self.configured.auth_header.unwrap_or(false) && api_key.is_none() {
            return Err(ProviderError::Failure(format!(
                "provider {}: authHeader requires a resolved API key",
                self.configured.id
            )));
        }
        if self.configured.auth_header.unwrap_or(false)
            && let Some(api_key) = &api_key
        {
            insert_header(&mut headers, "Authorization", format!("Bearer {api_key}"));
        }
        let route_headers = model
            .map(|model| &model.headers)
            .or_else(|| model_override.map(|value| &value.headers));
        if let Some(route_headers) = route_headers {
            let route_headers = self
                .resolve_header_set(
                    route_headers,
                    &format!("model {}/{}", self.configured.id, model_id),
                    signal,
                )
                .await?;
            for (name, value) in route_headers {
                insert_header(&mut headers, name, value);
            }
        }
        Ok(ResolvedRequestConfig { api_key, headers })
    }

    async fn resolve_header_set(
        &self,
        configured: &BTreeMap<String, String>,
        description: &str,
        signal: &AbortSignal,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let mut resolved = BTreeMap::new();
        for (name, value) in configured {
            let value = self
                .resolver
                .resolve(value, &format!("{description} header {name:?}"), signal)
                .await
                .map_err(map_resolve_error)?;
            insert_header(&mut resolved, name, value);
        }
        Ok(resolved)
    }

    fn apply_model_defaults(
        api: Option<&str>,
        model: Option<&PreparedModel>,
        request: &mut ProviderRequest,
    ) {
        let max_tokens = model.map(|model| model.spec.max_tokens);
        if request.max_output_tokens.is_none() {
            request.max_output_tokens = max_tokens;
        }

        // Pi only applies free-form samplingParams to OpenAI-compatible APIs.
        let mut sampling_params = if matches!(
            api,
            Some(
                "openai-completions"
                    | OPENAI_RESPONSES_API
                    | AZURE_OPENAI_RESPONSES_API
                    | MISTRAL_CONVERSATIONS_API
            )
        ) {
            model
                .map(|model| model.spec.sampling_params.clone())
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        sampling_params.extend(std::mem::take(&mut request.sampling_params));
        request.sampling_params = sampling_params;
        if let Some(model) = model {
            request.model_spec = Some(model.spec.clone());
        }
    }
}

#[async_trait]
impl Provider for ModelsJsonProvider {
    fn id(&self) -> ProviderId {
        self.configured.id.clone()
    }

    fn name(&self) -> String {
        self.configured
            .name
            .clone()
            .unwrap_or_else(|| self.configured.id.to_string())
    }

    fn availability(&self) -> ProviderAvailability {
        if self.configured.runtime_api_key.is_some()
            || self.configured.api_key.is_some()
            || self
                .fallback
                .as_ref()
                .is_some_and(|provider| provider.availability().is_available())
        {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn stream(
        &self,
        mut request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let model = self.model(&request.model);
        let model_override = self.model_override(&request.model);
        let api = model
            .map(|model| model.spec.api.as_str())
            .or(self.configured.api.as_deref());
        Self::apply_model_defaults(api, model, &mut request);
        let cost = model.map(|model| model.spec.cost.clone());
        let resolved = self
            .resolve_request_config(&request.model, model, model_override, &signal)
            .await?;
        let request_headers = std::mem::take(&mut request.headers);
        request.headers = resolved.headers;
        for (name, value) in request_headers {
            insert_header(&mut request.headers, name, value);
        }

        let base_url = model
            .and_then(|model| model.spec.base_url.as_deref())
            .or(self.configured.base_url.as_deref());
        let fallback_supports_api = api.is_some_and(|api| self.fallback_apis.contains(api));
        if let Some(fallback) = &self.fallback
            && (base_url.is_none() || (fallback_supports_api && resolved.api_key.is_none()))
        {
            if let Some(api_key) = &resolved.api_key {
                insert_header(
                    &mut request.headers,
                    "Authorization",
                    format!("Bearer {api_key}"),
                );
            }
            let stream = fallback.stream(request, context, signal).await?;
            return Ok(apply_cost(stream, cost));
        }
        if let Some(base_url) = base_url {
            let api = model
                .map(|model| model.spec.api.as_str())
                .or(self.configured.api.as_deref())
                .ok_or_else(|| {
                    ProviderError::Failure(format!(
                        "provider {} has no API route for model {}",
                        self.configured.id, request.model
                    ))
                })?;
            let stream = match api {
                "openai-completions" => {
                    let config = resolved.api_key.map_or_else(
                        || OpenAiCompatibleConfig::without_api_key(base_url),
                        |api_key| OpenAiCompatibleConfig::new(base_url, api_key),
                    );
                    let provider = OpenAiCompatibleProvider::with_transport(
                        config.provider_id(self.configured.id.clone()),
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                OPENAI_RESPONSES_API => {
                    let config = resolved.api_key.map_or_else(
                        || OpenAiCompatibleConfig::without_api_key(base_url),
                        |api_key| OpenAiCompatibleConfig::new(base_url, api_key),
                    );
                    let provider = OpenAiResponsesCompatibleProvider::with_transport(
                        config.provider_id(self.configured.id.clone()),
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                AZURE_OPENAI_RESPONSES_API => {
                    let config = resolved
                        .api_key
                        .map_or_else(
                            || AzureOpenAiResponsesConfig::without_api_key(base_url),
                            |api_key| AzureOpenAiResponsesConfig::new(base_url, api_key),
                        )
                        .provider_id(self.configured.id.clone());
                    let provider = AzureOpenAiResponsesProvider::with_transport(
                        config,
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                MISTRAL_CONVERSATIONS_API => {
                    let config = resolved
                        .api_key
                        .map_or_else(
                            || MistralCompatibleConfig::without_api_key(base_url),
                            |api_key| MistralCompatibleConfig::new(base_url, api_key),
                        )
                        .provider_id(self.configured.id.clone());
                    let provider = MistralCompatibleProvider::with_transport(
                        config,
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                ANTHROPIC_MESSAGES_API => {
                    let config = resolved.api_key.map_or_else(
                        || AnthropicCompatibleConfig::without_api_key(base_url),
                        |api_key| AnthropicCompatibleConfig::new(base_url, api_key),
                    );
                    let provider = AnthropicCompatibleProvider::with_transport(
                        config.provider_id(self.configured.id.clone()),
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                GOOGLE_GENERATIVE_AI_API => {
                    let config = resolved.api_key.map_or_else(
                        || GoogleCompatibleConfig::without_api_key(base_url),
                        |api_key| GoogleCompatibleConfig::new(base_url, api_key),
                    );
                    let provider = GoogleCompatibleProvider::with_transport(
                        config.provider_id(self.configured.id.clone()),
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                GOOGLE_VERTEX_API => {
                    let mut config =
                        GoogleVertexCompatibleConfig::from_environment(resolved.api_key)?;
                    config.base_url = Some(base_url.to_string());
                    let provider = GoogleVertexCompatibleProvider::with_transport(
                        config.provider_id(self.configured.id.clone()),
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                BEDROCK_CONVERSE_STREAM_API => {
                    let mut config =
                        BedrockConfig::from_environment(resolved.api_key, BTreeMap::new())?;
                    config.base_url = Some(base_url.to_string());
                    let provider = AmazonBedrockProvider::with_transport(
                        config.provider_id(self.configured.id.clone()),
                        Arc::clone(&self.transport),
                    )?;
                    provider.stream(request, context, signal).await
                }
                _ => Err(ProviderError::Failure(format!(
                    "provider {} has no implementation for API {api:?}",
                    self.configured.id
                ))),
            }?;
            return Ok(apply_cost(stream, cost));
        }
        if let Some(api_key) = resolved.api_key {
            insert_header(
                &mut request.headers,
                "Authorization",
                format!("Bearer {api_key}"),
            );
        }
        let stream = match &self.fallback {
            Some(fallback) => fallback.stream(request, context, signal).await?,
            None => Err(ProviderError::Failure(format!(
                "provider {} has no baseUrl or lower-layer provider for model {}",
                self.configured.id, request.model
            )))?,
        };
        Ok(apply_cost(stream, cost))
    }
}

fn apply_cost(stream: ProviderStream, cost: Option<ModelCost>) -> ProviderStream {
    let Some(cost) = cost else {
        return stream;
    };
    Box::pin(stream.map(move |event| {
        event.map(|mut event| {
            if let StreamEvent::Done { usage, .. } = &mut event {
                usage.cost = cost.calculate(usage);
            }
            event
        })
    }))
}

fn map_resolve_error(error: ResolveError) -> ProviderError {
    match error {
        ResolveError::Aborted => ProviderError::Aborted,
        ResolveError::Failed(message) => ProviderError::Failure(message),
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, ModelSpec, StopReason, ThinkingLevel, Usage};
    use pi_provider::{HttpResponse, TransportError};
    use serde_json::json;

    use super::*;

    struct CapturingProvider {
        request: Arc<Mutex<Option<ProviderRequest>>>,
    }

    struct UsageProvider;

    #[async_trait]
    impl Provider for UsageProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("priced")
        }

        async fn stream(
            &self,
            _request: ProviderRequest,
            _context: ProviderCallContext,
            _signal: AbortSignal,
        ) -> Result<ProviderStream, ProviderError> {
            Ok(Box::pin(stream::iter([Ok(StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage {
                    input: 100,
                    output: 20,
                    cache_read: 40,
                    cache_write: 80,
                    cache_write_1h: Some(30),
                    total_tokens: 240,
                    ..Usage::default()
                },
            })])))
        }
    }

    #[derive(Default)]
    struct CapturedHttpRequest {
        url: String,
        headers: BTreeMap<String, String>,
        body: Option<serde_json::Value>,
    }

    #[derive(Default)]
    struct CapturingTransport {
        request: Mutex<CapturedHttpRequest>,
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            body: &serde_json::Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = CapturedHttpRequest {
                url: url.to_string(),
                headers: headers.clone(),
                body: Some(body.clone()),
            };
            Ok(HttpResponse {
                status: 200,
                content_type: Some(
                    if url.ends_with("/converse-stream") {
                        "application/vnd.amazon.eventstream"
                    } else {
                        "text/event-stream"
                    }
                    .to_string(),
                ),
                headers: Vec::new(),
                body: Box::pin(stream::empty()),
            })
        }
    }

    #[async_trait]
    impl Provider for CapturingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("custom")
        }

        async fn stream(
            &self,
            request: ProviderRequest,
            _context: ProviderCallContext,
            _signal: AbortSignal,
        ) -> Result<ProviderStream, ProviderError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(Box::pin(stream::empty()))
        }
    }

    #[tokio::test]
    async fn routing_applies_runtime_auth_headers_and_model_defaults() {
        let mut spec = ModelSpec::new("custom", "model", "Model", "openai-completions");
        spec.reasoning = true;
        spec.max_tokens = 4_096;
        spec.sampling_params
            .insert("temperature".to_string(), json!(0.2));
        spec.thinking_level_map
            .insert("high".to_string(), Some("medium".to_string()));
        let configured = PreparedProvider {
            id: ProviderId::new("custom"),
            name: None,
            api: None,
            base_url: None,
            compat: None,
            api_key: Some("models-json-key".to_string()),
            runtime_api_key: Some("runtime-key".to_string()),
            headers: BTreeMap::from([("X-Provider".to_string(), "provider".to_string())]),
            auth_header: Some(false),
            models: vec![PreparedModel {
                id: ModelId::new("model"),
                spec,
                headers: BTreeMap::from([("X-Model".to_string(), "model".to_string())]),
            }],
            model_overrides: BTreeMap::new(),
            replace_models: false,
        };
        let captured = Arc::new(Mutex::new(None));
        let fallback: Arc<dyn Provider> = Arc::new(CapturingProvider {
            request: Arc::clone(&captured),
        });
        let provider = ModelsJsonProvider::new(
            configured,
            Some(fallback),
            HashSet::from(["openai-completions".to_string()]),
            Arc::new(ConfigValueResolver::default()),
        );
        let (_, signal) = AbortHandle::new();
        let call_context = ProviderCallContext::without_plugins(
            "/project",
            ProviderId::new("custom"),
            ModelId::new("model"),
        );
        let _stream = provider
            .stream(
                ProviderRequest {
                    model: ModelId::new("model"),
                    model_spec: None,
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::High,
                    thinking_budgets: None,
                    max_output_tokens: None,
                    headers: BTreeMap::from([("X-Request".to_string(), "request".to_string())]),
                    sampling_params: BTreeMap::new(),
                    session_id: None,
                },
                call_context,
                signal,
            )
            .await
            .unwrap();

        let request = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap();
        assert_eq!(request.headers["Authorization"], "Bearer runtime-key");
        assert_eq!(request.headers["X-Provider"], "provider");
        assert_eq!(request.headers["X-Model"], "model");
        assert_eq!(request.headers["X-Request"], "request");
        assert_eq!(request.max_output_tokens, Some(4_096));
        assert_eq!(request.sampling_params["temperature"], 0.2);
        assert!(!request.sampling_params.contains_key("reasoning_effort"));
        assert_eq!(
            request.model_spec.as_ref().unwrap().thinking_level_map["high"].as_deref(),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn routing_dispatches_anthropic_messages_with_models_json_auth_and_headers() {
        let mut spec = ModelSpec::new(
            "byteintl",
            "custom-claude",
            "Custom Claude",
            ANTHROPIC_MESSAGES_API,
        );
        spec.reasoning = true;
        spec.compat = Some(json!({"forceAdaptiveThinking": true}));
        let configured = PreparedProvider {
            id: ProviderId::new("byteintl"),
            name: None,
            api: Some(ANTHROPIC_MESSAGES_API.to_string()),
            base_url: Some("https://gateway.example/v1".to_string()),
            compat: None,
            api_key: Some("configured-key".to_string()),
            runtime_api_key: None,
            headers: BTreeMap::from([("X-Provider".to_string(), "provider".to_string())]),
            auth_header: Some(true),
            models: vec![PreparedModel {
                id: ModelId::new("custom-claude"),
                spec,
                headers: BTreeMap::from([("X-Model".to_string(), "model".to_string())]),
            }],
            model_overrides: BTreeMap::new(),
            replace_models: false,
        };
        let transport = Arc::new(CapturingTransport::default());
        let provider = ModelsJsonProvider::with_transport(
            configured,
            None,
            HashSet::new(),
            Arc::new(ConfigValueResolver::default()),
            transport.clone(),
        );
        let (_, signal) = AbortHandle::new();
        let call_context = ProviderCallContext::without_plugins(
            "/project",
            ProviderId::new("byteintl"),
            ModelId::new("custom-claude"),
        );

        let _stream = provider
            .stream(
                ProviderRequest {
                    model: ModelId::new("custom-claude"),
                    model_spec: None,
                    system_prompt: "system".to_string(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::Medium,
                    thinking_budgets: None,
                    max_output_tokens: Some(2_048),
                    headers: BTreeMap::from([("X-Request".to_string(), "request".to_string())]),
                    sampling_params: BTreeMap::new(),
                    session_id: None,
                },
                call_context,
                signal,
            )
            .await
            .unwrap();

        let captured = transport
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(captured.url, "https://gateway.example/v1/messages");
        assert_eq!(captured.headers["x-api-key"], "configured-key");
        assert_eq!(captured.headers["Authorization"], "Bearer configured-key");
        assert_eq!(captured.headers["anthropic-version"], "2023-06-01");
        assert_eq!(captured.headers["X-Provider"], "provider");
        assert_eq!(captured.headers["X-Model"], "model");
        assert_eq!(captured.headers["X-Request"], "request");
        assert_eq!(captured.body.as_ref().unwrap()["model"], "custom-claude");
        assert_eq!(captured.body.as_ref().unwrap()["max_tokens"], 2_048);
        assert_eq!(
            captured.body.as_ref().unwrap()["output_config"]["effort"],
            "medium"
        );
        assert!(captured.body.as_ref().unwrap()["reasoning_effort"].is_null());
    }

    #[tokio::test]
    async fn routing_dispatches_openai_responses_for_xai_compatible_providers() {
        let mut spec = ModelSpec::new(
            "custom-xai",
            "grok-custom",
            "Custom Grok",
            OPENAI_RESPONSES_API,
        );
        spec.reasoning = true;
        spec.sampling_params
            .insert("temperature".to_string(), json!(0.3));
        let configured = PreparedProvider {
            id: ProviderId::new("custom-xai"),
            name: None,
            api: Some(OPENAI_RESPONSES_API.to_string()),
            base_url: Some("https://api.x.ai/v1".to_string()),
            compat: None,
            api_key: Some("xai-key".to_string()),
            runtime_api_key: None,
            headers: BTreeMap::from([("X-Provider".to_string(), "provider".to_string())]),
            auth_header: Some(false),
            models: vec![PreparedModel {
                id: ModelId::new("grok-custom"),
                spec,
                headers: BTreeMap::new(),
            }],
            model_overrides: BTreeMap::new(),
            replace_models: false,
        };
        let transport = Arc::new(CapturingTransport::default());
        let provider = ModelsJsonProvider::with_transport(
            configured,
            None,
            HashSet::new(),
            Arc::new(ConfigValueResolver::default()),
            transport.clone(),
        );
        let (_, signal) = AbortHandle::new();
        let call_context = ProviderCallContext::without_plugins(
            "/project",
            ProviderId::new("custom-xai"),
            ModelId::new("grok-custom"),
        );

        let _stream = provider
            .stream(
                ProviderRequest {
                    model: ModelId::new("grok-custom"),
                    model_spec: None,
                    system_prompt: "system".to_string(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::High,
                    thinking_budgets: None,
                    max_output_tokens: Some(8),
                    headers: BTreeMap::from([("X-Request".to_string(), "request".to_string())]),
                    sampling_params: BTreeMap::new(),
                    session_id: None,
                },
                call_context,
                signal,
            )
            .await
            .unwrap();

        let captured = transport
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let body = captured.body.as_ref().unwrap();
        assert_eq!(captured.url, "https://api.x.ai/v1/responses");
        assert_eq!(captured.headers["Authorization"], "Bearer xai-key");
        assert_eq!(captured.headers["X-Provider"], "provider");
        assert_eq!(captured.headers["X-Request"], "request");
        assert_eq!(body["model"], "grok-custom");
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][0]["content"], "system");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["max_output_tokens"], 16);
        assert_eq!(body["temperature"], 0.3);
        assert_eq!(body["store"], false);
    }

    #[tokio::test]
    async fn routing_dispatches_google_generative_ai_models() {
        let mut spec = ModelSpec::new(
            "my-google",
            "gemma-4-31b-it",
            "Gemma 4",
            GOOGLE_GENERATIVE_AI_API,
        );
        spec.base_url = Some("https://generativelanguage.googleapis.com/v1beta".to_string());
        spec.reasoning = true;
        let configured = PreparedProvider {
            id: ProviderId::new("my-google"),
            name: None,
            api: Some(GOOGLE_GENERATIVE_AI_API.to_string()),
            base_url: spec.base_url.clone(),
            compat: None,
            api_key: Some("google-key".to_string()),
            runtime_api_key: None,
            headers: BTreeMap::new(),
            auth_header: Some(false),
            models: vec![PreparedModel {
                id: ModelId::new("gemma-4-31b-it"),
                spec,
                headers: BTreeMap::new(),
            }],
            model_overrides: BTreeMap::new(),
            replace_models: false,
        };
        let transport = Arc::new(CapturingTransport::default());
        let provider = ModelsJsonProvider::with_transport(
            configured,
            None,
            HashSet::new(),
            Arc::new(ConfigValueResolver::default()),
            transport.clone(),
        );
        let (_, signal) = AbortHandle::new();
        let _stream = provider
            .stream(
                ProviderRequest {
                    model: ModelId::new("gemma-4-31b-it"),
                    model_spec: None,
                    system_prompt: "system".to_string(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::High,
                    thinking_budgets: None,
                    max_output_tokens: Some(1_024),
                    headers: BTreeMap::new(),
                    sampling_params: BTreeMap::new(),
                    session_id: None,
                },
                ProviderCallContext::without_plugins(
                    "/project",
                    ProviderId::new("my-google"),
                    ModelId::new("gemma-4-31b-it"),
                ),
                signal,
            )
            .await
            .unwrap();
        let captured = transport
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        assert_eq!(
            captured.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemma-4-31b-it:streamGenerateContent?alt=sse"
        );
        assert_eq!(captured.headers["x-goog-api-key"], "google-key");
        assert_eq!(
            captured.body.as_ref().unwrap()["systemInstruction"]["parts"][0]["text"],
            "system"
        );
    }

    #[tokio::test]
    async fn routing_dispatches_all_new_models_json_protocols() {
        for (api, base_url, expected_url, auth_header) in [
            (
                MISTRAL_CONVERSATIONS_API,
                "https://mistral.example",
                "https://mistral.example/v1/chat/completions",
                "Authorization",
            ),
            (
                AZURE_OPENAI_RESPONSES_API,
                "https://demo.openai.azure.com",
                "https://demo.openai.azure.com/openai/v1/responses?api-version=v1",
                "api-key",
            ),
            (
                GOOGLE_VERTEX_API,
                "https://vertex.example/v1",
                "https://vertex.example/v1/publishers/google/models/custom-model:streamGenerateContent?alt=sse",
                "x-goog-api-key",
            ),
            (
                BEDROCK_CONVERSE_STREAM_API,
                "https://bedrock.example",
                "https://bedrock.example/model/custom-model/converse-stream",
                "Authorization",
            ),
        ] {
            let mut spec = ModelSpec::new("custom", "custom-model", "Custom Model", api);
            spec.base_url = Some(base_url.to_string());
            spec.sampling_params
                .insert("temperature".to_string(), json!(0.25));
            let configured = PreparedProvider {
                id: ProviderId::new("custom"),
                name: None,
                api: Some(api.to_string()),
                base_url: Some(base_url.to_string()),
                compat: None,
                api_key: Some("provider-secret".to_string()),
                runtime_api_key: None,
                headers: BTreeMap::new(),
                auth_header: Some(false),
                models: vec![PreparedModel {
                    id: ModelId::new("custom-model"),
                    spec,
                    headers: BTreeMap::new(),
                }],
                model_overrides: BTreeMap::new(),
                replace_models: false,
            };
            let transport = Arc::new(CapturingTransport::default());
            let provider = ModelsJsonProvider::with_transport(
                configured,
                None,
                HashSet::new(),
                Arc::new(ConfigValueResolver::default()),
                transport.clone(),
            );
            let (_, signal) = AbortHandle::new();
            let _stream = provider
                .stream(
                    ProviderRequest {
                        model: ModelId::new("custom-model"),
                        model_spec: None,
                        system_prompt: "system".to_string(),
                        messages: Vec::new(),
                        tools: Vec::new(),
                        thinking_level: ThinkingLevel::Off,
                        thinking_budgets: None,
                        max_output_tokens: Some(1_024),
                        headers: BTreeMap::new(),
                        sampling_params: BTreeMap::new(),
                        session_id: Some("session-1".to_string()),
                    },
                    ProviderCallContext::without_plugins(
                        "/project",
                        ProviderId::new("custom"),
                        ModelId::new("custom-model"),
                    ),
                    signal,
                )
                .await
                .unwrap();
            let captured = transport
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(captured.url, expected_url, "API {api}");
            assert!(
                captured
                    .headers
                    .get(auth_header)
                    .is_some_and(|value| value.contains("provider-secret")),
                "API {api}: {:?}",
                captured.headers
            );
            let body = captured.body.as_ref().unwrap();
            if matches!(api, MISTRAL_CONVERSATIONS_API | AZURE_OPENAI_RESPONSES_API) {
                assert_eq!(body["temperature"], 0.25, "API {api}");
            } else {
                assert!(body.get("temperature").is_none(), "API {api}");
            }
        }
    }

    #[tokio::test]
    async fn priced_models_calculate_cost_on_the_done_event() {
        let mut spec = ModelSpec::new("priced", "model", "Model", "openai-completions");
        spec.cost = ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.25,
            cache_write: 1.25,
            tiers: vec![pi_core::ModelCostTier {
                input_tokens_above: 200,
                input: 5.0,
                output: 6.0,
                cache_read: 1.0,
                cache_write: 6.25,
            }],
        };
        let configured = PreparedProvider {
            id: ProviderId::new("priced"),
            name: None,
            api: None,
            base_url: None,
            compat: None,
            api_key: None,
            runtime_api_key: None,
            headers: BTreeMap::new(),
            auth_header: Some(false),
            models: vec![PreparedModel {
                id: ModelId::new("model"),
                spec,
                headers: BTreeMap::new(),
            }],
            model_overrides: BTreeMap::new(),
            replace_models: false,
        };
        let provider = ModelsJsonProvider::new(
            configured,
            Some(Arc::new(UsageProvider)),
            HashSet::from(["openai-completions".to_string()]),
            Arc::new(ConfigValueResolver::default()),
        );
        let (_, signal) = AbortHandle::new();
        let stream = provider
            .stream(
                ProviderRequest {
                    model: ModelId::new("model"),
                    model_spec: None,
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::Off,
                    thinking_budgets: None,
                    max_output_tokens: None,
                    headers: BTreeMap::new(),
                    sampling_params: BTreeMap::new(),
                    session_id: None,
                },
                ProviderCallContext::without_plugins(
                    "/project",
                    ProviderId::new("priced"),
                    ModelId::new("model"),
                ),
                signal,
            )
            .await
            .unwrap();
        let events = stream.collect::<Vec<_>>().await;
        let usage = events
            .into_iter()
            .find_map(|event| match event.unwrap() {
                StreamEvent::Done { usage, .. } => Some(usage),
                _ => None,
            })
            .unwrap();

        assert_eq!(usage.cost.input, 0.000_5);
        assert_eq!(usage.cost.output, 0.000_12);
        assert_eq!(usage.cost.cache_read, 0.000_04);
        assert_eq!(usage.cost.cache_write, 0.000_612_5);
        assert_eq!(usage.cost.total, 0.001_272_5);
    }
}
