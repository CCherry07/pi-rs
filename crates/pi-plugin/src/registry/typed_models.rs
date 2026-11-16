use crate::{AnyModelSpec, CoreError, ModelType, PluginId, ProviderId, Result};

use super::RegistriesBuilder;

impl RegistriesBuilder {
    pub(crate) fn register_model_of_type(
        &mut self,
        owner: PluginId,
        model: AnyModelSpec,
    ) -> Result<()> {
        if let AnyModelSpec::Chat(model) = model {
            return self.register_model(owner, model);
        }
        if !self.providers.contains_key(model.provider()) {
            return Err(CoreError::ModelProviderNotFound {
                provider: model.provider().to_string(),
                model: model.id().to_string(),
            });
        }
        let key = (
            model.model_type(),
            model.provider().clone(),
            model.id().clone(),
        );
        if self.typed_models.contains_key(&key) {
            return Err(CoreError::DuplicateModel(format!(
                "{:?}/{}/{}",
                model.model_type(),
                model.provider(),
                model.id()
            )));
        }
        self.typed_models.insert(key, (owner, model));
        Ok(())
    }

    pub(crate) fn models_for_provider_of_type(
        &self,
        model_type: ModelType,
        provider: &ProviderId,
    ) -> Vec<AnyModelSpec> {
        if model_type == ModelType::Chat {
            return self
                .models_for_provider(provider)
                .into_iter()
                .map(AnyModelSpec::Chat)
                .collect();
        }
        let mut models = self
            .typed_models
            .iter()
            .filter(|((kind, candidate, _), _)| *kind == model_type && candidate == provider)
            .map(|(_, (_, model))| model.clone())
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id().cmp(right.id()));
        models
    }

    pub(crate) fn replace_provider_models_of_type(
        &mut self,
        owner: PluginId,
        provider: ProviderId,
        model_type: ModelType,
        models: Vec<AnyModelSpec>,
    ) -> Result<()> {
        if !self.providers.contains_key(&provider) {
            return Err(CoreError::ProviderNotFound(provider.to_string()));
        }
        let mut seen = std::collections::HashSet::new();
        for model in &models {
            if model.model_type() != model_type || model.provider() != &provider {
                return Err(CoreError::ModelProviderNotFound {
                    provider: model.provider().to_string(),
                    model: model.id().to_string(),
                });
            }
            if !seen.insert(model.id().clone()) {
                return Err(CoreError::DuplicateModel(format!(
                    "{model_type:?}/{provider}/{}",
                    model.id()
                )));
            }
        }
        if model_type == ModelType::Chat {
            return self.replace_provider_models(
                owner,
                provider,
                models
                    .into_iter()
                    .filter_map(|model| match model {
                        AnyModelSpec::Chat(model) => Some(model),
                        _ => None,
                    })
                    .collect(),
            );
        }
        if !self
            .typed_catalog_replacements
            .insert((model_type, provider.clone()))
        {
            return Err(CoreError::DuplicateModelCatalogReplacement(format!(
                "{model_type:?}/{provider}"
            )));
        }
        self.typed_models
            .retain(|(kind, candidate, _), _| *kind != model_type || candidate != &provider);
        for model in models {
            self.typed_models.insert(
                (model_type, provider.clone(), model.id().clone()),
                (owner.clone(), model),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::{
        ClassifierModelSpec, ImageModelSpec, ModelId, ModelSpec, Provider, ProviderAvailability,
    };

    struct CatalogProvider;

    #[async_trait]
    impl Provider for CatalogProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("catalog")
        }

        fn model_availability(&self, model: &AnyModelSpec) -> ProviderAvailability {
            if model.model_type() == ModelType::Image {
                ProviderAvailability::MissingCredentials
            } else {
                ProviderAvailability::Available
            }
        }
    }

    fn classifier(id: &str) -> AnyModelSpec {
        AnyModelSpec::Classifier(ClassifierModelSpec::new(
            "catalog",
            id,
            "Classifier",
            "classify",
        ))
    }

    fn image(id: &str) -> AnyModelSpec {
        AnyModelSpec::Image(ImageModelSpec::new("catalog", id, "Image", "images"))
    }

    fn builder() -> RegistriesBuilder {
        let mut builder = RegistriesBuilder::new();
        builder
            .register_provider(PluginId::new("provider"), Arc::new(CatalogProvider))
            .unwrap();
        builder
    }

    #[test]
    fn typed_catalogs_allow_shared_ids_and_keep_chat_resolution_separate() {
        let mut builder = builder();
        let owner = PluginId::new("models");
        builder
            .register_model(
                owner.clone(),
                ModelSpec::new("catalog", "same", "Chat", "chat"),
            )
            .unwrap();
        builder
            .register_model_of_type(owner.clone(), classifier("same"))
            .unwrap();
        builder
            .register_model_of_type(owner, image("same"))
            .unwrap();
        let registries = builder.freeze();
        let runtime = registries.model_runtime();
        let provider = ProviderId::new("catalog");
        let id = ModelId::new("same");
        assert_eq!(runtime.models().len(), 1);
        assert_eq!(
            runtime.resolve_reference(&provider, "same").unwrap().name,
            "Chat"
        );
        assert!(runtime.resolve_reference(&provider, "Image").is_none());
        for kind in [ModelType::Chat, ModelType::Classifier, ModelType::Image] {
            assert_eq!(
                runtime
                    .model_of_type(kind, &provider, &id)
                    .unwrap()
                    .model_type(),
                kind
            );
            assert_eq!(runtime.models_of_type(kind, Some(&provider)).len(), 1);
            assert!(
                runtime
                    .models_of_type(kind, Some(&ProviderId::new("other")))
                    .is_empty()
            );
        }
        assert_eq!(
            runtime
                .available_models_of_type(ModelType::Classifier, None)
                .len(),
            1
        );
        assert!(
            runtime
                .available_models_of_type(ModelType::Image, None)
                .is_empty()
        );
    }

    #[test]
    fn typed_registration_requires_provider_and_unique_per_type_identity() {
        let owner = PluginId::new("models");
        let mut missing = RegistriesBuilder::new();
        assert!(matches!(
            missing.register_model_of_type(owner.clone(), classifier("one")),
            Err(CoreError::ModelProviderNotFound { .. })
        ));
        let mut builder = builder();
        builder
            .register_model_of_type(owner.clone(), classifier("one"))
            .unwrap();
        assert!(matches!(
            builder.register_model_of_type(owner.clone(), classifier("one")),
            Err(CoreError::DuplicateModel(_))
        ));
        builder.register_model_of_type(owner, image("one")).unwrap();
    }

    #[test]
    fn replacements_validate_before_mutation_and_affect_only_the_selected_type() {
        let owner = PluginId::new("models");
        let provider = ProviderId::new("catalog");
        let mut builder = builder();
        builder
            .register_model_of_type(owner.clone(), classifier("old"))
            .unwrap();
        builder
            .register_model_of_type(owner.clone(), image("old"))
            .unwrap();
        for invalid in [
            vec![image("wrong-type")],
            vec![classifier("duplicate"), classifier("duplicate")],
        ] {
            assert!(
                builder
                    .replace_provider_models_of_type(
                        owner.clone(),
                        provider.clone(),
                        ModelType::Classifier,
                        invalid
                    )
                    .is_err()
            );
            assert_eq!(
                builder.models_for_provider_of_type(ModelType::Classifier, &provider),
                vec![classifier("old")]
            );
        }
        builder
            .replace_provider_models_of_type(
                owner.clone(),
                provider.clone(),
                ModelType::Classifier,
                vec![classifier("b"), classifier("a")],
            )
            .unwrap();
        assert_eq!(
            builder.models_for_provider_of_type(ModelType::Classifier, &provider),
            vec![classifier("a"), classifier("b")]
        );
        assert_eq!(
            builder.models_for_provider_of_type(ModelType::Image, &provider),
            vec![image("old")]
        );
        assert!(matches!(
            builder.replace_provider_models_of_type(
                owner,
                provider,
                ModelType::Classifier,
                Vec::new()
            ),
            Err(CoreError::DuplicateModelCatalogReplacement(_))
        ));
    }
}
