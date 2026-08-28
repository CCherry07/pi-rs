use std::sync::Arc;

use pi_core::{
    ModelCost, ModelCostTier, ModelInput, ModelSpec, PluginId, Provider, ProviderError,
    ProviderPlugin, ProviderRegisterContext,
};
use pi_provider::HttpTransport;

use crate::codex::{CodexTransportOptions, OpenAiCodexProvider};
use crate::config::{OpenAiCompatibleConfig, OpenAiConfig};
use crate::provider::{OpenAiCompatibleProvider, OpenAiProvider};

/// OpenAI Codex transport and model catalog loaded as one provider contribution.
pub struct OpenAiCodexPlugin {
    provider: Arc<OpenAiCodexProvider>,
}

impl OpenAiCodexPlugin {
    pub fn discover() -> Self {
        Self::new(crate::CodexCredentials::discover())
    }

    pub fn new(credentials: crate::CodexCredentials) -> Self {
        Self {
            provider: Arc::new(OpenAiCodexProvider::new(credentials)),
        }
    }

    pub fn with_transport(
        credentials: crate::CodexCredentials,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            provider: Arc::new(OpenAiCodexProvider::with_transport(credentials, transport)),
        }
    }

    pub fn with_transport_options(
        credentials: crate::CodexCredentials,
        transport: Arc<dyn HttpTransport>,
        transport_options: CodexTransportOptions,
    ) -> Self {
        Self {
            provider: Arc::new(OpenAiCodexProvider::with_transport_options(
                credentials,
                transport,
                transport_options,
            )),
        }
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for OpenAiCodexPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-codex-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in openai_codex_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

/// Built-in metadata for the models exposed by OpenAI's Codex backend.
pub struct OpenAiCodexCatalogPlugin;

const CODEX_PROVIDER: &str = "openai-codex";
const CODEX_API: &str = "openai-codex-responses";
const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const CODEX_CONTEXT_WINDOW: u64 = 272_000;
const CODEX_SPARK_CONTEXT_WINDOW: u64 = 128_000;
const CODEX_MAX_TOKENS: u64 = 128_000;

#[pi_core::provider_plugin]
impl ProviderPlugin for OpenAiCodexCatalogPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-codex-catalog")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        for model in openai_codex_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

/// Returns the current Pi-compatible OpenAI Codex model catalog.
pub fn openai_codex_models() -> Vec<ModelSpec> {
    vec![
        codex_model(
            "gpt-5.3-codex-spark",
            "GPT-5.3 Codex Spark",
            CODEX_SPARK_CONTEXT_WINDOW,
            false,
            ModelCost {
                input: 1.75,
                output: 14.0,
                cache_read: 0.175,
                cache_write: 0.0,
                tiers: Vec::new(),
            },
        ),
        codex_model(
            "gpt-5.4",
            "GPT-5.4",
            CODEX_CONTEXT_WINDOW,
            true,
            long_context_cost(2.5, 15.0, 0.25, 0.0),
        ),
        codex_model(
            "gpt-5.4-mini",
            "GPT-5.4 mini",
            CODEX_CONTEXT_WINDOW,
            true,
            ModelCost {
                input: 0.75,
                output: 4.5,
                cache_read: 0.075,
                cache_write: 0.0,
                tiers: Vec::new(),
            },
        ),
        codex_model(
            "gpt-5.5",
            "GPT-5.5",
            CODEX_CONTEXT_WINDOW,
            true,
            long_context_cost(5.0, 30.0, 0.5, 0.0),
        ),
        codex_model(
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            CODEX_CONTEXT_WINDOW,
            true,
            long_context_cost(0.2, 1.2, 0.02, 0.25),
        ),
        codex_model(
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            CODEX_CONTEXT_WINDOW,
            true,
            long_context_cost(5.0, 30.0, 0.5, 6.25),
        ),
        codex_model(
            "gpt-5.6-terra",
            "GPT-5.6 Terra",
            CODEX_CONTEXT_WINDOW,
            true,
            long_context_cost(2.0, 12.0, 0.2, 2.5),
        ),
    ]
}

fn codex_model(
    id: &str,
    name: &str,
    context_window: u64,
    image_input: bool,
    cost: ModelCost,
) -> ModelSpec {
    let mut model = ModelSpec::new(CODEX_PROVIDER, id, name, CODEX_API);
    model.base_url = Some(CODEX_BASE_URL.to_string());
    model.reasoning = true;
    model.input = if image_input {
        vec![ModelInput::Text, ModelInput::Image]
    } else {
        vec![ModelInput::Text]
    };
    model.cost = cost;
    model.context_window = context_window;
    model.max_tokens = CODEX_MAX_TOKENS;
    model
}

fn long_context_cost(input: f64, output: f64, cache_read: f64, cache_write: f64) -> ModelCost {
    ModelCost {
        input,
        output,
        cache_read,
        cache_write,
        tiers: vec![ModelCostTier {
            input_tokens_above: CODEX_CONTEXT_WINDOW,
            input: input * 2.0,
            output: output * 1.5,
            cache_read: cache_read * 2.0,
            cache_write: cache_write * 2.0,
        }],
    }
}

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

    pub fn with_transport(
        config: OpenAiCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(OpenAiCompatibleProvider::with_transport(config, transport)?),
        })
    }
}

#[pi_core::provider_plugin]
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

    pub fn with_transport(
        config: OpenAiConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(OpenAiProvider::with_transport(config, transport)?),
        })
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for OpenAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())
    }
}

#[cfg(test)]
mod tests {
    use pi_core::{ModelId, ProviderId};
    use pi_runtime::PiRuntime;

    use super::*;

    #[test]
    fn codex_catalog_distinguishes_registered_from_available_models() {
        let runtime = PiRuntime::builder()
            .provider_plugin(
                OpenAiCompatiblePlugin::new(
                    OpenAiCompatibleConfig::without_api_key("https://chatgpt.com/backend-api")
                        .provider_id("openai-codex"),
                )
                .unwrap(),
            )
            .provider_plugin(OpenAiCodexCatalogPlugin)
            .build()
            .unwrap();

        assert_eq!(runtime.models().len(), 7);
        assert!(runtime.available_models().is_empty());
        assert_eq!(
            runtime.provider_statuses()[0].availability,
            pi_core::ProviderAvailability::MissingCredentials
        );
    }

    #[test]
    fn codex_catalog_registers_current_models_and_context_windows() {
        let runtime = PiRuntime::builder()
            .provider_plugin(
                OpenAiCompatiblePlugin::new(
                    OpenAiCompatibleConfig::without_api_key("https://chatgpt.com/backend-api")
                        .provider_id("openai-codex"),
                )
                .unwrap(),
            )
            .provider_plugin(OpenAiCodexCatalogPlugin)
            .build()
            .unwrap();

        assert_eq!(runtime.models().len(), 7);
        let model = runtime
            .model(&ProviderId::new("openai-codex"), &ModelId::new("gpt-5.5"))
            .unwrap();
        assert_eq!(model.context_window, 272_000);
        assert_eq!(model.max_tokens, 128_000);
        assert!(model.reasoning);
        assert!(model.input.contains(&pi_core::ModelInput::Image));

        let spark = runtime
            .model(
                &ProviderId::new("openai-codex"),
                &ModelId::new("gpt-5.3-codex-spark"),
            )
            .unwrap();
        assert_eq!(spark.context_window, 128_000);
    }
}
