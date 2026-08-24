mod oauth;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, ModelCost, ModelInput, ModelSpec, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream,
};
use pi_plugin_openai::responses::{
    request_body as responses_request_body, stream as responses_stream,
};
use pi_provider::{HttpTransport, ReqwestTransport, TransportError, collect_body_limited};
use serde_json::{Value, json};

pub use oauth::{
    DeviceAuthorization, OAuthCredential, poll_device_authorization, refresh,
    start_device_authorization,
};

const PROVIDER_ID: &str = "xai";
const API_NAME: &str = "openai-responses";
const BASE_URL: &str = "https://api.x.ai/v1";

/// Built-in xAI Responses provider and current Pi-compatible Grok catalog.
pub struct XAiPlugin {
    provider: Arc<XAiProvider>,
}

impl XAiPlugin {
    pub fn discover() -> Self {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Self {
        Self {
            provider: Arc::new(XAiProvider::new(env("XAI_API_KEY").or(api_key))),
        }
    }

    pub fn new(api_key: Option<String>) -> Self {
        Self {
            provider: Arc::new(XAiProvider::new(api_key)),
        }
    }
}

impl ProviderPlugin for XAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("xai-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in xai_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

pub struct XAiProvider {
    api_key: Option<String>,
    transport: Arc<dyn HttpTransport>,
}

impl XAiProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_transport(api_key, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(api_key: Option<String>, transport: Arc<dyn HttpTransport>) -> Self {
        Self { api_key, transport }
    }

    fn headers(
        &self,
        request: &ProviderRequest,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let key = self
            .api_key
            .as_deref()
            .ok_or_else(|| ProviderError::Failure("xAI requires XAI_API_KEY".to_string()))?;
        if key.contains(['\r', '\n']) {
            return Err(ProviderError::Failure("invalid xAI API key".to_string()));
        }
        let mut headers = BTreeMap::from([
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {key}")),
            ("User-Agent".to_string(), "pi_rs".to_string()),
        ]);
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        Ok(headers)
    }
}

#[async_trait]
impl Provider for XAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn availability(&self) -> ProviderAvailability {
        if self.api_key.is_some() {
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
        let payload = context
            .before_provider_request(&signal, xai_request_body(&request))
            .await?;
        let endpoint = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .map(responses_endpoint)
            .unwrap_or_else(|| format!("{BASE_URL}/responses"));
        let response = self
            .transport
            .post_json(&endpoint, &headers, &payload, signal.clone())
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

        Ok(responses_stream(
            ProviderId::new(PROVIDER_ID),
            request.model,
            API_NAME,
            response.body,
            signal,
        ))
    }
}

fn xai_request_body(request: &ProviderRequest) -> Value {
    let mut payload = responses_request_body(request);
    payload["include"] = json!(["reasoning.encrypted_content"]);
    if payload.get("tools").is_some() {
        payload["parallel_tool_calls"] = Value::Bool(true);
    }
    payload
}

fn responses_endpoint(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/responses") {
        base.to_string()
    } else {
        format!("{base}/responses")
    }
}

pub fn xai_models() -> Vec<ModelSpec> {
    vec![
        xai_model(
            "grok-4.5", "Grok 4.5", 500_000, 500_000, 2.0, 6.0, 0.3, false,
        ),
        xai_model(
            "grok-4.6", "Grok 4.6", 500_000, 500_000, 2.0, 6.0, 0.5, true,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn xai_model(
    id: &str,
    name: &str,
    context_window: u64,
    max_tokens: u64,
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    xhigh: bool,
) -> ModelSpec {
    let mut model = ModelSpec::new(PROVIDER_ID, id, name, API_NAME);
    model.base_url = Some(BASE_URL.to_string());
    model.reasoning = true;
    model.input = vec![ModelInput::Text, ModelInput::Image];
    model.cost = ModelCost {
        input: input_cost,
        output: output_cost,
        cache_read: cache_read_cost,
        cache_write: 0.0,
        tiers: Vec::new(),
    };
    model.context_window = context_window;
    model.max_tokens = max_tokens;
    model.thinking_level_map = BTreeMap::from([
        ("off".to_string(), None),
        ("minimal".to_string(), None),
        ("low".to_string(), Some("low".to_string())),
        ("medium".to_string(), Some("medium".to_string())),
        ("high".to_string(), Some("high".to_string())),
        ("xhigh".to_string(), xhigh.then(|| "xhigh".to_string())),
        ("max".to_string(), None),
    ]);
    model
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
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

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        other => ProviderError::Failure(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{Message, ModelId, ThinkingLevel, UserMessage};

    #[test]
    fn current_catalog_has_pi_reasoning_levels_and_limits() {
        let models = xai_models();
        let grok = models
            .iter()
            .find(|model| model.id == ModelId::new("grok-4.6"))
            .unwrap();
        assert_eq!(grok.context_window, 500_000);
        assert_eq!(grok.max_tokens, 500_000);
        assert_eq!(grok.thinking_level_map["xhigh"].as_deref(), Some("xhigh"));
        assert_eq!(grok.thinking_level_map["off"], None);
    }

    #[test]
    fn payload_uses_xai_responses_shape() {
        let request = ProviderRequest {
            model: ModelId::new("grok-4.6"),
            model_spec: xai_models()
                .into_iter()
                .find(|model| model.id == ModelId::new("grok-4.6")),
            system_prompt: "system".to_string(),
            messages: vec![Message::User(UserMessage::text("hello", 0))],
            tools: Vec::new(),
            thinking_level: ThinkingLevel::XHigh,
            max_output_tokens: Some(8),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let payload = xai_request_body(&request);
        assert_eq!(payload["input"][0]["role"], "developer");
        assert_eq!(payload["reasoning"]["effort"], "xhigh");
        assert_eq!(payload["include"][0], "reasoning.encrypted_content");
        assert_eq!(payload["max_output_tokens"], 16);
    }
}
