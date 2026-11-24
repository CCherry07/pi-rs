#![forbid(unsafe_code)]

//! OpenRouter provider and high-value built-in model catalog.

mod account_catalog;
mod catalog;
mod oauth;
pub use account_catalog::{personalize_catalog, refresh_account_catalog};

pub use catalog::openrouter_models;
pub use oauth::{
    OAuthCredential as OpenRouterOAuthCredential, OAuthLogin as OpenRouterOAuthLogin,
    start_oauth as start_openrouter_oauth,
};

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{AbortSignal, PluginId, ProviderId};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderPlugin,
    ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use pi_provider::{HttpTransport, ReqwestTransport};

pub const OPENROUTER_PROVIDER_ID: &str = "openrouter";
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouterPlugin {
    provider: Arc<dyn Provider>,
    models: Vec<pi_core::ModelSpec>,
}

impl OpenRouterPlugin {
    pub fn with_credential_source(
        mut self,
        source: Arc<dyn pi_provider::CredentialSource>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        self.provider = Arc::new(pi_provider::CredentialProvider::new(
            self.provider,
            source,
            move |credential| {
                let key = credential
                    .map(|credential| credential.secret.trim())
                    .filter(|secret| !secret.is_empty())
                    .map(str::to_owned);
                Ok(Arc::new(OpenRouterProvider::with_transport(
                    key,
                    Arc::clone(&transport),
                )?))
            },
        ));
        self
    }

    pub fn with_catalog(mut self, models: Vec<pi_core::ModelSpec>) -> Self {
        self.models = models;
        self
    }

    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::new_with_transport(
            std::env::var("OPENROUTER_API_KEY").ok().or(api_key),
            Arc::new(ReqwestTransport::new()),
        )
    }

    pub fn from_stored_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_transport(
            std::env::var("OPENROUTER_API_KEY").ok().or(api_key),
            transport,
        )
    }

    pub fn new_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(OpenRouterProvider::with_transport(api_key, transport)?),
            models: openrouter_models(),
        })
    }
}

#[pi_plugin::provider_plugin]
impl ProviderPlugin for OpenRouterPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openrouter-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in &self.models {
            context.register_model(model.clone())?;
        }
        Ok(())
    }
}

pub struct OpenRouterProvider {
    inner: OpenAiCompatibleProvider,
    anthropic: pi_plugin_anthropic::AnthropicCompatibleProvider,
}

impl OpenRouterProvider {
    pub fn with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        let mut config = api_key.map_or_else(
            || OpenAiCompatibleConfig::without_api_key(OPENROUTER_BASE_URL),
            |api_key| OpenAiCompatibleConfig::new(OPENROUTER_BASE_URL, api_key),
        );
        config.provider_id = ProviderId::new(OPENROUTER_PROVIDER_ID);
        config
            .headers
            .insert("User-Agent".to_string(), "pi-rs".to_string());
        let mut anthropic_config =
            pi_plugin_anthropic::AnthropicCompatibleConfig::without_api_key(OPENROUTER_BASE_URL);
        anthropic_config.provider_id = OPENROUTER_PROVIDER_ID.into();
        if let Some(key) = &config.api_key {
            anthropic_config
                .headers
                .insert("Authorization".into(), format!("Bearer {key}"));
        }
        Ok(Self {
            anthropic: pi_plugin_anthropic::AnthropicCompatibleProvider::with_transport(
                anthropic_config,
                transport.clone(),
            )?,
            inner: OpenAiCompatibleProvider::with_transport(config, transport)?,
        })
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(OPENROUTER_PROVIDER_ID)
    }

    fn name(&self) -> String {
        "OpenRouter".to_string()
    }

    fn availability(&self) -> ProviderAvailability {
        self.inner.availability()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        if request
            .model_spec
            .as_ref()
            .is_some_and(|model| model.api == "anthropic-messages")
        {
            self.anthropic.stream(request, context, signal).await
        } else {
            self.inner.stream(request, context, signal).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AnyModelSpec;
    use pi_provider::{CredentialKind, CredentialSource, ProviderCredential};
    use std::collections::BTreeMap;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct RotatingSource {
        value: Mutex<Option<ProviderCredential>>,
        checks: AtomicUsize,
        resolves: AtomicUsize,
    }
    #[async_trait]
    impl CredentialSource for RotatingSource {
        async fn check(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<ProviderCredential>, ProviderError> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            Ok(self.value.lock().unwrap().clone())
        }
        async fn resolve(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<ProviderCredential>, ProviderError> {
            self.resolves.fetch_add(1, Ordering::SeqCst);
            Ok(self.value.lock().unwrap().clone())
        }
    }

    #[derive(Default)]
    struct NoRequestTransport(AtomicUsize);
    #[async_trait]
    impl HttpTransport for NoRequestTransport {
        async fn post_json(
            &self,
            _: &str,
            _: &BTreeMap<String, String>,
            _: &serde_json::Value,
            _: AbortSignal,
        ) -> Result<pi_provider::HttpResponse, pi_provider::TransportError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(pi_provider::TransportError::Request(
                "unexpected request".into(),
            ))
        }
    }

    type CapturedRequest = (String, BTreeMap<String, String>);
    #[derive(Default)]
    struct Capture(Mutex<Option<CapturedRequest>>);
    #[async_trait]
    impl HttpTransport for Capture {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            _: &serde_json::Value,
            _: AbortSignal,
        ) -> Result<pi_provider::HttpResponse, pi_provider::TransportError> {
            *self.0.lock().unwrap() = Some((url.into(), headers.clone()));
            Err(pi_provider::TransportError::Request("captured".into()))
        }
    }

    #[tokio::test]
    async fn availability_rechecks_openrouter_credentials_without_requests() {
        let source = Arc::new(RotatingSource::default());
        let transport = Arc::new(NoRequestTransport::default());
        let plugin = OpenRouterPlugin::new_with_transport(None, transport.clone())
            .unwrap()
            .with_credential_source(source.clone(), transport.clone());
        let models = vec![AnyModelSpec::Chat(openrouter_models().remove(0))];

        for (credential, expected) in [
            (None, 0),
            (
                Some(ProviderCredential {
                    secret: "present".into(),
                    kind: CredentialKind::ApiKey,
                    metadata: BTreeMap::new(),
                }),
                1,
            ),
            (None, 0),
        ] {
            *source.value.lock().unwrap() = credential;
            let (_, signal) = pi_core::AbortHandle::new();
            assert_eq!(
                plugin
                    .provider
                    .filter_available_models(models.clone(), signal)
                    .await
                    .unwrap()
                    .len(),
                expected
            );
        }

        assert_eq!(source.checks.load(Ordering::SeqCst), 3);
        assert_eq!(source.resolves.load(Ordering::SeqCst), 0);
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn routes_messages_and_completions_with_openrouter_bearer_auth() {
        for (api, base, endpoint) in [
            (
                "anthropic-messages",
                None,
                "https://openrouter.ai/api/v1/messages",
            ),
            (
                "openai-completions",
                None,
                "https://openrouter.ai/api/v1/chat/completions",
            ),
            (
                "anthropic-messages",
                Some("https://us.openrouter.ai/api/v1"),
                "https://us.openrouter.ai/api/v1/messages",
            ),
            (
                "openai-completions",
                Some("https://us.openrouter.ai/api/v1"),
                "https://us.openrouter.ai/api/v1/chat/completions",
            ),
        ] {
            let mut model = openrouter_models()
                .into_iter()
                .find(|model| model.api == api)
                .unwrap();
            if let Some(base) = base {
                model.base_url = Some(base.into());
            }
            let capture = Arc::new(Capture::default());
            let provider =
                OpenRouterProvider::with_transport(Some("fixture".into()), capture.clone())
                    .unwrap();
            let context =
                ProviderCallContext::without_plugins("/", model.provider.clone(), model.id.clone());
            let request = ProviderRequest {
                model: model.id.clone(),
                model_spec: Some(model),
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                thinking_level: pi_core::ThinkingLevel::Off,
                thinking_budgets: None,
                max_output_tokens: None,
                cache_retention: None,
                headers: BTreeMap::new(),
                sampling_params: BTreeMap::new(),
                session_id: None,
            };
            let (_, signal) = pi_core::AbortHandle::new();
            assert!(provider.stream(request, context, signal).await.is_err());
            let (url, headers) = capture
                .0
                .lock()
                .unwrap()
                .take()
                .expect("request reached transport");
            assert_eq!(url, endpoint);
            assert_eq!(
                headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                    .unwrap()
                    .1,
                "Bearer fixture"
            );
            assert!(
                !headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case("x-api-key"))
            );
        }
    }

    #[test]
    fn catalog_models_keep_openrouter_compat_and_aliases() {
        let models = openrouter_models();
        assert!(
            models
                .iter()
                .any(|model| model.id.as_str() == "~openai/gpt-sol-latest")
        );
        assert!(models.iter().all(|model| {
            model.provider == ProviderId::new(OPENROUTER_PROVIDER_ID)
                && matches!(
                    model.api.as_str(),
                    "openai-completions" | "anthropic-messages"
                )
                && model
                    .base_url
                    .as_deref()
                    .is_some_and(|url| url.starts_with("https://openrouter.ai/api"))
                && (model.api == "anthropic-messages" || model.compat.is_some())
        }));
    }
}
