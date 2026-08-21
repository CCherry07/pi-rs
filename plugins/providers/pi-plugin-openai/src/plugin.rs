use std::sync::Arc;

use pi_core::{PluginId, Provider, ProviderError, ProviderPlugin, ProviderRegisterContext};

use crate::config::{OpenAiCompatibleConfig, OpenAiConfig};
use crate::provider::{OpenAiCompatibleProvider, OpenAiProvider};

pub struct OpenAiCompatiblePlugin {
    provider: Arc<OpenAiCompatibleProvider>,
}

impl OpenAiCompatiblePlugin {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(OpenAiCompatibleProvider::new(config)?),
        })
    }

    pub fn provider(&self) -> Arc<OpenAiCompatibleProvider> {
        Arc::clone(&self.provider)
    }
}

impl ProviderPlugin for OpenAiCompatiblePlugin {
    fn id(&self) -> PluginId {
        PluginId::new(format!("{}-provider", self.provider.id()))
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())
    }
}

pub struct OpenAiPlugin {
    provider: Arc<OpenAiProvider>,
}

impl OpenAiPlugin {
    pub fn new(config: OpenAiConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(OpenAiProvider::new(config)?),
        })
    }

    pub fn provider(&self) -> Arc<OpenAiProvider> {
        Arc::clone(&self.provider)
    }
}

impl ProviderPlugin for OpenAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())
    }
}
