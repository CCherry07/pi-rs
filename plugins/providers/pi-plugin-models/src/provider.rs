use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{AbortSignal, ModelCost, ModelId, ProviderId, StreamEvent};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderRequest,
    ProviderStream,
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
#[cfg(test)]
use pi_provider::ReqwestTransport;
use pi_provider::{HttpTransport, insert_header};
use serde_json::Value;

use crate::config::{PreparedModel, PreparedOverride, PreparedProvider};
use crate::resolver::{ConfigValueResolver, ResolveError};

pub(crate) struct ModelsJsonProvider {
    configured: PreparedProvider,
    fallback: Option<Arc<dyn Provider>>,
    fallback_apis: HashSet<String>,
    resolver: Arc<ConfigValueResolver>,
    transport: Arc<dyn HttpTransport>,
    credentials: Option<crate::ModelCredentialSource>,
}

struct ResolvedRequestConfig {
    api_key: Option<String>,
    headers: BTreeMap<String, String>,
    provider_env: BTreeMap<String, String>,
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
            credentials: None,
        }
    }

    pub(crate) fn with_credentials(mut self, source: Option<crate::ModelCredentialSource>) -> Self {
        self.credentials = source;
        self
    }

    fn model(&self, id: &ModelId) -> Option<&PreparedModel> {
        self.configured
            .models
            .iter()
            .find(|model| model.kind == pi_core::ModelType::Chat && &model.id == id)
    }

    fn model_override(&self, id: &ModelId) -> Option<&PreparedOverride> {
        self.configured.model_overrides.get(id)
    }

    async fn resolve_request_config(
        &self,
        model_id: &ModelId,
        model: Option<&PreparedModel>,
        model_override: Option<&PreparedOverride>,
        use_credential_source: bool,
        signal: &AbortSignal,
    ) -> Result<ResolvedRequestConfig, ProviderError> {
        let mut headers = self
            .resolve_header_set(
                &self.configured.headers,
                &format!("provider {}", self.configured.id),
                signal,
            )
            .await?;
        let mut api_key = match &self.configured.runtime_api_key {
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
        let mut provider_env = BTreeMap::new();
        if use_credential_source && let Some(source) = &self.credentials {
            let credential = if api_key.is_none() {
                source.source.resolve(signal).await?
            } else {
                // Routing metadata may still be needed with an explicit key,
                // but an unused OAuth credential must not be refreshed.
                source.source.check(signal).await?
            };
            if let Some(env) = credential
                .as_ref()
                .and_then(|value| value.metadata.get("env"))
                .and_then(Value::as_object)
            {
                provider_env.extend(env.iter().filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                }));
            }
            if api_key.is_none()
                && model.is_some_and(|model| model.spec.api == "openai-decisions")
                && credential
                    .as_ref()
                    .is_some_and(|value| value.kind == pi_provider::CredentialKind::OAuth)
            {
                return Err(ProviderError::Failure(
                    "OpenAI Decisions requires an API key; OAuth is not supported".into(),
                ));
            }
            if api_key.is_none() {
                api_key = credential
                    .map(|value| value.secret)
                    .filter(|key| !key.trim().is_empty());
            }
        }
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
        Ok(ResolvedRequestConfig {
            api_key,
            headers,
            provider_env,
        })
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

    async fn operation_client<'a>(
        &'a self,
        kind: pi_core::ModelType,
        id: &ModelId,
        context: &'a ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<crate::operations::OperationClient<'a>, ProviderError> {
        let prepared = self
            .configured
            .models
            .iter()
            .find(|m| m.kind == kind && &m.id == id);
        let resolved = self
            .resolve_request_config(id, prepared, self.model_override(id), true, &signal)
            .await?;
        let key = resolved.api_key;
        // llama-server supports unauthenticated deployments. All remote API
        // protocols keep their existing API-key requirement.
        let optional_auth = prepared.is_some_and(|model| model.spec.api == "llama-cpp-classify");
        if key.is_none() && !optional_auth {
            return Err(ProviderError::Failure(format!(
                "No API key for provider: {}",
                self.configured.id
            )));
        }
        let mut headers = BTreeMap::new();
        if let Some(key) = key {
            insert_header(&mut headers, "Authorization", format!("Bearer {key}"));
        }
        insert_header(&mut headers, "Content-Type", "application/json");
        for (name, value) in resolved.headers {
            insert_header(&mut headers, name, value);
        }
        Ok(crate::operations::OperationClient {
            transport: self.transport.as_ref(),
            context,
            headers,
            provider_env: resolved.provider_env,
            signal,
        })
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
                .map(|model| {
                    let mut params = model.spec.sampling_params.clone();
                    // Resolve the Pi level before any provider-specific level map.
                    // Explicit request params are merged last below.
                    let level = model.spec.clamp_thinking_level(request.thinking_level);
                    if let Some(overrides) = model
                        .spec
                        .sampling_params_by_thinking_level
                        .as_ref()
                        .and_then(|levels| levels.get(level.as_str()))
                    {
                        params.extend(overrides.clone());
                    }
                    params
                })
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

    fn model_availability(&self, model: &pi_core::AnyModelSpec) -> ProviderAvailability {
        if model.model_type() == pi_core::ModelType::Chat {
            return if self.configured.runtime_api_key.is_none() && self.configured.api_key.is_none()
            {
                self.fallback.as_ref().map_or_else(
                    || self.availability(),
                    |provider| provider.model_availability(model),
                )
            } else {
                self.availability()
            };
        }
        if matches!(model, pi_core::AnyModelSpec::Classifier(model)
            if model.api == "llama-cpp-classify" && model.base_url.as_deref().is_some_and(|url| !url.is_empty()) && !self.configured.auth_header.unwrap_or(false))
            || self.configured.runtime_api_key.is_some()
            || self.configured.api_key.is_some()
            || self
                .credentials
                .as_ref()
                .is_some_and(|source| source.available)
        {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn filter_available_models(
        &self,
        models: Vec<pi_core::AnyModelSpec>,
        signal: AbortSignal,
    ) -> Result<Vec<pi_core::AnyModelSpec>, ProviderError> {
        if signal.is_aborted() {
            return Err(ProviderError::Aborted);
        }
        let credential = match &self.credentials {
            Some(source) => source.source.check(&signal).await?,
            None => None,
        };
        let configured = self.configured.runtime_api_key.is_some()
            || self
                .configured
                .api_key
                .as_deref()
                .is_some_and(|key| self.resolver.is_configured(key));
        let inherited = match &self.fallback {
            Some(fallback) => {
                fallback
                    .filter_available_models(models.clone(), signal)
                    .await?
            }
            None => vec![],
        };
        Ok(models.into_iter().filter(|model| {
            let base_url = match model {
                pi_core::AnyModelSpec::Chat(spec) => spec.base_url.as_deref(),
                pi_core::AnyModelSpec::Classifier(spec) => spec.base_url.as_deref(),
                pi_core::AnyModelSpec::Image(spec) => spec.base_url.as_deref(),
            };
            for name in ["CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_GATEWAY_ID"] {
                if base_url.is_some_and(|url| url.contains(&format!("{{{name}}}"))) {
                    let configured_env = credential.as_ref().and_then(|value| value.metadata.get("env")).and_then(|env| env.get(name)).and_then(Value::as_str);
                    let value = configured_env.map(str::to_owned).or_else(|| std::env::var(name).ok());
                    if value.is_none_or(|value| value.is_empty()) { return false; }
                }
            }
            let oauth = credential.as_ref().is_some_and(|value| value.kind == pi_provider::CredentialKind::OAuth);
            // This protocol rejects subscription tokens even when the chat
            // provider is available. Never leak those tokens into its catalog.
            if !configured && oauth && matches!(model, pi_core::AnyModelSpec::Classifier(spec) if spec.api == "openai-decisions") {
                return false;
            }
            if configured {
                return true;
            }
            if model.model_type() == pi_core::ModelType::Chat {
                return if self.fallback.is_some() {
                    inherited.iter().any(|candidate| candidate.model_type() == model.model_type() && candidate.id() == model.id())
                } else {
                    credential.as_ref().is_some_and(|value| !value.secret.trim().is_empty())
                };
            }
            credential.as_ref().is_some_and(|value| !value.secret.trim().is_empty()) || matches!(model, pi_core::AnyModelSpec::Classifier(spec)
                if spec.api == "llama-cpp-classify" && spec.base_url.as_deref().is_some_and(|url| !url.is_empty()) && !self.configured.auth_header.unwrap_or(false))
        }).collect())
    }

    async fn classify(
        &self,
        model: pi_core::ClassifierModelSpec,
        request: pi_core::ClassifierContext,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<pi_core::ClassifierResult, ProviderError> {
        let client = self
            .operation_client(pi_core::ModelType::Classifier, &model.id, &context, signal)
            .await?;
        Ok(crate::operations::classify(model, request, client).await)
    }

    async fn generate_images(
        &self,
        model: pi_core::ImageModelSpec,
        request: pi_core::ImagesContext,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<pi_core::ImagesResult, ProviderError> {
        let client = self
            .operation_client(pi_core::ModelType::Image, &model.id, &context, signal)
            .await?;
        Ok(crate::operations::generate_images(model, request, client).await)
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
            .resolve_request_config(
                &request.model,
                model,
                model_override,
                self.fallback.is_none(),
                &signal,
            )
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

    struct FixtureCredential;

    #[async_trait]
    impl pi_provider::CredentialSource for FixtureCredential {
        async fn resolve(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<pi_provider::ProviderCredential>, ProviderError> {
            Ok(Some(pi_provider::ProviderCredential {
                secret: "chatgpt-access-token".into(),
                kind: pi_provider::CredentialKind::OAuth,
                metadata: BTreeMap::new(),
            }))
        }
    }

    #[tokio::test]
    async fn responses_models_json_overlays_reach_direct_and_oauth_fallback_requests() {
        for (key, budget) in [(Some("sk-fixture"), Some(2_048)), (None, None)] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("models.json");
            std::fs::write(&path, json!({"providers": {"openai": {
                "apiKey": key,
                "baseUrl": "https://api.openai.com/v1",
                "api": "openai-responses",
                "compat": {"supportsMaxOutputTokens": false, "promptCacheKeyMode": "disabled"},
                "models": [{"id": "custom-model", "maxTokens": 2_048}],
                "modelOverrides": {"custom-model": {"compat": {"supportsMaxOutputTokens": true}}}
            }}}).to_string()).unwrap();
            let transport = Arc::new(CapturingTransport::default());
            let builtin = pi_plugin_openai::OpenAiPlugin::from_stored_with_transport(
                Some("chatgpt-access-token".into()),
                Arc::new(FixtureCredential),
                transport.clone(),
            )
            .unwrap();
            let models = crate::ModelsPlugin::load_with_transport(
                crate::ModelsPluginOptions::new(&path),
                transport.clone(),
            )
            .unwrap();
            let (_, _, registries) = pi_plugin::RegistriesBuilder::new()
                .register_plugin_sets(Vec::new(), vec![Arc::new(builtin), Arc::new(models)])
                .unwrap();
            let provider = registries.provider(&ProviderId::new("openai")).unwrap();
            let (_, signal) = AbortHandle::new();
            let _stream = provider
                .stream(
                    ProviderRequest {
                        model: ModelId::new("custom-model"),
                        model_spec: None,
                        system_prompt: String::new(),
                        messages: Vec::new(),
                        tools: Vec::new(),
                        thinking_level: ThinkingLevel::Off,
                        thinking_budgets: None,
                        max_output_tokens: None,
                        cache_retention: None,
                        headers: BTreeMap::new(),
                        sampling_params: BTreeMap::new(),
                        session_id: Some("session".into()),
                    },
                    ProviderCallContext::without_plugins(
                        "/",
                        "openai".into(),
                        "custom-model".into(),
                    ),
                    signal,
                )
                .await
                .unwrap();
            let captured = transport.request.lock().unwrap();
            assert_eq!(captured.url, "https://api.openai.com/v1/responses");
            assert_eq!(
                captured.headers["Authorization"],
                format!("Bearer {}", key.unwrap_or("chatgpt-access-token"))
            );
            let body = captured.body.as_ref().unwrap();
            assert_eq!(
                body.get("max_output_tokens")
                    .and_then(serde_json::Value::as_u64),
                budget
            );
            assert!(body.get("prompt_cache_key").is_none());
            assert!(body.get("prompt_cache_retention").is_none() || key.is_some());
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
                catalog_headers: BTreeMap::new(),
                kind: pi_core::ModelType::Chat,
                output: Vec::new(),
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
                    cache_retention: None,
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
            "custom-provider",
            "custom-claude",
            "Custom Claude",
            ANTHROPIC_MESSAGES_API,
        );
        spec.reasoning = true;
        spec.compat = Some(json!({"forceAdaptiveThinking": true}));
        let configured = PreparedProvider {
            id: ProviderId::new("custom-provider"),
            name: None,
            api: Some(ANTHROPIC_MESSAGES_API.to_string()),
            base_url: Some("https://gateway.example/v1".to_string()),
            compat: None,
            api_key: Some("configured-key".to_string()),
            runtime_api_key: None,
            headers: BTreeMap::from([("X-Provider".to_string(), "provider".to_string())]),
            auth_header: Some(true),
            models: vec![PreparedModel {
                catalog_headers: BTreeMap::new(),
                kind: pi_core::ModelType::Chat,
                output: Vec::new(),
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
            ProviderId::new("custom-provider"),
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
                    cache_retention: None,
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
                catalog_headers: BTreeMap::new(),
                kind: pi_core::ModelType::Chat,
                output: Vec::new(),
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
                    cache_retention: None,
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
                catalog_headers: BTreeMap::new(),
                kind: pi_core::ModelType::Chat,
                output: Vec::new(),
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
                    cache_retention: None,
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
                    catalog_headers: BTreeMap::new(),
                    kind: pi_core::ModelType::Chat,
                    output: Vec::new(),
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
                        cache_retention: None,
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
                catalog_headers: BTreeMap::new(),
                kind: pi_core::ModelType::Chat,
                output: Vec::new(),
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
                    cache_retention: None,
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
