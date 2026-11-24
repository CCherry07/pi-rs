use std::collections::HashMap;
use std::sync::Arc;

use crate::{ModelId, ModelSpec, PluginId, Provider, ProviderAvailability, ProviderId};

#[cfg(test)]
mod tests;

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
    provider_order: Vec<ProviderId>,
    model_order: Vec<ModelKey>,
    typed_model_order: Vec<(crate::ModelType, ProviderId, ModelId)>,
}

impl ModelRuntime {
    pub(crate) fn new(
        providers: HashMap<ProviderId, (PluginId, Arc<dyn Provider>)>,
        models: HashMap<ModelKey, (PluginId, ModelSpec)>,
        typed_models: HashMap<
            (crate::ModelType, ProviderId, ModelId),
            (PluginId, crate::AnyModelSpec),
        >,
        provider_order: Vec<ProviderId>,
        model_order: Vec<ModelKey>,
        typed_model_order: Vec<(crate::ModelType, ProviderId, ModelId)>,
    ) -> Self {
        Self {
            providers,
            models,
            typed_models,
            provider_order,
            model_order,
            typed_model_order,
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
        self.provider_order
            .iter()
            .filter(|candidate| provider.is_none_or(|provider| *candidate == provider))
            .flat_map(|provider| {
                if model_type == crate::ModelType::Chat {
                    self.model_order
                        .iter()
                        .filter(|(candidate, _)| candidate == provider)
                        .filter_map(|key| {
                            self.models
                                .get(key)
                                .map(|(_, model)| crate::AnyModelSpec::Chat(model.clone()))
                        })
                        .collect::<Vec<_>>()
                } else {
                    self.typed_model_order
                        .iter()
                        .filter(|(kind, candidate, _)| *kind == model_type && candidate == provider)
                        .filter_map(|key| {
                            self.typed_models.get(key).map(|(_, model)| model.clone())
                        })
                        .collect()
                }
            })
            .collect()
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

    /// Authenticates providers once per query, then applies their filters to the
    /// canonical frozen catalog. Provider checks run concurrently, in registration
    /// order; a cancelled query drops every outstanding check.
    pub async fn query_available_models_of_type(
        &self,
        model_type: crate::ModelType,
        provider: Option<&ProviderId>,
        signal: crate::AbortSignal,
    ) -> Result<Vec<crate::AnyModelSpec>, crate::ProviderError> {
        if signal.is_aborted() {
            return Err(crate::ProviderError::Aborted);
        }
        let checks = self
            .provider_order
            .iter()
            .filter(|id| provider.is_none_or(|selected| *id == selected))
            .filter_map(|id| {
                self.providers
                    .get(id)
                    .map(|(_, implementation)| (id, implementation))
            })
            .map(|(id, implementation)| {
                let signal = signal.clone();
                async move {
                    // Authentication precedes the requested type filter, as in Pi.
                    let models = [
                        crate::ModelType::Chat,
                        crate::ModelType::Classifier,
                        crate::ModelType::Image,
                    ]
                    .into_iter()
                    .flat_map(|kind| self.models_of_type(kind, Some(id)))
                    .collect();
                    let available = implementation
                        .filter_available_models(models, signal)
                        .await?;
                    let identities = available
                        .into_iter()
                        .map(|model| {
                            (
                                model.model_type(),
                                model.provider().clone(),
                                model.id().clone(),
                            )
                        })
                        .collect::<std::collections::HashSet<_>>();
                    Ok::<_, crate::ProviderError>(
                        self.models_of_type(model_type, Some(id))
                            .into_iter()
                            .filter(|model| {
                                identities.contains(&(
                                    model_type,
                                    model.provider().clone(),
                                    model.id().clone(),
                                ))
                            })
                            .collect::<Vec<_>>(),
                    )
                }
            });
        match futures::future::select(
            Box::pin(signal.wait()),
            Box::pin(futures::future::try_join_all(checks)),
        )
        .await
        {
            futures::future::Either::Left(_) => Err(crate::ProviderError::Aborted),
            futures::future::Either::Right((result, _)) => {
                Ok(result?.into_iter().flatten().collect())
            }
        }
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
            .is_some_and(|(_, provider)| {
                provider
                    .model_availability(&crate::AnyModelSpec::Chat(model.clone()))
                    .is_available()
            })
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
