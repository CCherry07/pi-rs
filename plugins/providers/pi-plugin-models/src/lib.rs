#![forbid(unsafe_code)]

mod config;
mod provider;
mod resolver;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_core::{
    PluginId, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext, Result,
};

use config::{PreparedProvider, load_models_file};
use provider::ModelsJsonProvider;
use resolver::ConfigValueResolver;

/// Returns the generated JSON Schema for the supported models.json format.
///
/// Editors and standalone validation tools can consume this without loading a
/// runtime generation.
pub fn models_json_schema() -> serde_json::Value {
    config::models_json_schema()
}

#[derive(Debug, Clone)]
pub struct ModelsPluginOptions {
    pub path: PathBuf,
    pub runtime_api_keys: BTreeMap<ProviderId, String>,
}

impl ModelsPluginOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            runtime_api_keys: BTreeMap::new(),
        }
    }

    pub fn for_agent_dir(agent_dir: impl AsRef<Path>) -> Self {
        Self::new(agent_dir.as_ref().join("models.json"))
    }

    /// Adds a runtime credential override. This value has precedence over
    /// models.json but is never copied into the public model catalog.
    pub fn runtime_api_key(
        mut self,
        provider: impl Into<ProviderId>,
        api_key: impl Into<String>,
    ) -> Self {
        let api_key = api_key.into();
        if !api_key.trim().is_empty() {
            self.runtime_api_keys.insert(provider.into(), api_key);
        }
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelsPluginError {
    #[error("failed to read models.json at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse models.json at {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid models.json at {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

pub struct ModelsPlugin {
    providers: Vec<PreparedProvider>,
    resolver: Arc<ConfigValueResolver>,
}

impl ModelsPlugin {
    pub fn load(options: ModelsPluginOptions) -> std::result::Result<Self, ModelsPluginError> {
        let providers = load_models_file(&options.path, &options.runtime_api_keys)?;
        Ok(Self {
            providers,
            resolver: Arc::new(ConfigValueResolver::default()),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Returns configured provider IDs from the validated models.json snapshot.
    pub fn provider_ids(&self) -> Vec<ProviderId> {
        self.providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect()
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for ModelsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("models")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
        for configured in &self.providers {
            let fallback = context.base_provider(&configured.id);
            let base_models = context.base_models(&configured.id);
            let base_ids = base_models
                .iter()
                .map(|model| model.id.clone())
                .collect::<std::collections::HashSet<_>>();
            let fallback_apis = base_models
                .iter()
                .map(|model| model.api.clone())
                .collect::<std::collections::HashSet<_>>();
            let configured = configured
                .compose_with_base(&base_models)
                .map_err(ProviderError::Failure)?;
            context.register_provider_override(Arc::new(ModelsJsonProvider::new(
                configured.clone(),
                fallback,
                fallback_apis,
                Arc::clone(&self.resolver),
            )))?;
            for model in &configured.models {
                if base_ids.contains(&model.id) {
                    context.register_model_override(model.spec.clone())?;
                } else {
                    context.register_model(model.spec.clone())?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent::AgentOptions;
    use pi_core::{ModelId, ModelInput, RegistriesBuilder};
    use pi_plugin_anthropic::AnthropicPlugin;
    use pi_runtime::{PiRuntime, SystemPrompt};

    fn config(name: &str) -> String {
        format!(
            r#"{{
              // models.json accepts comments
              "providers": {{
                "custom": {{
                  "name": "Custom Provider",
                  "baseUrl": "http://localhost:11434/v1",
                  "api": "openai-completions",
                  "models": [{{
                    "id": "test/model",
                    "name": "{name}",
                    "reasoning": true,
                    "contextWindow": 32000,
                    "maxTokens": 2048,
                    "samplingParams": {{ "temperature": 0.2 }}
                  }}],
                  "modelOverrides": {{
                    "test/model": {{
                      "name": "{name} overridden",
                      "maxTokens": 4096
                    }}
                  }}
                }}
              }}
            }}"#
        )
    }

    #[test]
    fn missing_models_file_is_an_empty_plugin() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = ModelsPlugin::load(ModelsPluginOptions::new(
            directory.path().join("missing.json"),
        ))
        .unwrap();
        assert!(plugin.is_empty());
    }

    #[test]
    fn jsonc_catalog_registers_provider_and_model_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(&path, config("Generation 1")).unwrap();
        let plugin = ModelsPlugin::load(ModelsPluginOptions::new(&path)).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(Vec::new(), vec![Arc::new(plugin)])
            .unwrap();
        let provider = ProviderId::new("custom");
        let model = ModelId::new("test/model");
        let spec = registries.model(&provider, &model).unwrap();

        assert!(registries.provider(&provider).is_some());
        assert_eq!(
            registries.provider_name(&provider).as_deref(),
            Some("Custom Provider")
        );
        assert_eq!(spec.name, "Generation 1 overridden");
        assert_eq!(spec.context_window, 32_000);
        assert_eq!(spec.max_tokens, 4_096);
        assert_eq!(spec.sampling_params["temperature"], 0.2);
    }

    #[test]
    fn anthropic_messages_api_registers_custom_models() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(
            &path,
            r#"{
              "providers": {
                "custom": {
                  "baseUrl": "https://example.test",
                  "api": "anthropic-messages",
                  "models": [{ "id": "model" }]
                }
              }
            }"#,
        )
        .unwrap();
        let plugin = ModelsPlugin::load(ModelsPluginOptions::new(&path)).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(Vec::new(), vec![Arc::new(plugin)])
            .unwrap();
        let spec = registries
            .model(&ProviderId::new("custom"), &ModelId::new("model"))
            .unwrap();

        assert_eq!(spec.api, "anthropic-messages");
        assert_eq!(spec.base_url.as_deref(), Some("https://example.test"));
    }

    #[test]
    fn openai_responses_api_registers_custom_models() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(
            &path,
            r#"{
              "providers": {
                "custom-xai": {
                  "baseUrl": "https://api.x.ai/v1",
                  "api": "openai-responses",
                  "models": [{ "id": "grok-custom" }]
                }
              }
            }"#,
        )
        .unwrap();
        let plugin = ModelsPlugin::load(ModelsPluginOptions::new(&path)).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(Vec::new(), vec![Arc::new(plugin)])
            .unwrap();
        let spec = registries
            .model(&ProviderId::new("custom-xai"), &ModelId::new("grok-custom"))
            .unwrap();

        assert_eq!(spec.api, "openai-responses");
        assert_eq!(spec.base_url.as_deref(), Some("https://api.x.ai/v1"));
    }

    #[test]
    fn google_generative_ai_api_registers_custom_models() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(
            &path,
            r#"{
              "providers": {
                "my-google": {
                  "baseUrl": "https://generativelanguage.googleapis.com/v1beta",
                  "api": "google-generative-ai",
                  "models": [{ "id": "gemma-4-31b-it", "reasoning": true }]
                }
              }
            }"#,
        )
        .unwrap();
        let plugin = ModelsPlugin::load(ModelsPluginOptions::new(&path)).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(Vec::new(), vec![Arc::new(plugin)])
            .unwrap();
        let spec = registries
            .model(
                &ProviderId::new("my-google"),
                &ModelId::new("gemma-4-31b-it"),
            )
            .unwrap();

        assert_eq!(spec.api, "google-generative-ai");
        assert!(spec.reasoning);
        assert_eq!(
            spec.base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta")
        );
    }

    #[test]
    fn built_in_models_receive_provider_overlays_full_overrides_and_custom_upserts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(
            &path,
            r#"{
              "providers": {
                "anthropic": {
                  "baseUrl": "https://proxy.example/v1",
                  "compat": {
                    "forceAdaptiveThinking": true,
                    "supportsLongCacheRetention": true
                  },
                  "models": [{ "id": "custom-claude", "reasoning": true }],
                  "modelOverrides": {
                    "claude-sonnet-4-6": {
                      "name": "Proxy Sonnet",
                      "reasoning": false,
                      "input": ["text"],
                      "cost": { "input": 9.0 },
                      "contextWindow": 123456,
                      "maxTokens": 6543,
                      "samplingParams": { "top_p": 0.8 },
                      "compat": { "supportsLongCacheRetention": false }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();
        let plugin = ModelsPlugin::load(ModelsPluginOptions::new(&path)).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(
                Vec::new(),
                vec![
                    Arc::new(AnthropicPlugin::with_api_key("test")),
                    Arc::new(plugin),
                ],
            )
            .unwrap();
        let provider = ProviderId::new("anthropic");
        let sonnet = registries
            .model(&provider, &ModelId::new("claude-sonnet-4-6"))
            .unwrap();
        let custom = registries
            .model(&provider, &ModelId::new("custom-claude"))
            .unwrap();

        assert_eq!(registries.model_specs().len(), 9);
        assert_eq!(sonnet.name, "Proxy Sonnet");
        assert!(!sonnet.reasoning);
        assert_eq!(sonnet.input, vec![ModelInput::Text]);
        assert_eq!(sonnet.cost.input, 9.0);
        assert_eq!(sonnet.cost.output, 15.0);
        assert_eq!(sonnet.context_window, 123_456);
        assert_eq!(sonnet.max_tokens, 6_543);
        assert_eq!(sonnet.sampling_params["top_p"], 0.8);
        assert_eq!(
            sonnet.compat.as_ref().unwrap()["forceAdaptiveThinking"],
            true
        );
        assert_eq!(
            sonnet.compat.as_ref().unwrap()["supportsLongCacheRetention"],
            false
        );
        assert_eq!(sonnet.base_url.as_deref(), Some("https://proxy.example/v1"));
        assert_eq!(custom.api, "anthropic-messages");
        assert_eq!(custom.base_url.as_deref(), Some("https://proxy.example/v1"));
    }

    #[tokio::test]
    async fn failed_reload_keeps_the_previous_models_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(&path, config("Generation 1")).unwrap();
        let options = ModelsPluginOptions::new(&path);
        let runtime = PiRuntime::builder()
            .try_provider_plugin_factory({
                let options = options.clone();
                move || ModelsPlugin::load(options.clone())
            })
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("custom"),
                model_id: ModelId::new("test/model"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Final("test".to_string()))
            .build()
            .unwrap();
        let generation = runtime.generation();
        assert_eq!(
            runtime
                .model(&ProviderId::new("custom"), &ModelId::new("test/model"))
                .unwrap()
                .name,
            "Generation 1 overridden"
        );

        std::fs::write(&path, "{ invalid json").unwrap();
        assert!(runtime.reload().await.is_err());
        assert_eq!(runtime.generation(), generation);
        assert_eq!(
            runtime
                .model(&ProviderId::new("custom"), &ModelId::new("test/model"))
                .unwrap()
                .name,
            "Generation 1 overridden"
        );

        std::fs::write(&path, r#"{"providers": {}}"#).unwrap();
        assert!(runtime.reload().await.is_err());
        assert_eq!(runtime.generation(), generation);
        assert!(runtime.has_provider(&ProviderId::new("custom")));
    }
}
