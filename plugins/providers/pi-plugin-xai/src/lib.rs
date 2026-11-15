mod oauth;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{AbortSignal, ModelSpec, PluginId, ProviderId};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderPlugin,
    ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_plugin_openai::responses::{
    request_body as responses_request_body, stream as responses_stream,
};
use pi_provider::{
    HttpTransport, ReqwestTransport, TransportError, insert_header, post_json_with_provider_hooks,
    read_error_response,
};
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
    provider: Arc<dyn Provider>,
}

impl XAiPlugin {
    pub fn with_credential_source(
        mut self,
        source: Arc<dyn pi_provider::CredentialSource>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        self.provider = Arc::new(pi_provider::CredentialProvider::new(
            self.provider,
            source,
            move |value| {
                Ok(Arc::new(XAiProvider::with_transport(
                    value.map(|value| value.secret.clone()),
                    Arc::clone(&transport),
                )))
            },
        ));
        self
    }

    pub fn discover() -> Self {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Self {
        Self::from_stored_with_transport(api_key, Arc::new(ReqwestTransport::new()))
    }

    pub fn from_stored_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            provider: Arc::new(XAiProvider::with_transport(
                env("XAI_API_KEY").or(api_key),
                transport,
            )),
        }
    }

    pub fn new(api_key: Option<String>) -> Self {
        Self::new_with_transport(api_key, Arc::new(ReqwestTransport::new()))
    }

    pub fn new_with_transport(api_key: Option<String>, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            provider: Arc::new(XAiProvider::with_transport(api_key, transport)),
        }
    }
}

#[pi_plugin::provider_plugin]
impl ProviderPlugin for XAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("xai-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
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
            ("User-Agent".to_string(), "pi-rs".to_string()),
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
            let message = read_error_response(response)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(message));
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
    serde_json::from_str(include_str!("data/xai.json")).expect("checked-in xai catalog")
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn map_transport_error(error: TransportError) -> ProviderError {
    error.into_provider_error()
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
            thinking_budgets: None,
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
