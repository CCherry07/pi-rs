//! Model catalogue and selection capabilities exposed to plugins.

use async_trait::async_trait;
use serde_json::Value;

use super::types::unbound;
use super::{PluginContextHandle, PluginContextResult, PluginContextScope, ScopedModel};
use crate::{ModelId, ModelSpec, ProviderId, ThinkingLevel};

/// Model-catalogue and selection access implemented by the owning product layer.
#[doc(hidden)]
#[async_trait]
pub trait ModelsContextAccess: Send + Sync {
    fn model(&self) -> PluginContextResult<Option<ModelSpec>> {
        unbound()
    }

    fn scoped_models(&self) -> PluginContextResult<Vec<ScopedModel>> {
        unbound()
    }

    fn models(&self) -> PluginContextResult<Vec<ModelSpec>> {
        unbound()
    }

    fn available_models(&self) -> PluginContextResult<Vec<ModelSpec>> {
        unbound()
    }

    fn provider_display_name(&self, _provider: &ProviderId) -> PluginContextResult<String> {
        unbound()
    }

    fn thinking_level(&self) -> PluginContextResult<Option<ThinkingLevel>> {
        unbound()
    }

    fn set_thinking_level(&self, _level: ThinkingLevel) -> PluginContextResult<()> {
        unbound()
    }

    fn register_provider(&self, _name: String, _config: Value) -> PluginContextResult<()> {
        unbound()
    }

    fn unregister_provider(&self, _name: String) -> PluginContextResult<()> {
        unbound()
    }

    async fn set_model(
        &self,
        _scope: PluginContextScope,
        _provider: ProviderId,
        _model_id: ModelId,
    ) -> PluginContextResult<bool> {
        unbound()
    }
}

/// Read-only view of the generation-local model catalogue and selection.
#[derive(Clone)]
pub struct ModelsContext {
    handle: PluginContextHandle,
}

/// Model capabilities available to command callbacks.
#[derive(Clone)]
pub struct CommandModelsContext {
    handle: PluginContextHandle,
}

macro_rules! impl_models_context {
    ($context:ident) => {
        impl $context {
            pub(super) fn from_handle(handle: PluginContextHandle) -> Self {
                Self { handle }
            }

            pub fn current(&self) -> PluginContextResult<Option<ModelSpec>> {
                self.handle.access()?.model()
            }

            pub fn scoped(&self) -> PluginContextResult<Vec<ScopedModel>> {
                self.handle.access()?.scoped_models()
            }

            pub fn thinking_level(&self) -> PluginContextResult<Option<ThinkingLevel>> {
                self.handle.access()?.thinking_level()
            }

            pub fn all(&self) -> PluginContextResult<Vec<ModelSpec>> {
                self.handle.access()?.models()
            }

            pub fn available(&self) -> PluginContextResult<Vec<ModelSpec>> {
                self.handle.access()?.available_models()
            }

            pub fn find(
                &self,
                provider: &ProviderId,
                model_id: &ModelId,
            ) -> PluginContextResult<Option<ModelSpec>> {
                Ok(self
                    .all()?
                    .into_iter()
                    .find(|model| &model.provider == provider && &model.id == model_id))
            }

            pub fn has_configured_auth(&self, model: &ModelSpec) -> PluginContextResult<bool> {
                Ok(self.available()?.into_iter().any(|candidate| {
                    candidate.provider == model.provider && candidate.id == model.id
                }))
            }

            pub fn provider_display_name(
                &self,
                provider: &ProviderId,
            ) -> PluginContextResult<String> {
                self.handle.access()?.provider_display_name(provider)
            }
        }
    };
}

impl_models_context!(ModelsContext);
impl_models_context!(CommandModelsContext);

impl CommandModelsContext {
    pub async fn set_current(
        &self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> PluginContextResult<bool> {
        let access = self.handle.access()?;
        access
            .set_model(
                self.handle.scope(),
                ProviderId::new(provider.into()),
                ModelId::new(model_id.into()),
            )
            .await
    }
}
