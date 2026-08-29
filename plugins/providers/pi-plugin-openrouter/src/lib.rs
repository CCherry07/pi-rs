#![forbid(unsafe_code)]

//! OpenRouter provider and high-value built-in model catalog.

mod catalog;
mod oauth;

pub use catalog::openrouter_models;
pub use oauth::{
    OAuthCredential as OpenRouterOAuthCredential, OAuthLogin as OpenRouterOAuthLogin,
    start_oauth as start_openrouter_oauth,
};

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, PluginId, Provider, ProviderAvailability, ProviderCallContext, ProviderError,
    ProviderId, ProviderPlugin, ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use pi_provider::{HttpTransport, ReqwestTransport};

pub const OPENROUTER_PROVIDER_ID: &str = "openrouter";
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouterPlugin {
    provider: Arc<OpenRouterProvider>,
}

impl OpenRouterPlugin {
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
        })
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for OpenRouterPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openrouter-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in openrouter_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

pub struct OpenRouterProvider {
    inner: OpenAiCompatibleProvider,
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
        Ok(Self {
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
        self.inner.stream(request, context, signal).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_models_keep_openrouter_compat_and_aliases() {
        let models = openrouter_models();
        assert!(
            models
                .iter()
                .any(|model| model.id.as_str() == "~openai/gpt-latest")
        );
        assert!(models.iter().all(|model| {
            model.provider == ProviderId::new(OPENROUTER_PROVIDER_ID)
                && model.api == "openai-completions"
                && model.base_url.as_deref() == Some(OPENROUTER_BASE_URL)
                && model.compat.is_some()
        }));
    }
}
