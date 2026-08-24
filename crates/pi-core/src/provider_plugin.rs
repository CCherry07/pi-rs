use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    AbortSignal, CoreError, ModelId, ModelSpec, PluginError, PluginId, Provider, ProviderId,
    RegistriesBuilder, Result,
};

#[derive(Clone)]
pub struct ProviderPluginContext {
    pub plugin_id: PluginId,
    pub generation: u64,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BeforeProviderRequestEvent {
    pub payload: Value,
}

/// Registration surface reserved for provider plugins.
///
/// Provider plugins deliberately do not participate in agent input or
/// lifecycle hooks. A single plugin may register multiple provider variants.
pub struct ProviderRegisterContext<'a> {
    owner: PluginId,
    registries: &'a mut RegistriesBuilder,
}

impl<'a> ProviderRegisterContext<'a> {
    fn new(owner: PluginId, registries: &'a mut RegistriesBuilder) -> Self {
        Self { owner, registries }
    }

    pub fn register_provider(&mut self, provider: Arc<dyn Provider>) -> Result<()> {
        self.registries
            .register_provider(self.owner.clone(), provider)
    }

    /// Returns a provider registered by an earlier provider plugin. A later
    /// plugin can use it as the fallback for a configuration overlay.
    pub fn base_provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.registries.provider(id)
    }

    /// Adds or replaces the provider selected for this generation. Two
    /// provider plugins may not override the same provider.
    pub fn register_provider_override(&mut self, provider: Arc<dyn Provider>) -> Result<()> {
        self.registries
            .register_provider_override(self.owner.clone(), provider)
    }

    pub fn register_model(&mut self, model: ModelSpec) -> Result<()> {
        self.registries.register_model(self.owner.clone(), model)
    }

    /// Returns model metadata registered by earlier provider plugins for one
    /// provider. A later catalog overlay can use this immutable snapshot to
    /// compose explicit user configuration.
    pub fn base_models(&self, provider: &ProviderId) -> Vec<ModelSpec> {
        self.registries.models_for_provider(provider)
    }

    /// Replaces one model registered by an earlier provider plugin. A model
    /// may be overridden at most once while constructing a generation.
    pub fn register_model_override(&mut self, model: ModelSpec) -> Result<()> {
        self.registries
            .register_model_override(self.owner.clone(), model)
    }
}

/// A provider-system plugin that contributes providers, routing overlays,
/// model catalog entries, and provider request lifecycle hooks.
#[async_trait]
pub trait ProviderPlugin: Send + Sync {
    fn id(&self) -> PluginId;

    fn register(&self, _context: &mut ProviderRegisterContext<'_>) -> Result<()> {
        Ok(())
    }

    /// Runs after a concrete provider has serialized its final wire payload and
    /// immediately before that payload is sent. Returning `None` preserves the
    /// current payload; returning `Some` replaces it for later plugins and the
    /// request itself.
    async fn before_provider_request(
        &self,
        _context: ProviderPluginContext,
        _event: BeforeProviderRequestEvent,
    ) -> std::result::Result<Option<Value>, PluginError> {
        Ok(None)
    }
}

struct RegisteredProviderPlugin {
    id: PluginId,
    plugin: Arc<dyn ProviderPlugin>,
}

/// Immutable, generation-local provider plugin set.
pub struct ProviderPluginDriver {
    plugins: Vec<RegisteredProviderPlugin>,
}

impl ProviderPluginDriver {
    pub fn new(plugins: Vec<Arc<dyn ProviderPlugin>>) -> Result<Self> {
        let mut seen = std::collections::HashSet::new();
        let mut registered = Vec::with_capacity(plugins.len());
        for plugin in plugins {
            let id = plugin.id();
            if !seen.insert(id.clone()) {
                return Err(CoreError::DuplicateProviderPlugin(id.to_string()));
            }
            registered.push(RegisteredProviderPlugin { id, plugin });
        }
        Ok(Self {
            plugins: registered,
        })
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.plugins
            .iter()
            .map(|plugin| plugin.id.clone())
            .collect()
    }

    pub fn register_all(&self, registries: &mut RegistriesBuilder) -> Result<()> {
        for registered in &self.plugins {
            let mut context = ProviderRegisterContext::new(registered.id.clone(), registries);
            registered.plugin.register(&mut context)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn before_provider_request(
        &self,
        generation: u64,
        provider_id: &ProviderId,
        model_id: &ModelId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut payload: Value,
    ) -> std::result::Result<Value, PluginError> {
        for registered in &self.plugins {
            let event = BeforeProviderRequestEvent {
                payload: payload.clone(),
            };
            let replacement = registered
                .plugin
                .before_provider_request(
                    ProviderPluginContext {
                        plugin_id: registered.id.clone(),
                        generation,
                        provider_id: provider_id.clone(),
                        model_id: model_id.clone(),
                        cwd: cwd.to_path_buf(),
                        abort_signal: signal.clone(),
                    },
                    event,
                )
                .await
                .map_err(|error| PluginError::Hook {
                    plugin_id: registered.id.clone(),
                    hook: "before_provider_request",
                    message: error.to_string(),
                })?;
            if let Some(replacement) = replacement {
                payload = replacement;
            }
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbortSignal, ModelId, ProviderError, ProviderRequest, ProviderStream};

    struct TestProvider(&'static str);

    #[async_trait::async_trait]
    impl Provider for TestProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new(self.0)
        }

        async fn stream(
            &self,
            _request: ProviderRequest,
            _context: crate::ProviderCallContext,
            _signal: AbortSignal,
        ) -> std::result::Result<ProviderStream, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    struct CatalogProviderPlugin;

    struct BaseProviderPlugin;

    struct PayloadPlugin {
        id: &'static str,
        field: &'static str,
    }

    #[async_trait]
    impl ProviderPlugin for PayloadPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn before_provider_request(
            &self,
            _context: ProviderPluginContext,
            event: BeforeProviderRequestEvent,
        ) -> std::result::Result<Option<Value>, PluginError> {
            let mut payload = event.payload;
            payload[self.field] = Value::String(self.id.to_string());
            Ok(Some(payload))
        }
    }

    impl ProviderPlugin for BaseProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("base")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            context.register_provider(Arc::new(TestProvider("custom")))
        }
    }

    impl ProviderPlugin for CatalogProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("models")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            assert!(context.base_provider(&ProviderId::new("custom")).is_some());
            context.register_provider_override(Arc::new(TestProvider("custom")))?;
            context.register_model(ModelSpec::new(
                "custom",
                "model",
                "Model",
                "openai-completions",
            ))
        }
    }

    #[test]
    fn provider_plugin_contribution_freezes_provider_and_catalog_together() {
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(
                Vec::new(),
                vec![
                    Arc::new(BaseProviderPlugin),
                    Arc::new(CatalogProviderPlugin),
                ],
            )
            .unwrap();
        let provider = ProviderId::new("custom");
        let model = ModelId::new("model");

        assert!(registries.provider(&provider).is_some());
        assert_eq!(
            registries.provider_owner(&provider),
            Some(&PluginId::new("models"))
        );
        assert_eq!(registries.model(&provider, &model).unwrap().name, "Model");
        assert_eq!(
            registries.model_owner(&provider, &model),
            Some(&PluginId::new("models"))
        );
    }

    #[tokio::test]
    async fn request_hooks_chain_in_provider_plugin_order_without_registering_a_provider() {
        let driver = ProviderPluginDriver::new(vec![
            Arc::new(PayloadPlugin {
                id: "first",
                field: "first",
            }),
            Arc::new(PayloadPlugin {
                id: "second",
                field: "second",
            }),
        ])
        .unwrap();
        let (_, signal) = crate::AbortHandle::new();

        let payload = driver
            .before_provider_request(
                7,
                &ProviderId::new("openai-compatible"),
                &ModelId::new("model"),
                std::path::Path::new("/project"),
                &signal,
                serde_json::json!({ "existing": true }),
            )
            .await
            .unwrap();

        assert_eq!(
            payload,
            serde_json::json!({
                "existing": true,
                "first": "first",
                "second": "second"
            })
        );
    }
}
