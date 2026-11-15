//! Independently registered, catalog-backed gateway providers sharing standard wire protocols.
use async_trait::async_trait;
use pi_core::{AbortSignal, ModelSpec, PluginId, ProviderId};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderPlugin,
    ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_plugin_anthropic::{AnthropicCompatibleConfig, AnthropicCompatibleProvider};
use pi_plugin_openai::{
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, OpenAiResponsesCompatibleProvider,
};
use pi_provider::{CredentialSource, HttpTransport};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

pub mod kimi_oauth;

#[derive(Clone, Deserialize)]
pub struct GatewayDefinition {
    pub id: String,
    pub name: String,
    pub environment: Vec<String>,
    pub models: Vec<ModelSpec>,
    #[serde(default, rename = "modelHeaders")]
    pub model_headers: BTreeMap<String, BTreeMap<String, String>>,
}

pub fn gateway_definitions() -> &'static [GatewayDefinition] {
    static CATALOG: OnceLock<Vec<GatewayDefinition>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("data/catalog.json")).expect("checked-in gateway catalog")
    })
}

pub struct GatewayPlugin {
    provider: Arc<GatewayProvider>,
    models: Vec<ModelSpec>,
}

impl GatewayPlugin {
    pub fn new(
        definition: &GatewayDefinition,
        source: Arc<dyn CredentialSource>,
        available: bool,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            provider: Arc::new(GatewayProvider {
                id: definition.id.clone().into(),
                name: definition.name.clone(),
                source,
                available,
                transport,
                model_headers: definition.model_headers.clone(),
            }),
            models: definition.models.clone(),
        }
    }
}

#[pi_plugin::provider_plugin]
impl ProviderPlugin for GatewayPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(format!("{}-provider", self.provider.id))
    }
    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in &self.models {
            context.register_model(model.clone())?;
        }
        Ok(())
    }
}

struct GatewayProvider {
    id: ProviderId,
    name: String,
    source: Arc<dyn CredentialSource>,
    available: bool,
    transport: Arc<dyn HttpTransport>,
    model_headers: BTreeMap<String, BTreeMap<String, String>>,
}

#[async_trait]
impl Provider for GatewayProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn availability(&self) -> ProviderAvailability {
        if self.available {
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
        let credential = self.source.resolve(&signal).await?.ok_or_else(|| {
            ProviderError::Failure(format!("missing credentials for {}", self.id))
        })?;
        if let Some(headers) = self.model_headers.get(request.model.as_str()) {
            for (name, value) in headers {
                if !request
                    .headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case(name))
                {
                    request.headers.insert(name.clone(), value.clone());
                }
            }
        }
        let model = request
            .model_spec
            .as_mut()
            .ok_or_else(|| ProviderError::Failure("gateway requires model metadata".into()))?;
        let api = model.api.clone();
        let mut base = model
            .base_url
            .clone()
            .ok_or_else(|| ProviderError::Failure("gateway model has no endpoint".into()))?;
        let mut key = Some(credential.secret);
        if self.id.as_str().starts_with("cloudflare-") {
            for field in ["CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_GATEWAY_ID"] {
                let placeholder = format!("{{{field}}}");
                if !base.contains(&placeholder) {
                    continue;
                }
                let value = credential
                    .metadata
                    .get("env")
                    .and_then(|value| value.get(field))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| std::env::var(field).ok())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| ProviderError::Failure(format!("missing {field}")))?;
                // An account ID is a path segment, not an arbitrary URL fragment.
                let encoded: String = reqwest::Url::parse("https://gateway.invalid/")
                    .map(|mut url| {
                        url.path_segments_mut().expect("HTTP base").push(&value);
                        url.path().trim_start_matches('/').to_owned()
                    })
                    .map_err(|error| ProviderError::Failure(error.to_string()))?;
                base = base.replace(&placeholder, &encoded);
            }
            if self.id.as_str() == "cloudflare-ai-gateway" {
                pi_provider::insert_header(
                    &mut request.headers,
                    "cf-aig-authorization",
                    format!("Bearer {}", key.take().unwrap()),
                );
                pi_provider::remove_header(&mut request.headers, "Authorization");
                pi_provider::remove_header(&mut request.headers, "x-api-key");
            }
        }
        model.base_url = Some(base.clone());
        if matches!(self.id.as_str(), "opencode" | "opencode-go")
            && !request
                .headers
                .keys()
                .any(|key| key.eq_ignore_ascii_case("x-opencode-session"))
            && let Some(session) = &request.session_id
        {
            request
                .headers
                .insert("x-opencode-session".into(), session.clone());
        }
        let provider: Arc<dyn Provider> = match api.as_str() {
            "openai-completions" | "openai-responses" => {
                let mut config = key.map_or_else(
                    || OpenAiCompatibleConfig::without_api_key(&base),
                    |key| OpenAiCompatibleConfig::new(&base, key),
                );
                config.provider_id = self.id.clone();
                if api == "openai-responses" {
                    Arc::new(OpenAiResponsesCompatibleProvider::with_transport(
                        config,
                        self.transport.clone(),
                    )?)
                } else {
                    Arc::new(OpenAiCompatibleProvider::with_transport(
                        config,
                        self.transport.clone(),
                    )?)
                }
            }
            "anthropic-messages" => {
                let mut config = key.map_or_else(
                    || AnthropicCompatibleConfig::without_api_key(&base),
                    |key| AnthropicCompatibleConfig::new(&base, key),
                );
                config.provider_id = self.id.clone();
                Arc::new(AnthropicCompatibleProvider::with_transport(
                    config,
                    self.transport.clone(),
                )?)
            }
            "google-generative-ai" => {
                let mut config = pi_plugin_google::GoogleCompatibleConfig::without_api_key(&base);
                config.provider_id = self.id.clone();
                config.api_key = key;
                Arc::new(pi_plugin_google::GoogleCompatibleProvider::with_transport(
                    config,
                    self.transport.clone(),
                )?)
            }
            other => {
                return Err(ProviderError::Failure(format!(
                    "unsupported gateway API {other}"
                )));
            }
        };
        provider.stream(request, context, signal).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{AbortHandle, ModelId, ThinkingLevel};
    use pi_provider::{CredentialKind, HttpResponse, ProviderCredential, TransportError};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    struct Key;
    #[async_trait]
    impl CredentialSource for Key {
        async fn resolve(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<ProviderCredential>, ProviderError> {
            Ok(Some(ProviderCredential {
                secret: "fixture".into(),
                kind: CredentialKind::ApiKey,
                metadata: BTreeMap::from([(
                    "env".into(),
                    serde_json::json!({
                        "CLOUDFLARE_ACCOUNT_ID":"account/segment", "CLOUDFLARE_GATEWAY_ID":"gateway"
                    }),
                )]),
            }))
        }
    }
    type CapturedRequest = (String, BTreeMap<String, String>, serde_json::Value);
    #[derive(Default)]
    struct Capture(Mutex<Option<CapturedRequest>>);
    #[async_trait]
    impl HttpTransport for Capture {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            body: &serde_json::Value,
            _: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self.0.lock().unwrap() = Some((url.into(), headers.clone(), body.clone()));
            Err(TransportError::Request("captured".into()))
        }
    }
    #[test]
    fn generated_catalog_has_unique_models_and_only_supported_chat_protocols() {
        let mut providers = BTreeSet::new();
        for definition in gateway_definitions() {
            assert!(providers.insert(&definition.id));
            let mut ids = BTreeSet::new();
            for model in &definition.models {
                assert!(ids.insert(&model.id));
                assert_eq!(model.provider.as_str(), definition.id);
                assert!(matches!(
                    model.api.as_str(),
                    "openai-completions"
                        | "openai-responses"
                        | "anthropic-messages"
                        | "google-generative-ai"
                ));
            }
        }
        assert!(!providers.contains(&"radius".to_owned()));
        for id in [
            "deepseek",
            "groq",
            "moonshotai",
            "minimax",
            "qwen-token-plan",
            "zai",
            "cloudflare-ai-gateway",
            "kimi-coding",
        ] {
            assert!(providers.contains(&id.to_owned()));
        }
    }
    #[tokio::test]
    async fn gateway_applies_vendor_headers_and_cloudflare_account_routing() {
        for (id, expected_header, expected_value) in [
            (
                "cloudflare-ai-gateway",
                "cf-aig-authorization",
                "Bearer fixture",
            ),
            ("cloudflare-workers-ai", "Authorization", "Bearer fixture"),
            ("nvidia", "NVCF-POLL-SECONDS", "3600"),
            ("kimi-coding", "Authorization", "Bearer fixture"),
        ] {
            let definition = gateway_definitions()
                .iter()
                .find(|value| value.id == id)
                .unwrap();
            let model = definition.models.first().unwrap();
            let capture = Arc::new(Capture::default());
            let plugin = GatewayPlugin::new(definition, Arc::new(Key), true, capture.clone());
            let request = ProviderRequest {
                model: model.id.clone(),
                model_spec: Some(model.clone()),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                thinking_level: ThinkingLevel::Off,
                thinking_budgets: None,
                max_output_tokens: None,
                headers: if id == "cloudflare-ai-gateway" {
                    BTreeMap::from([
                        ("Authorization".into(), "unwanted".into()),
                        ("x-api-key".into(), "unwanted".into()),
                    ])
                } else {
                    BTreeMap::new()
                },
                sampling_params: BTreeMap::new(),
                session_id: None,
            };
            let (_, signal) = AbortHandle::new();
            let context = ProviderCallContext::without_plugins("/", id.into(), model.id.clone());
            assert!(
                plugin
                    .provider
                    .stream(request, context, signal)
                    .await
                    .is_err()
            );
            let (url, headers, _) = capture
                .0
                .lock()
                .unwrap()
                .take()
                .expect("request reached transport");
            assert_eq!(
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(expected_header))
                    .unwrap()
                    .1,
                expected_value
            );
            if id.starts_with("cloudflare-") {
                assert!(url.contains("account%2Fsegment"), "{url}");
                assert!(!url.contains('{'), "{url}");
            }
            if id == "cloudflare-ai-gateway" {
                assert!(
                    !headers
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case("authorization")
                            || name.eq_ignore_ascii_case("x-api-key"))
                );
            }
        }
    }

    #[tokio::test]
    async fn gateway_routes_each_model_through_its_own_wire_protocol() {
        let definition = gateway_definitions()
            .iter()
            .find(|value| value.id == "opencode")
            .unwrap();
        for (api, suffix) in [
            ("openai-completions", "/chat/completions"),
            ("openai-responses", "/responses"),
            ("anthropic-messages", "/messages"),
            ("google-generative-ai", ":streamGenerateContent?alt=sse"),
        ] {
            let model = definition
                .models
                .iter()
                .find(|model| model.api == api)
                .unwrap();
            let capture = Arc::new(Capture::default());
            let plugin = GatewayPlugin::new(definition, Arc::new(Key), true, capture.clone());
            let request = ProviderRequest {
                model: model.id.clone(),
                model_spec: Some(model.clone()),
                system_prompt: "test".into(),
                messages: vec![],
                tools: vec![],
                thinking_level: ThinkingLevel::Off,
                thinking_budgets: None,
                max_output_tokens: None,
                headers: BTreeMap::new(),
                sampling_params: BTreeMap::new(),
                session_id: Some("session".into()),
            };
            let context = ProviderCallContext::without_plugins(
                "/",
                definition.id.clone().into(),
                ModelId::new(model.id.as_str()),
            );
            let (_, signal) = AbortHandle::new();
            assert!(
                plugin
                    .provider
                    .stream(request, context, signal)
                    .await
                    .is_err()
            );
            let (url, headers, _) = capture
                .0
                .lock()
                .unwrap()
                .take()
                .expect("request reached transport");
            assert!(url.ends_with(suffix), "{api}: {url}");
            assert_eq!(
                headers.get("x-opencode-session").map(String::as_str),
                Some("session")
            );
        }
    }
}
