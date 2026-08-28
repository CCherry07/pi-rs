#![forbid(unsafe_code)]

mod messages;
mod oauth;

pub use oauth::{OAuthCredential, OAuthStart, complete_oauth, refresh, start_oauth};

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use messages::{AnthropicMode, compatibility, request_body, stream as messages_stream};
use pi_core::{
    AbortSignal, ModelCost, ModelInput, ModelSpec, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream,
};
use pi_provider::{HttpTransport, ReqwestTransport, TransportError, collect_body_limited};
use serde_json::json;

pub use messages::AnthropicMessagesCompat;

const PROVIDER_ID: &str = "anthropic";
pub const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

#[derive(Clone)]
enum AnthropicCredential {
    ApiKey(String),
    Bearer(String),
    ClaudeCodeOAuth(String),
}

#[derive(Debug, Clone)]
pub struct AnthropicCompatibleConfig {
    pub provider_id: ProviderId,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl AnthropicCompatibleConfig {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("anthropic-compatible"),
            base_url: base_url.into(),
            api_key: Some(api_key.into()),
            headers: BTreeMap::new(),
        }
    }

    pub fn without_api_key(base_url: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("anthropic-compatible"),
            base_url: base_url.into(),
            api_key: None,
            headers: BTreeMap::new(),
        }
    }

    pub fn provider_id(mut self, id: impl Into<ProviderId>) -> Self {
        self.provider_id = id.into();
        self
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

/// Independent Anthropic provider plugin with Claude Code OAuth-token request shaping.
pub struct AnthropicPlugin {
    provider: Arc<AnthropicProvider>,
}

impl AnthropicPlugin {
    pub fn discover() -> Self {
        Self::from_stored(None)
    }

    pub fn from_stored(stored: Option<(&str, bool)>) -> Self {
        let credential = env("ANTHROPIC_AUTH_TOKEN")
            .map(AnthropicCredential::Bearer)
            .or_else(|| env("ANTHROPIC_OAUTH_TOKEN").map(AnthropicCredential::ClaudeCodeOAuth))
            .or_else(|| env("ANTHROPIC_API_KEY").map(AnthropicCredential::ApiKey))
            .or_else(|| {
                stored.map(|(secret, oauth)| {
                    if oauth {
                        AnthropicCredential::ClaudeCodeOAuth(secret.to_string())
                    } else {
                        AnthropicCredential::ApiKey(secret.to_string())
                    }
                })
            });
        Self {
            provider: Arc::new(AnthropicProvider::new(credential)),
        }
    }

    pub fn with_api_key(api_key: impl Into<String>) -> Self {
        Self {
            provider: Arc::new(AnthropicProvider::new(Some(AnthropicCredential::ApiKey(
                api_key.into(),
            )))),
        }
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for AnthropicPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("anthropic-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in anthropic_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

pub struct AnthropicProvider {
    credential: Option<AnthropicCredential>,
    transport: Arc<dyn HttpTransport>,
}

pub struct AnthropicCompatibleProvider {
    config: AnthropicCompatibleConfig,
    transport: Arc<dyn HttpTransport>,
}

impl AnthropicCompatibleProvider {
    pub fn new(config: AnthropicCompatibleConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: AnthropicCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        validate_compatible_config(&config)?;
        Ok(Self { config, transport })
    }

    pub(crate) fn endpoint(&self) -> String {
        messages_endpoint(&self.config.base_url)
    }

    fn headers(&self, request: &ProviderRequest) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        insert_header(&mut headers, "anthropic-version", "2023-06-01");
        if let Some(api_key) = &self.config.api_key {
            insert_header(&mut headers, "x-api-key", api_key);
        }
        apply_compat_headers(&mut headers, request);
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }
}

impl AnthropicProvider {
    fn new(credential: Option<AnthropicCredential>) -> Self {
        Self::with_transport(credential, Arc::new(ReqwestTransport::new()))
    }

    fn with_transport(
        credential: Option<AnthropicCredential>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            credential,
            transport,
        }
    }

    fn is_oauth(&self) -> bool {
        matches!(
            self.credential,
            Some(AnthropicCredential::ClaudeCodeOAuth(_))
        )
    }

    fn headers(
        &self,
        request: &ProviderRequest,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ProviderError::Failure(
                "Anthropic requires ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, or ANTHROPIC_OAUTH_TOKEN"
                    .to_string(),
            )
        })?;
        let mut headers = BTreeMap::from([
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]);
        match credential {
            AnthropicCredential::ApiKey(key) => {
                headers.insert("x-api-key".to_string(), key.clone());
            }
            AnthropicCredential::Bearer(token) => {
                headers.insert("Authorization".to_string(), format!("Bearer {token}"));
            }
            AnthropicCredential::ClaudeCodeOAuth(token) => {
                headers.insert("Authorization".to_string(), format!("Bearer {token}"));
                headers.insert(
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14".to_string(),
                );
                headers.insert("user-agent".to_string(), "claude-cli/2.1.75".to_string());
                headers.insert("x-app".to_string(), "cli".to_string());
                headers.insert(
                    "anthropic-dangerous-direct-browser-access".to_string(),
                    "true".to_string(),
                );
            }
        }
        apply_compat_headers(&mut headers, request);
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        Ok(headers)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn availability(&self) -> ProviderAvailability {
        if self.credential.is_some() {
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
        let headers = self.headers(&request)?;
        let mode = if self.is_oauth() {
            AnthropicMode::ClaudeCode
        } else {
            AnthropicMode::Standard
        };
        let endpoint = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .map(messages_endpoint)
            .unwrap_or_else(|| ENDPOINT.to_string());
        stream_messages(
            &self.transport,
            ProviderId::new(PROVIDER_ID),
            &endpoint,
            headers,
            request.model.clone(),
            request,
            context,
            signal,
            mode,
        )
        .await
    }
}

fn messages_endpoint(base: &str) -> String {
    let base = base.trim();
    let suffix_start = [base.find('?'), base.find('#')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(base.len());
    let (path, suffix) = base.split_at(suffix_start);
    let path = path.trim_end_matches('/');
    if path.ends_with("/messages") {
        format!("{path}{suffix}")
    } else if path.ends_with("/v1") {
        format!("{path}/messages{suffix}")
    } else {
        format!("{path}/v1/messages{suffix}")
    }
}

#[async_trait]
impl Provider for AnthropicCompatibleProvider {
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
        stream_messages(
            &self.transport,
            self.config.provider_id.clone(),
            &self.endpoint(),
            headers,
            request.model.clone(),
            request,
            context,
            signal,
            AnthropicMode::Standard,
        )
        .await
    }
}

pub fn anthropic_models() -> Vec<ModelSpec> {
    vec![
        model(
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            200_000,
            64_000,
            1.0,
            5.0,
            0.1,
            1.25,
        ),
        model(
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            1_000_000,
            128_000,
            3.0,
            15.0,
            0.3,
            3.75,
        ),
        model(
            "claude-opus-4-6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            5.0,
            25.0,
            0.5,
            6.25,
        ),
        model(
            "claude-opus-4-7",
            "Claude Opus 4.7",
            1_000_000,
            128_000,
            5.0,
            25.0,
            0.5,
            6.25,
        ),
        model(
            "claude-opus-4-8",
            "Claude Opus 4.8",
            1_000_000,
            128_000,
            5.0,
            25.0,
            0.5,
            6.25,
        ),
        model(
            "claude-sonnet-5",
            "Claude Sonnet 5",
            1_000_000,
            128_000,
            2.0,
            10.0,
            0.2,
            2.5,
        ),
        model(
            "claude-opus-5",
            "Claude Opus 5",
            1_000_000,
            128_000,
            5.0,
            25.0,
            0.5,
            6.25,
        ),
        model(
            "claude-fable-5",
            "Claude Fable 5",
            1_000_000,
            128_000,
            10.0,
            50.0,
            1.0,
            12.5,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn model(
    id: &str,
    name: &str,
    context: u64,
    output: u64,
    input_cost: f64,
    output_cost: f64,
    cache_read: f64,
    cache_write: f64,
) -> ModelSpec {
    let mut model = ModelSpec::new(PROVIDER_ID, id, name, ANTHROPIC_MESSAGES_API);
    model.base_url = Some("https://api.anthropic.com".to_string());
    model.reasoning = true;
    model.input = vec![ModelInput::Text, ModelInput::Image];
    model.cost = ModelCost {
        input: input_cost,
        output: output_cost,
        cache_read,
        cache_write,
        tiers: Vec::new(),
    };
    model.context_window = context;
    model.max_tokens = output;
    let mut compat = json!({
        "supportsEagerToolInputStreaming": true,
        "supportsStrictTools": true,
        "supportsToolReferences": !id.contains("haiku")
    });
    if matches!(
        id,
        "claude-sonnet-4-6"
            | "claude-opus-4-6"
            | "claude-opus-4-7"
            | "claude-opus-4-8"
            | "claude-sonnet-5"
            | "claude-opus-5"
            | "claude-fable-5"
    ) {
        compat["forceAdaptiveThinking"] = json!(true);
    }
    if matches!(id, "claude-opus-4-7" | "claude-opus-4-8" | "claude-opus-5") {
        compat["supportsTemperature"] = json!(false);
    }
    match id {
        "claude-opus-4-6" | "claude-sonnet-4-6" => {
            model
                .thinking_level_map
                .insert("max".to_string(), Some("max".to_string()));
        }
        "claude-opus-4-7" | "claude-opus-4-8" | "claude-opus-5" | "claude-sonnet-5" => {
            model
                .thinking_level_map
                .insert("xhigh".to_string(), Some("xhigh".to_string()));
            model
                .thinking_level_map
                .insert("max".to_string(), Some("max".to_string()));
        }
        "claude-fable-5" => {
            model.thinking_level_map.insert("off".to_string(), None);
            model
                .thinking_level_map
                .insert("xhigh".to_string(), Some("xhigh".to_string()));
            model
                .thinking_level_map
                .insert("max".to_string(), Some("max".to_string()));
            compat["allowedFallbackModels"] = json!(["claude-opus-4-8", "claude-opus-5"]);
        }
        _ => {}
    }
    if id == "claude-opus-5" {
        compat["allowedFallbackModels"] = json!(["claude-opus-4-8"]);
    }
    model.compat = Some(compat);
    model
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[allow(clippy::too_many_arguments)]
async fn stream_messages(
    transport: &Arc<dyn HttpTransport>,
    provider_id: ProviderId,
    endpoint: &str,
    headers: BTreeMap<String, String>,
    model: pi_core::ModelId,
    request: ProviderRequest,
    context: ProviderCallContext,
    signal: AbortSignal,
    mode: AnthropicMode,
) -> Result<ProviderStream, ProviderError> {
    let payload = context
        .before_provider_request(&signal, request_body(&request, mode))
        .await?;
    let response = transport
        .post_json(endpoint, &headers, &payload, signal.clone())
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
    Ok(messages_stream(
        provider_id,
        model,
        ANTHROPIC_MESSAGES_API,
        response.body,
        signal,
        mode,
    ))
}

fn validate_compatible_config(config: &AnthropicCompatibleConfig) -> Result<(), ProviderError> {
    if config.base_url.trim().is_empty() {
        return Err(ProviderError::Failure(
            "base URL cannot be empty".to_string(),
        ));
    }
    if config
        .api_key
        .as_ref()
        .is_some_and(|key| key.contains(['\r', '\n']))
    {
        return Err(ProviderError::Failure("invalid API key".to_string()));
    }
    Ok(())
}

fn insert_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if let Some(existing) = headers
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.to_string());
}

fn apply_compat_headers(headers: &mut BTreeMap<String, String>, request: &ProviderRequest) {
    let compat = compatibility(request);
    if !request.tools.is_empty() && !compat.supports_eager_tool_input_streaming {
        let beta = "fine-grained-tool-streaming-2025-05-14";
        let current = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        if !current.split(',').any(|value| value.trim() == beta) {
            let value = if current.is_empty() {
                beta.to_string()
            } else {
                format!("{current},{beta}")
            };
            insert_header(headers, "anthropic-beta", &value);
        }
    }
    if compat.send_session_affinity_headers
        && let Some(session_id) = &request.session_id
    {
        insert_header(headers, "x-session-affinity", session_id);
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
    use std::sync::Mutex;

    use super::*;
    use pi_core::{AbortHandle, ModelId, ThinkingLevel};
    use pi_provider::{HttpResponse, TransportError};
    use serde_json::Value;

    #[derive(Default)]
    struct CapturedRequest {
        url: String,
        headers: BTreeMap<String, String>,
        body: Option<Value>,
    }

    #[derive(Default)]
    struct CapturingTransport {
        request: Mutex<CapturedRequest>,
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = CapturedRequest {
                url: url.to_string(),
                headers: headers.clone(),
                body: Some(body.clone()),
            };
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/event-stream".to_string()),
                headers: Vec::new(),
                body: Box::pin(futures::stream::empty()),
            })
        }
    }

    #[test]
    fn catalog_contains_current_claude_models() {
        let models = anthropic_models();
        let expected = [
            ("claude-haiku-4-5", 200_000, 64_000, 1.0, 5.0, 0.1, 1.25),
            (
                "claude-sonnet-4-6",
                1_000_000,
                128_000,
                3.0,
                15.0,
                0.3,
                3.75,
            ),
            ("claude-opus-4-6", 1_000_000, 128_000, 5.0, 25.0, 0.5, 6.25),
            ("claude-opus-4-7", 1_000_000, 128_000, 5.0, 25.0, 0.5, 6.25),
            ("claude-opus-4-8", 1_000_000, 128_000, 5.0, 25.0, 0.5, 6.25),
            ("claude-sonnet-5", 1_000_000, 128_000, 2.0, 10.0, 0.2, 2.5),
            ("claude-opus-5", 1_000_000, 128_000, 5.0, 25.0, 0.5, 6.25),
            ("claude-fable-5", 1_000_000, 128_000, 10.0, 50.0, 1.0, 12.5),
        ];

        assert_eq!(models.len(), expected.len());
        for (id, context_window, max_tokens, input_cost, output_cost, cache_read, cache_write) in
            expected
        {
            let spec = models
                .iter()
                .find(|model| model.id == ModelId::new(id))
                .unwrap_or_else(|| panic!("missing Anthropic model {id}"));
            assert_eq!(spec.context_window, context_window, "{id}");
            assert_eq!(spec.max_tokens, max_tokens, "{id}");
            assert_eq!(spec.cost.input, input_cost, "{id}");
            assert_eq!(spec.cost.output, output_cost, "{id}");
            assert_eq!(spec.cost.cache_read, cache_read, "{id}");
            assert_eq!(spec.cost.cache_write, cache_write, "{id}");
            assert_eq!(spec.input, vec![ModelInput::Text, ModelInput::Image]);
            assert!(spec.reasoning, "{id}");
        }
    }

    #[test]
    fn catalog_matches_pi_anthropic_thinking_compatibility() {
        let models = anthropic_models();
        let find = |id: &str| {
            models
                .iter()
                .find(|model| model.id == ModelId::new(id))
                .unwrap_or_else(|| panic!("missing Anthropic model {id}"))
        };

        let haiku = find("claude-haiku-4-5");
        assert_eq!(
            haiku.compat.as_ref().unwrap()["forceAdaptiveThinking"],
            Value::Null
        );
        assert!(haiku.thinking_level_map.is_empty());

        for id in [
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-fable-5",
        ] {
            assert_eq!(
                find(id).compat.as_ref().unwrap()["forceAdaptiveThinking"],
                true,
                "{id}"
            );
        }

        assert_eq!(
            find("claude-opus-4-6").thinking_level_map["max"],
            Some("max".to_string())
        );
        assert!(
            !find("claude-opus-4-6")
                .thinking_level_map
                .contains_key("xhigh")
        );
        assert_eq!(
            find("claude-opus-4-8").thinking_level_map["xhigh"],
            Some("xhigh".to_string())
        );
        for id in ["claude-opus-4-7", "claude-opus-4-8", "claude-opus-5"] {
            assert_eq!(
                find(id).compat.as_ref().unwrap()["supportsTemperature"],
                false,
                "{id}"
            );
        }
        assert_eq!(
            find("claude-opus-5").compat.as_ref().unwrap()["allowedFallbackModels"],
            json!(["claude-opus-4-8"])
        );
        assert_eq!(find("claude-fable-5").thinking_level_map["off"], None);
        assert_eq!(
            find("claude-fable-5").compat.as_ref().unwrap()["allowedFallbackModels"],
            json!(["claude-opus-4-8", "claude-opus-5"])
        );
    }

    #[test]
    fn compatible_endpoint_accepts_root_v1_and_full_urls() {
        let root = AnthropicCompatibleProvider::new(AnthropicCompatibleConfig::without_api_key(
            "https://example.test",
        ))
        .unwrap();
        assert_eq!(root.endpoint(), "https://example.test/v1/messages");

        let versioned = AnthropicCompatibleProvider::new(
            AnthropicCompatibleConfig::without_api_key("https://example.test/v1"),
        )
        .unwrap();
        assert_eq!(versioned.endpoint(), "https://example.test/v1/messages");

        let full = AnthropicCompatibleProvider::new(AnthropicCompatibleConfig::without_api_key(
            "https://example.test/v1/messages?region=cn",
        ))
        .unwrap();
        assert_eq!(
            full.endpoint(),
            "https://example.test/v1/messages?region=cn"
        );
    }

    #[tokio::test]
    async fn compatible_provider_sends_anthropic_headers_and_payload() {
        let transport = Arc::new(CapturingTransport::default());
        let provider = AnthropicCompatibleProvider::with_transport(
            AnthropicCompatibleConfig::new("https://example.test/v1", "configured-key")
                .provider_id("byteintl")
                .header("X-Provider", "provider"),
            transport.clone(),
        )
        .unwrap();
        let request = ProviderRequest {
            model: ModelId::new("custom-claude"),
            model_spec: None,
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Off,
            max_output_tokens: Some(4_096),
            headers: BTreeMap::from([
                ("x-api-key".to_string(), "request-key".to_string()),
                ("X-Request".to_string(), "request".to_string()),
            ]),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let context = ProviderCallContext::without_plugins(
            "/project",
            ProviderId::new("byteintl"),
            ModelId::new("custom-claude"),
        );
        let (_, signal) = AbortHandle::new();

        let _stream = provider.stream(request, context, signal).await.unwrap();

        let captured = transport
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(captured.url, "https://example.test/v1/messages");
        assert_eq!(captured.headers["x-api-key"], "request-key");
        assert_eq!(captured.headers["anthropic-version"], "2023-06-01");
        assert_eq!(captured.headers["X-Provider"], "provider");
        assert_eq!(captured.headers["X-Request"], "request");
        assert_eq!(captured.body.as_ref().unwrap()["model"], "custom-claude");
        assert_eq!(captured.body.as_ref().unwrap()["max_tokens"], 4_096);
    }
}
