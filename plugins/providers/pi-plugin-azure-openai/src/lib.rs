#![forbid(unsafe_code)]

//! Azure OpenAI Responses provider, catalog, and reusable wire adapter.

mod catalog;

pub use catalog::azure_openai_models;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, PluginId, Provider, ProviderAvailability, ProviderCallContext, ProviderError,
    ProviderId, ProviderPlugin, ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_plugin_openai::responses;
use pi_provider::{
    HttpTransport, ReqwestTransport, TransportError, collect_body_limited,
    post_json_with_provider_hooks,
};

pub const AZURE_OPENAI_RESPONSES_API: &str = "azure-openai-responses";
pub const AZURE_PROVIDER_ID: &str = "azure-openai-responses";
const DEFAULT_API_VERSION: &str = "v1";

pub struct AzureOpenAiPlugin {
    provider: Arc<AzureOpenAiResponsesProvider>,
}

impl AzureOpenAiPlugin {
    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::new_with_transport(
            std::env::var("AZURE_OPENAI_API_KEY").ok().or(api_key),
            Arc::new(ReqwestTransport::new()),
        )
    }

    pub fn from_stored_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_transport(
            std::env::var("AZURE_OPENAI_API_KEY").ok().or(api_key),
            transport,
        )
    }

    pub fn new_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        let config = AzureOpenAiResponsesConfig::from_environment(api_key)?;
        Ok(Self {
            provider: Arc::new(AzureOpenAiResponsesProvider::with_transport(
                config, transport,
            )?),
        })
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for AzureOpenAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("azure-openai-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in azure_openai_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AzureOpenAiResponsesConfig {
    pub provider_id: ProviderId,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_version: String,
    pub deployment_names: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
}

impl AzureOpenAiResponsesConfig {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new(AZURE_PROVIDER_ID),
            base_url: Some(base_url.into()),
            api_key: Some(api_key.into()),
            api_version: DEFAULT_API_VERSION.to_string(),
            deployment_names: BTreeMap::new(),
            headers: BTreeMap::new(),
        }
    }

    pub fn without_api_key(base_url: impl Into<String>) -> Self {
        Self {
            api_key: None,
            ..Self::new(base_url, "")
        }
    }

    pub fn from_environment(api_key: Option<String>) -> Result<Self, ProviderError> {
        let base_url = std::env::var("AZURE_OPENAI_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("AZURE_OPENAI_RESOURCE_NAME")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .map(|resource| {
                        format!("https://{}.openai.azure.com/openai/v1", resource.trim())
                    })
            });
        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
        let deployment_names = parse_deployment_name_map(
            &std::env::var("AZURE_OPENAI_DEPLOYMENT_NAME_MAP").unwrap_or_default(),
        );
        let config = Self {
            provider_id: ProviderId::new(AZURE_PROVIDER_ID),
            base_url,
            api_key,
            api_version,
            deployment_names,
            headers: BTreeMap::new(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn provider_id(mut self, provider_id: impl Into<ProviderId>) -> Self {
        self.provider_id = provider_id.into();
        self
    }

    pub fn api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    pub fn deployment_name(
        mut self,
        model: impl Into<String>,
        deployment: impl Into<String>,
    ) -> Self {
        self.deployment_names
            .insert(model.into(), deployment.into());
        self
    }

    fn validate(&self) -> Result<(), ProviderError> {
        if self
            .base_url
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ProviderError::Failure(
                "Azure OpenAI base URL cannot be empty".to_string(),
            ));
        }
        if self.api_version.trim().is_empty() {
            return Err(ProviderError::Failure(
                "Azure OpenAI API version cannot be empty".to_string(),
            ));
        }
        if self
            .api_key
            .as_ref()
            .is_some_and(|key| key.contains(['\r', '\n']))
        {
            return Err(ProviderError::Failure(
                "invalid Azure OpenAI API key".to_string(),
            ));
        }
        Ok(())
    }
}

pub struct AzureOpenAiResponsesProvider {
    config: AzureOpenAiResponsesConfig,
    transport: Arc<dyn HttpTransport>,
}

impl AzureOpenAiResponsesProvider {
    pub fn new(config: AzureOpenAiResponsesConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: AzureOpenAiResponsesConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        config.validate()?;
        Ok(Self { config, transport })
    }

    fn headers(&self, request: &ProviderRequest) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        insert_header(&mut headers, "User-Agent", "pi-rs");
        if let Some(api_key) = &self.config.api_key {
            insert_header(&mut headers, "api-key", api_key);
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }

    fn endpoint(&self, request: &ProviderRequest) -> Result<String, ProviderError> {
        let base_url = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .filter(|value| !value.trim().is_empty())
            .or(self.config.base_url.as_deref())
            .ok_or_else(|| {
                ProviderError::Failure(
                    "Azure OpenAI base URL is required; set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME"
                        .to_string(),
                )
            })?;
        azure_responses_endpoint(base_url, &self.config.api_version)
    }
}

#[async_trait]
impl Provider for AzureOpenAiResponsesProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id.clone()
    }

    fn name(&self) -> String {
        if self.config.provider_id.as_str() == AZURE_PROVIDER_ID {
            "Azure OpenAI".to_string()
        } else {
            self.config.provider_id.to_string()
        }
    }

    fn availability(&self) -> ProviderAvailability {
        if self.config.api_key.is_some() && self.config.base_url.is_some() {
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
        let endpoint = self.endpoint(&request)?;
        let headers = self.headers(&request);
        let original_model = request.model.clone();
        let mut wire_request = request;
        if let Some(deployment) = self.config.deployment_names.get(original_model.as_str()) {
            wire_request.model = deployment.clone().into();
        }
        let payload = context
            .before_provider_request(&signal, responses::request_body(&wire_request))
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
            return Err(ProviderError::Failure(format!(
                "Azure OpenAI API error ({status}): {body}"
            )));
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
        Ok(responses::stream(
            self.config.provider_id.clone(),
            original_model,
            AZURE_OPENAI_RESPONSES_API,
            response.body,
            signal,
        ))
    }
}

fn azure_responses_endpoint(base_url: &str, api_version: &str) -> Result<String, ProviderError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let (scheme, remainder) = trimmed.split_once("://").ok_or_else(|| {
        ProviderError::Failure(format!("invalid Azure OpenAI base URL: {base_url}"))
    })?;
    if !matches!(scheme, "http" | "https") || remainder.is_empty() || remainder.contains('#') {
        return Err(ProviderError::Failure(format!(
            "invalid Azure OpenAI base URL: {base_url}"
        )));
    }
    let (without_query, query) = remainder
        .split_once('?')
        .map_or((remainder, None), |(path, query)| (path, Some(query)));
    let (authority, path) = without_query
        .split_once('/')
        .map_or((without_query, ""), |(authority, path)| (authority, path));
    if authority.is_empty() {
        return Err(ProviderError::Failure(format!(
            "invalid Azure OpenAI base URL: {base_url}"
        )));
    }
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or(authority)
        .to_ascii_lowercase();
    let azure_host = [
        ".openai.azure.com",
        ".cognitiveservices.azure.com",
        ".ai.azure.com",
    ]
    .iter()
    .any(|suffix| host.ends_with(suffix));
    let normalized_path = path.trim_matches('/');
    let base_path =
        if azure_host && matches!(normalized_path, "" | "openai" | "openai/v1/responses") {
            "openai/v1".to_string()
        } else if normalized_path.ends_with("/responses") {
            normalized_path.trim_end_matches("/responses").to_string()
        } else {
            normalized_path.to_string()
        };
    let path = if base_path.is_empty() {
        "responses".to_string()
    } else {
        format!("{base_path}/responses")
    };
    let mut parameters = query
        .map(|query| {
            query
                .split('&')
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !parameters.iter().any(|entry| {
        entry
            .split_once('=')
            .map_or(entry.as_str(), |(name, _)| name)
            .eq_ignore_ascii_case("api-version")
    }) {
        parameters.push(format!("api-version={}", api_version.trim()));
    }
    let query = if parameters.is_empty() {
        String::new()
    } else {
        format!("?{}", parameters.join("&"))
    };
    Ok(format!("{scheme}://{authority}/{path}{query}"))
}

fn parse_deployment_name_map(value: &str) -> BTreeMap<String, String> {
    value
        .split(',')
        .filter_map(|entry| {
            let (model, deployment) = entry.trim().split_once('=')?;
            let model = model.trim();
            let deployment = deployment.trim();
            (!model.is_empty() && !deployment.is_empty())
                .then(|| (model.to_string(), deployment.to_string()))
        })
        .collect()
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

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        error => ProviderError::Failure(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::{StreamExt, stream};
    use pi_core::{
        AbortHandle, ModelId, ModelSpec, ProviderCallContext, StreamEvent, ThinkingLevel,
    };
    use pi_provider::{HttpResponse, TransportError};
    use serde_json::Value;

    use super::*;

    #[derive(Default)]
    struct Capture {
        url: String,
        headers: BTreeMap<String, String>,
        body: Value,
    }

    struct CapturingTransport(Arc<Mutex<Capture>>);

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self.0.lock().unwrap() = Capture {
                url: url.to_string(),
                headers: headers.clone(),
                body: body.clone(),
            };
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/event-stream".to_string()),
                headers: Vec::new(),
                body: Box::pin(stream::iter([Ok(
                    b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
                        .to_vec(),
                )])),
            })
        }
    }

    #[test]
    fn normalizes_azure_hosts_and_preserves_explicit_api_version() {
        assert_eq!(
            azure_responses_endpoint("https://demo.openai.azure.com", "2026-01-01").unwrap(),
            "https://demo.openai.azure.com/openai/v1/responses?api-version=2026-01-01"
        );
        assert_eq!(
            azure_responses_endpoint(
                "https://gateway.example/openai/v1?api-version=preview&x=1",
                "v1"
            )
            .unwrap(),
            "https://gateway.example/openai/v1/responses?api-version=preview&x=1"
        );
    }

    #[test]
    fn parses_deployment_mapping_like_current_pi() {
        assert_eq!(
            parse_deployment_name_map("gpt-5=prod, bad, gpt-4 = legacy "),
            BTreeMap::from([
                ("gpt-4".to_string(), "legacy".to_string()),
                ("gpt-5".to_string(), "prod".to_string()),
            ])
        );
    }

    #[tokio::test]
    async fn sends_api_key_and_deployment_through_responses_wire_adapter() {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let provider = AzureOpenAiResponsesProvider::with_transport(
            AzureOpenAiResponsesConfig::new("https://demo.openai.azure.com", "azure-secret")
                .api_version("2026-01-01")
                .deployment_name("gpt-5", "production-gpt-5"),
            Arc::new(CapturingTransport(Arc::clone(&capture))),
        )
        .unwrap();
        let mut spec = ModelSpec::new(
            AZURE_PROVIDER_ID,
            "gpt-5",
            "GPT-5",
            AZURE_OPENAI_RESPONSES_API,
        );
        spec.reasoning = true;
        let request = ProviderRequest {
            model: ModelId::new("gpt-5"),
            model_spec: Some(spec),
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::High,
            thinking_budgets: None,
            max_output_tokens: Some(1),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: Some("session-1".to_string()),
        };
        let (_, signal) = AbortHandle::new();
        let events = provider
            .stream(
                request,
                ProviderCallContext::without_plugins(
                    ".",
                    ProviderId::new(AZURE_PROVIDER_ID),
                    ModelId::new("gpt-5"),
                ),
                signal,
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            events.first(),
            Some(Ok(StreamEvent::Start { metadata }))
                if metadata.api == AZURE_OPENAI_RESPONSES_API
        ));
        let capture = capture.lock().unwrap();
        assert_eq!(
            capture.url,
            "https://demo.openai.azure.com/openai/v1/responses?api-version=2026-01-01"
        );
        assert_eq!(capture.headers["api-key"], "azure-secret");
        assert!(!capture.headers.contains_key("Authorization"));
        assert_eq!(capture.body["model"], "production-gpt-5");
        assert_eq!(capture.body["max_output_tokens"], 16);
    }
}
