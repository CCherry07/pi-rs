#![forbid(unsafe_code)]

mod config;
mod provider;
mod resolver;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_core::{PluginId, ProviderId, ProviderPlugin, ProviderRegisterContext, Result};

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

impl ProviderPlugin for ModelsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("models")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
        for configured in &self.providers {
            let fallback = context.base_provider(&configured.id);
            context.register_provider_override(Arc::new(ModelsJsonProvider::new(
                configured.clone(),
                fallback,
                Arc::clone(&self.resolver),
            )))?;
            for model in &configured.models {
                context.register_model(model.spec.clone())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent::AgentOptions;
    use pi_core::{ModelId, RegistriesBuilder};
    use pi_runtime::{PiRuntime, SystemPrompt};

    fn config(name: &str) -> String {
        format!(
            r#"{{
              // models.json accepts comments
              "providers": {{
                "custom": {{
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
        assert_eq!(spec.name, "Generation 1 overridden");
        assert_eq!(spec.context_window, 32_000);
        assert_eq!(spec.max_tokens, 4_096);
        assert_eq!(spec.sampling_params["temperature"], 0.2);
    }

    #[test]
    fn unsupported_api_fails_during_generation_build() {
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
        let error = ModelsPlugin::load(ModelsPluginOptions::new(&path))
            .err()
            .expect("unsupported API must fail");
        assert!(error.to_string().contains("unsupported api"));
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
