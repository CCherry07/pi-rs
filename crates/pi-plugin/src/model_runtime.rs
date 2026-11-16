use std::collections::HashMap;
use std::sync::Arc;

use crate::{ModelId, ModelSpec, PluginId, Provider, ProviderAvailability, ProviderId};

type ModelKey = (ProviderId, ModelId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub provider: ProviderId,
    pub availability: ProviderAvailability,
}

/// Read-only provider and model snapshot for one runtime generation.
#[derive(Clone)]
pub struct ModelRuntime {
    providers: HashMap<ProviderId, (PluginId, Arc<dyn Provider>)>,
    models: HashMap<ModelKey, (PluginId, ModelSpec)>,
    typed_models: HashMap<(crate::ModelType, ProviderId, ModelId), (PluginId, crate::AnyModelSpec)>,
}

impl ModelRuntime {
    pub(crate) fn new(
        providers: HashMap<ProviderId, (PluginId, Arc<dyn Provider>)>,
        models: HashMap<ModelKey, (PluginId, ModelSpec)>,
        typed_models: HashMap<
            (crate::ModelType, ProviderId, ModelId),
            (PluginId, crate::AnyModelSpec),
        >,
    ) -> Self {
        Self {
            providers,
            models,
            typed_models,
        }
    }

    pub fn provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.providers
            .get(id)
            .map(|(_, provider)| Arc::clone(provider))
    }

    pub fn provider_name(&self, id: &ProviderId) -> Option<String> {
        self.providers.get(id).map(|(_, provider)| provider.name())
    }

    pub fn has_providers(&self) -> bool {
        !self.providers.is_empty()
    }

    pub fn provider_owner(&self, id: &ProviderId) -> Option<&PluginId> {
        self.providers.get(id).map(|(owner, _)| owner)
    }

    pub fn model(&self, provider: &ProviderId, id: &ModelId) -> Option<&ModelSpec> {
        self.models
            .get(&(provider.clone(), id.clone()))
            .map(|(_, model)| model)
    }

    pub fn model_owner(&self, provider: &ProviderId, id: &ModelId) -> Option<&PluginId> {
        self.models
            .get(&(provider.clone(), id.clone()))
            .map(|(owner, _)| owner)
    }

    pub fn models(&self) -> Vec<ModelSpec> {
        let mut models = self
            .models
            .values()
            .map(|(_, model)| model.clone())
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then_with(|| left.id.cmp(&right.id))
        });
        models
    }

    pub fn available_models(&self) -> Vec<ModelSpec> {
        self.models()
            .into_iter()
            .filter(|model| self.model_is_available(model))
            .collect()
    }

    pub fn model_of_type(
        &self,
        model_type: crate::ModelType,
        provider: &ProviderId,
        id: &ModelId,
    ) -> Option<crate::AnyModelSpec> {
        if model_type == crate::ModelType::Chat {
            return self
                .model(provider, id)
                .cloned()
                .map(crate::AnyModelSpec::Chat);
        }
        self.typed_models
            .get(&(model_type, provider.clone(), id.clone()))
            .map(|(_, model)| model.clone())
    }

    pub fn models_of_type(
        &self,
        model_type: crate::ModelType,
        provider: Option<&ProviderId>,
    ) -> Vec<crate::AnyModelSpec> {
        let mut models = if model_type == crate::ModelType::Chat {
            self.models()
                .into_iter()
                .map(crate::AnyModelSpec::Chat)
                .collect::<Vec<_>>()
        } else {
            self.typed_models
                .values()
                .filter(|(_, model)| model.model_type() == model_type)
                .map(|(_, model)| model.clone())
                .collect()
        };
        models.retain(|model| provider.is_none_or(|provider| model.provider() == provider));
        models.sort_by(|left, right| {
            left.provider()
                .cmp(right.provider())
                .then_with(|| left.id().cmp(right.id()))
        });
        models
    }

    pub fn available_models_of_type(
        &self,
        model_type: crate::ModelType,
        provider: Option<&ProviderId>,
    ) -> Vec<crate::AnyModelSpec> {
        self.models_of_type(model_type, provider)
            .into_iter()
            .filter(|model| {
                self.providers
                    .get(model.provider())
                    .is_some_and(|(_, provider)| provider.model_availability(model).is_available())
            })
            .collect()
    }

    pub fn resolve_available_reference(
        &self,
        current_provider: &ProviderId,
        reference: &str,
    ) -> Option<ModelSpec> {
        let model = self.resolve_reference(current_provider, reference)?;
        self.model_is_available(&model).then_some(model)
    }

    fn model_is_available(&self, model: &ModelSpec) -> bool {
        self.providers
            .get(&model.provider)
            .is_some_and(|(_, provider)| provider.availability().is_available())
    }

    pub fn provider_statuses(&self) -> Vec<ProviderStatus> {
        let mut statuses = self
            .providers
            .iter()
            .map(|(provider, (_, implementation))| ProviderStatus {
                provider: provider.clone(),
                availability: implementation.availability(),
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|left, right| left.provider.cmp(&right.provider));
        statuses
    }

    /// Resolves an id, `provider/id`, or unique display name against this
    /// immutable snapshot. The current provider wins when model ids contain
    /// slashes of their own.
    pub fn resolve_reference(
        &self,
        current_provider: &ProviderId,
        reference: &str,
    ) -> Option<ModelSpec> {
        let direct = ModelId::new(reference);
        if let Some(model) = self.model(current_provider, &direct) {
            return Some(model.clone());
        }
        if let Some((provider, id)) = reference.split_once('/') {
            let provider = ProviderId::new(provider);
            let id = ModelId::new(id);
            if let Some(model) = self.model(&provider, &id) {
                return Some(model.clone());
            }
        }
        let mut matches = self
            .models
            .values()
            .map(|(_, model)| model)
            .filter(|model| model.id.as_str() == reference || model.name == reference);
        let first = matches.next()?.clone();
        matches.next().is_none().then_some(first)
    }
}
