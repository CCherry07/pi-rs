use std::sync::Arc;

use pi_core::{ModelSpec, PluginId};
use pi_plugin::{Provider, ProviderError, ProviderPlugin, ProviderRegisterContext};
use pi_provider::HttpTransport;

use crate::codex::{CodexTransportOptions, OpenAiCodexProvider};
use crate::config::{OpenAiCompatibleConfig, OpenAiConfig};
use crate::provider::{OpenAiCompatibleProvider, OpenAiProvider};

/// OpenAI Codex transport and model catalog loaded as one provider contribution.
pub struct OpenAiCodexPlugin {
    provider: Arc<dyn Provider>,
}

impl OpenAiCodexPlugin {
    pub fn with_credential_source(
        mut self,
        source: Arc<dyn pi_provider::CredentialSource>,
        transport: Arc<dyn HttpTransport>,
        options: CodexTransportOptions,
    ) -> Self {
        self.provider = Arc::new(pi_provider::CredentialProvider::new(
            self.provider,
            source,
            move |value| {
                let credentials = value
                    .map(|value| crate::CodexCredentials::from_access_token(&value.secret))
                    .unwrap_or_else(crate::CodexCredentials::discover);
                Ok(Arc::new(OpenAiCodexProvider::with_transport_options(
                    credentials,
                    Arc::clone(&transport),
                    options.clone(),
                )))
            },
        ));
        self
    }

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

#[pi_plugin::provider_plugin]
impl ProviderPlugin for OpenAiCodexPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-codex-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in openai_codex_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

/// Built-in metadata for the models exposed by OpenAI's Codex backend.
pub struct OpenAiCodexCatalogPlugin;

#[pi_plugin::provider_plugin]
impl ProviderPlugin for OpenAiCodexCatalogPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-codex-catalog")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        for model in openai_codex_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

/// Returns the current Pi-compatible OpenAI Codex model catalog.
pub fn openai_codex_models() -> Vec<ModelSpec> {
    serde_json::from_str(include_str!("data/openai-codex.json")).expect("checked-in Codex catalog")
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

#[pi_plugin::provider_plugin]
impl ProviderPlugin for OpenAiCompatiblePlugin {
    fn id(&self) -> PluginId {
        PluginId::new(format!("{}-provider", self.provider.id()))
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.provider.clone())
    }
}

pub struct OpenAiPlugin {
    provider: Arc<dyn Provider>,
}

impl OpenAiPlugin {
    pub fn from_stored_with_transport(
        key: Option<String>,
        source: Arc<dyn pi_provider::CredentialSource>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        let make = |key: Option<String>| {
            let config = key
                .map_or_else(
                    || OpenAiCompatibleConfig::without_api_key("https://api.openai.com/v1"),
                    |key| OpenAiCompatibleConfig::new("https://api.openai.com/v1", key),
                )
                .provider_id("openai");
            crate::OpenAiResponsesCompatibleProvider::with_transport(config, Arc::clone(&transport))
        };
        let initial = Arc::new(make(key)?);
        Ok(Self {
            provider: Arc::new(pi_provider::CredentialProvider::new(
                initial,
                source,
                move |value| {
                    let config = value
                        .map_or_else(
                            || OpenAiCompatibleConfig::without_api_key("https://api.openai.com/v1"),
                            |value| {
                                OpenAiCompatibleConfig::new(
                                    "https://api.openai.com/v1",
                                    &value.secret,
                                )
                            },
                        )
                        .provider_id("openai");
                    Ok(Arc::new(
                        crate::OpenAiResponsesCompatibleProvider::with_transport(
                            config,
                            Arc::clone(&transport),
                        )?,
                    ))
                },
            )),
        })
    }

    pub fn new(config: OpenAiConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(OpenAiProvider::new(config)?),
        })
    }

    pub fn provider(&self) -> Arc<dyn Provider> {
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

#[pi_plugin::provider_plugin]
impl ProviderPlugin for OpenAiPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("openai-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in openai_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

pub fn openai_models() -> Vec<ModelSpec> {
    serde_json::from_str(include_str!("data/openai.json")).expect("checked-in OpenAI catalog")
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
            pi_plugin::ProviderAvailability::MissingCredentials
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
            .model(
                &ProviderId::new("openai-codex"),
                &ModelId::new("gpt-5.6-sol"),
            )
            .unwrap();
        assert_eq!(model.context_window, 272_000);
        assert_eq!(model.max_tokens, 128_000);
        assert!(model.reasoning);
        assert!(model.input.contains(&pi_core::ModelInput::Image));
        assert_eq!(
            model.thinking_level_map.get("minimal"),
            Some(&Some("low".to_string()))
        );
        assert_eq!(
            model.thinking_level_map.get("xhigh"),
            Some(&Some("xhigh".to_string()))
        );
        assert_eq!(
            model.thinking_level_map.get("max"),
            Some(&Some("max".into()))
        );

        let latest = runtime
            .model(
                &ProviderId::new("openai-codex"),
                &ModelId::new("gpt-5.6-sol"),
            )
            .unwrap();
        assert_eq!(
            latest.thinking_level_map.get("max"),
            Some(&Some("max".to_string()))
        );

        let astra = runtime
            .model(
                &ProviderId::new("openai-codex"),
                &ModelId::new("gpt-6-astra"),
            )
            .unwrap();
        assert_eq!(astra.name, "GPT-6 Astra");
        assert_eq!(astra.context_window, 272_000);
        assert_eq!(astra.max_tokens, 128_000);
        assert_eq!(astra.cost.input, 10.0);
        assert_eq!(astra.cost.output, 50.0);
        assert_eq!(astra.cost.cache_read, 1.0);
        assert_eq!(astra.cost.cache_write, 12.5);
        assert_eq!(astra.cost.tiers.len(), 1);
        assert_eq!(astra.cost.tiers[0].input_tokens_above, 272_000);
        assert_eq!(astra.thinking_level_map.get("off"), Some(&None));
        assert_eq!(
            astra.thinking_level_map.get("minimal"),
            Some(&Some("low".into()))
        );
        assert_eq!(
            astra.thinking_level_map.get("xhigh"),
            Some(&Some("xhigh".to_string()))
        );
        assert_eq!(
            astra.thinking_level_map.get("max"),
            Some(&Some("max".to_string()))
        );
        assert_eq!(
            astra.supported_thinking_levels(),
            vec![
                pi_core::ThinkingLevel::Minimal,
                pi_core::ThinkingLevel::Low,
                pi_core::ThinkingLevel::Medium,
                pi_core::ThinkingLevel::High,
                pi_core::ThinkingLevel::XHigh,
                pi_core::ThinkingLevel::Max,
            ]
        );
        assert_eq!(
            astra.clamp_thinking_level(pi_core::ThinkingLevel::Off),
            pi_core::ThinkingLevel::Minimal
        );

        let spark = runtime
            .model(
                &ProviderId::new("openai-codex"),
                &ModelId::new("gpt-6.1-sol"),
            )
            .unwrap();
        assert_eq!(spark.context_window, 500_000);
    }
}
