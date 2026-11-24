use super::context::{PiPluginContext, PluginProviderMutation, context_failed};
use async_trait::async_trait;
use pi_core::{ModelId, ProviderId, ThinkingLevel};
use pi_plugin::{ModelsContextAccess, PluginContextError, PluginContextScope};
use serde_json::Value;

#[async_trait]
impl ModelsContextAccess for PiPluginContext {
    fn model_selection(&self) -> Result<Option<pi_core::ModelSelection>, PluginContextError> {
        let (provider, model_id) = self.session()?.runtime().agent().model_selection();
        Ok(Some(pi_core::ModelSelection { provider, model_id }))
    }

    fn model(&self) -> Result<Option<pi_core::ModelSpec>, PluginContextError> {
        let session = self.session()?;
        let (provider_id, model_id) = session.runtime().agent().model_selection();
        Ok(session.runtime().model(&provider_id, &model_id))
    }

    fn scoped_models(&self) -> Result<Vec<pi_plugin::ScopedModel>, PluginContextError> {
        Ok(crate::resolve_model_scope(
            &self.model_scope_patterns,
            &self.session()?.runtime().available_models(),
        ))
    }

    fn models(&self) -> Result<Vec<pi_core::ModelSpec>, PluginContextError> {
        Ok(self.session()?.runtime().models())
    }

    fn available_models(&self) -> Result<Vec<pi_core::ModelSpec>, PluginContextError> {
        Ok(self.session()?.runtime().available_models())
    }

    fn models_of_type(
        &self,
        model_type: pi_core::ModelType,
        provider: Option<&ProviderId>,
    ) -> Result<Vec<pi_core::AnyModelSpec>, PluginContextError> {
        Ok(self
            .session()?
            .runtime()
            .models_of_type(model_type, provider))
    }

    fn available_models_of_type(
        &self,
        model_type: pi_core::ModelType,
        provider: Option<&ProviderId>,
    ) -> Result<Vec<pi_core::AnyModelSpec>, PluginContextError> {
        Ok(self
            .session()?
            .runtime()
            .available_models_of_type(model_type, provider))
    }

    async fn query_available_models_of_type(
        &self,
        model_type: pi_core::ModelType,
        provider: Option<&ProviderId>,
        signal: pi_core::AbortSignal,
    ) -> Result<Vec<pi_core::AnyModelSpec>, PluginContextError> {
        self.session()?
            .runtime()
            .query_available_models_of_type(model_type, provider, signal)
            .await
            .map_err(context_failed)
    }

    async fn classify_model(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        request: pi_core::ClassifierContext,
        signal: pi_core::AbortSignal,
    ) -> Result<pi_core::ClassifierResult, PluginContextError> {
        self.session()?
            .runtime()
            .classify(provider, model_id, request, signal)
            .await
            .map_err(context_failed)
    }

    async fn generate_model_images(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        request: pi_core::ImagesContext,
        signal: pi_core::AbortSignal,
    ) -> Result<pi_core::ImagesResult, PluginContextError> {
        self.session()?
            .runtime()
            .generate_images(provider, model_id, request, signal)
            .await
            .map_err(context_failed)
    }

    fn provider_display_name(&self, provider: &ProviderId) -> Result<String, PluginContextError> {
        Ok(self
            .session()?
            .runtime()
            .provider_name(provider)
            .unwrap_or_else(|| provider.to_string()))
    }

    fn thinking_level(&self) -> Result<Option<ThinkingLevel>, PluginContextError> {
        Ok(Some(self.session()?.runtime().agent().thinking_level()))
    }

    fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), PluginContextError> {
        self.session()?
            .set_thinking_level(level)
            .map_err(context_failed)
    }

    fn register_provider(&self, name: String, config: Value) -> Result<(), PluginContextError> {
        self.stage_provider_mutation(PluginProviderMutation::Register { name, config })
    }

    fn unregister_provider(&self, name: String) -> Result<(), PluginContextError> {
        self.stage_provider_mutation(PluginProviderMutation::Unregister { name })
    }

    async fn set_model(
        &self,
        scope: PluginContextScope,
        provider: ProviderId,
        model_id: ModelId,
    ) -> Result<bool, PluginContextError> {
        let _provider_reload = if scope == PluginContextScope::Command {
            Some(self.provider_reload_gate.lock().await)
        } else {
            None
        };
        if scope == PluginContextScope::Command {
            self.reload_pending_providers().await?;
        }
        let session = self.session()?;
        let runtime = session.runtime();
        if runtime.model(&provider, &model_id).is_none()
            || !runtime.provider_is_available(&provider)
        {
            return Ok(false);
        }
        session
            .set_model_with_hooks(provider, model_id)
            .await
            .map_err(context_failed)?;
        Ok(true)
    }
}
