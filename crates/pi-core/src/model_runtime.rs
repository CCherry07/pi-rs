use std::collections::HashMap;
use std::sync::Arc;

use crate::{ModelId, ModelSpec, PluginId, Provider, ProviderId};

type ModelKey = (ProviderId, ModelId);

/// Read-only provider and model snapshot for one runtime generation.
pub struct ModelRuntime {
    providers: HashMap<ProviderId, (PluginId, Arc<dyn Provider>)>,
    models: HashMap<ModelKey, (PluginId, ModelSpec)>,
}

impl ModelRuntime {
    pub(crate) fn new(
        providers: HashMap<ProviderId, (PluginId, Arc<dyn Provider>)>,
        models: HashMap<ModelKey, (PluginId, ModelSpec)>,
    ) -> Self {
        Self { providers, models }
    }

    pub fn provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.providers
            .get(id)
            .map(|(_, provider)| Arc::clone(provider))
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
