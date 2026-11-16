//! Non-chat model operations capture one immutable generation and never enter the chat loop.
use pi_core::{
    AbortSignal, AnyModelSpec, ClassifierContext, ClassifierResult, ImagesContext, ImagesResult,
    ModelCallStopReason, ModelId, ModelInput, ModelType, ProviderId,
};
use pi_plugin::{ProviderCallContext, ProviderError};

use crate::{PiRuntime, RuntimeError, now_ms};

impl PiRuntime {
    pub fn models_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&ProviderId>,
    ) -> Vec<AnyModelSpec> {
        self.current_generation()
            .agent
            .registries()
            .model_runtime()
            .models_of_type(model_type, provider)
    }

    pub fn available_models_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&ProviderId>,
    ) -> Vec<AnyModelSpec> {
        self.current_generation()
            .agent
            .registries()
            .model_runtime()
            .available_models_of_type(model_type, provider)
    }

    pub fn model_of_type(
        &self,
        model_type: ModelType,
        provider: &ProviderId,
        model_id: &ModelId,
    ) -> Option<AnyModelSpec> {
        self.current_generation()
            .agent
            .registries()
            .model_runtime()
            .model_of_type(model_type, provider, model_id)
    }

    pub async fn classify(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        request: ClassifierContext,
        signal: AbortSignal,
    ) -> Result<ClassifierResult, RuntimeError> {
        request.validate().map_err(RuntimeError::Input)?;
        let generation = self.current_generation();
        let runtime = &generation.agent;
        let Some(AnyModelSpec::Classifier(model)) = runtime
            .registries()
            .model_runtime()
            .model_of_type(ModelType::Classifier, &provider, &model_id)
        else {
            return Err(RuntimeError::Provider(format!(
                "Unknown classifier model {provider}/{model_id}"
            )));
        };
        let implementation = runtime
            .registries()
            .provider(&provider)
            .ok_or_else(|| RuntimeError::Provider(format!("Provider {provider} is unavailable")))?;
        let context = ProviderCallContext::new(
            runtime.generation(),
            self.cwd(),
            provider,
            model_id,
            std::sync::Arc::clone(runtime.provider_plugins()),
        );
        if !request.images.is_empty() && !model.input.contains(&ModelInput::Image) {
            return Ok(ClassifierResult::failure(
                &model,
                "Classifier model does not accept images".into(),
                ModelCallStopReason::Error,
                now_ms(),
            ));
        }
        let result = tokio::select! {
            biased;
            () = signal.wait() => Err(ProviderError::Aborted),
            result = implementation.classify(model.clone(), request, context, signal.clone()) => result,
        };
        Ok(result.unwrap_or_else(|error| {
            let reason = if matches!(error, ProviderError::Aborted) {
                ModelCallStopReason::Aborted
            } else {
                ModelCallStopReason::Error
            };
            ClassifierResult::failure(&model, error.to_string(), reason, now_ms())
        }))
    }

    pub async fn generate_images(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        request: ImagesContext,
        signal: AbortSignal,
    ) -> Result<ImagesResult, RuntimeError> {
        request.validate().map_err(RuntimeError::Input)?;
        let generation = self.current_generation();
        let runtime = &generation.agent;
        let Some(AnyModelSpec::Image(model)) = runtime.registries().model_runtime().model_of_type(
            ModelType::Image,
            &provider,
            &model_id,
        ) else {
            return Err(RuntimeError::Provider(format!(
                "Unknown image model {provider}/{model_id}"
            )));
        };
        let implementation = runtime
            .registries()
            .provider(&provider)
            .ok_or_else(|| RuntimeError::Provider(format!("Provider {provider} is unavailable")))?;
        let context = ProviderCallContext::new(
            runtime.generation(),
            self.cwd(),
            provider,
            model_id,
            std::sync::Arc::clone(runtime.provider_plugins()),
        );
        if request
            .input
            .iter()
            .any(|block| matches!(block, pi_core::ContentBlock::Image(_)))
            && !model.input.contains(&ModelInput::Image)
        {
            return Ok(ImagesResult::failure(
                &model,
                "Image model does not accept image references".into(),
                ModelCallStopReason::Error,
                now_ms(),
            ));
        }
        let result = tokio::select! {
            biased;
            () = signal.wait() => Err(ProviderError::Aborted),
            result = implementation.generate_images(model.clone(), request, context, signal.clone()) => result,
        };
        Ok(result.unwrap_or_else(|error| {
            let reason = if matches!(error, ProviderError::Aborted) {
                ModelCallStopReason::Aborted
            } else {
                ModelCallStopReason::Error
            };
            ImagesResult::failure(&model, error.to_string(), reason, now_ms())
        }))
    }
}

#[cfg(test)]
mod tests;
