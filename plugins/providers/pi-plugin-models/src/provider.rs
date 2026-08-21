use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, ModelId, Provider, ProviderCallContext, ProviderError, ProviderId,
    ProviderRequest, ProviderStream,
};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};

use crate::config::{PreparedModel, PreparedOverride, PreparedProvider};
use crate::resolver::{ConfigValueResolver, ResolveError};

pub(crate) struct ModelsJsonProvider {
    configured: PreparedProvider,
    fallback: Option<Arc<dyn Provider>>,
    resolver: Arc<ConfigValueResolver>,
}

impl ModelsJsonProvider {
    pub fn new(
        configured: PreparedProvider,
        fallback: Option<Arc<dyn Provider>>,
        resolver: Arc<ConfigValueResolver>,
    ) -> Self {
        Self {
            configured,
            fallback,
            resolver,
        }
    }

    fn model(&self, id: &ModelId) -> Option<&PreparedModel> {
        self.configured.models.iter().find(|model| &model.id == id)
    }

    fn model_override(&self, id: &ModelId) -> Option<&PreparedOverride> {
        self.configured.model_overrides.get(id)
    }

    async fn resolve_headers(
        &self,
        model_id: &ModelId,
        model: Option<&PreparedModel>,
        model_override: Option<&PreparedOverride>,
        signal: &AbortSignal,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
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
        if self.configured.auth_header && api_key.is_none() {
            return Err(ProviderError::Failure(format!(
                "provider {}: authHeader requires a resolved API key",
                self.configured.id
            )));
        }
        if let Some(api_key) = api_key {
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
        Ok(headers)
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
        model: Option<&PreparedModel>,
        model_override: Option<&PreparedOverride>,
        request: &mut ProviderRequest,
    ) {
        let max_tokens = model
            .map(|model| model.spec.max_tokens)
            .or_else(|| model_override.and_then(|value| value.max_tokens));
        if request.max_output_tokens.is_none() {
            request.max_output_tokens = max_tokens;
        }

        let mut sampling_params = model
            .map(|model| model.spec.sampling_params.clone())
            .or_else(|| model_override.map(|value| value.sampling_params.clone()))
            .unwrap_or_default();
        let reasoning = model
            .map(|model| model.spec.reasoning)
            .or_else(|| model_override.and_then(|value| value.reasoning))
            .unwrap_or(false);
        let thinking_level_map = model
            .map(|model| &model.spec.thinking_level_map)
            .or_else(|| model_override.map(|value| &value.thinking_level_map));
        if reasoning {
            let level = request.thinking_level.as_str();
            match thinking_level_map.and_then(|map| map.get(level)) {
                Some(Some(mapped)) => {
                    sampling_params.insert("reasoning_effort".to_string(), mapped.clone().into());
                }
                Some(None) => {}
                None if level != "off" => {
                    sampling_params.insert("reasoning_effort".to_string(), level.into());
                }
                None => {}
            }
        }
        sampling_params.extend(std::mem::take(&mut request.sampling_params));
        request.sampling_params = sampling_params;
    }
}

#[async_trait]
impl Provider for ModelsJsonProvider {
    fn id(&self) -> ProviderId {
        self.configured.id.clone()
    }

    async fn stream(
        &self,
        mut request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let model = self.model(&request.model);
        let model_override = self.model_override(&request.model);
        Self::apply_model_defaults(model, model_override, &mut request);
        let configured_headers = self
            .resolve_headers(&request.model, model, model_override, &signal)
            .await?;
        let request_headers = std::mem::take(&mut request.headers);
        request.headers = configured_headers;
        for (name, value) in request_headers {
            insert_header(&mut request.headers, name, value);
        }

        let base_url = model
            .and_then(|model| model.spec.base_url.as_deref())
            .or(self.configured.base_url.as_deref());
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
            if api != "openai-completions" {
                return Err(ProviderError::Failure(format!(
                    "provider {} has no implementation for API {api:?}",
                    self.configured.id
                )));
            }
            let provider = OpenAiCompatibleProvider::new(
                OpenAiCompatibleConfig::without_api_key(base_url)
                    .provider_id(self.configured.id.clone()),
            )?;
            return provider.stream(request, context, signal).await;
        }
        match &self.fallback {
            Some(fallback) => fallback.stream(request, context, signal).await,
            None => Err(ProviderError::Failure(format!(
                "provider {} has no baseUrl or lower-layer provider for model {}",
                self.configured.id, request.model
            ))),
        }
    }
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

    use futures::stream;
    use pi_core::{AbortHandle, ModelSpec, ThinkingLevel};
    use serde_json::json;

    use super::*;

    struct CapturingProvider {
        request: Arc<Mutex<Option<ProviderRequest>>>,
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
            api: None,
            base_url: None,
            api_key: Some("models-json-key".to_string()),
            runtime_api_key: Some("runtime-key".to_string()),
            headers: BTreeMap::from([("X-Provider".to_string(), "provider".to_string())]),
            auth_header: false,
            models: vec![PreparedModel {
                id: ModelId::new("model"),
                spec,
                headers: BTreeMap::from([("X-Model".to_string(), "model".to_string())]),
            }],
            model_overrides: BTreeMap::new(),
        };
        let captured = Arc::new(Mutex::new(None));
        let fallback: Arc<dyn Provider> = Arc::new(CapturingProvider {
            request: Arc::clone(&captured),
        });
        let provider = ModelsJsonProvider::new(
            configured,
            Some(fallback),
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
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::High,
                    max_output_tokens: None,
                    headers: BTreeMap::from([("X-Request".to_string(), "request".to_string())]),
                    sampling_params: BTreeMap::new(),
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
        assert_eq!(request.sampling_params["reasoning_effort"], "medium");
    }
}
