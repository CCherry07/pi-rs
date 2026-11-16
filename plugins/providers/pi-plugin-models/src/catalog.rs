//! Small built-in specialized catalog, verified against legacy Pi provider data.
//!
//! Sources: legacy/pi/packages/ai/src/providers/{typesafe,openai,openrouter}.models.ts
//! and their data/*.json snapshots. This intentionally selects a few usable defaults;
//! callers add other models with models.json or their provider catalog plugins.

use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn builtin_model_configs() -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "typesafe".into(),
            json!({"name":"TypeSafe","models":[{
                "type":"classifier","id":"jev-latest","name":"Jev",
                "api":"typesafe-system-one","baseUrl":"https://api.typesafe.ai/v1/",
                "input":["text"],"contextWindow":64000,
                "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
            }]}),
        ),
        (
            "openai".into(),
            json!({"models":[{
                "type":"classifier","id":"gpt-6-luna","name":"GPT-6 Luna",
                "api":"openai-decisions","baseUrl":"https://api.openai.com/v1",
                "input":["text","image"],"contextWindow":922000,
                "cost":{"input":0.1,"output":0,"cacheRead":0,"cacheWrite":0,
                    "tiers":[{"inputTokensAbove":272000,"input":0.2,"output":0,"cacheRead":0,"cacheWrite":0}]}
            }]}),
        ),
        (
            "openrouter".into(),
            json!({"models":[{
                "type":"classifier","id":"~typesafe/jev-latest","name":"TypeSafe: Jev Latest",
                "api":"typesafe-system-one","baseUrl":"https://openrouter.ai/api/v1",
                "input":["text","image"],"contextWindow":262144,
                "cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0}
            },{
                "type":"classifier","id":"typesafe/jev-1.14","name":"TypeSafe: Jev 1.14",
                "api":"typesafe-system-one","baseUrl":"https://openrouter.ai/api/v1",
                "input":["text","image"],"contextWindow":262144,
                "cost":{"input":0.042,"output":0,"cacheRead":0,"cacheWrite":0}
            },{
                "type":"image","id":"google/gemini-3.1-flash-image","name":"Google: Nano Banana 2 (Gemini 3.1 Flash Image)",
                "api":"openrouter-images","baseUrl":"https://openrouter.ai/api/v1",
                "input":["image","text"],"output":["image","text"],
                "cost":{"input":0.5,"output":3,"cacheRead":0,"cacheWrite":0}
            },{
                "type":"image","id":"openai/gpt-image-2","name":"OpenAI: GPT Image 2",
                "api":"openrouter-images","baseUrl":"https://openrouter.ai/api/v1",
                "input":["text","image"],"output":["image"],
                "cost":{"input":8,"output":8,"cacheRead":2,"cacheWrite":0}
            }]}),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{AnyModelSpec, ModelSpec, ModelType, PluginId};
    use pi_plugin::{Provider, ProviderPlugin, ProviderRegisterContext, RegistriesBuilder, Result};
    use std::sync::Arc;

    struct ChatProvider;
    #[async_trait::async_trait]
    impl Provider for ChatProvider {
        fn id(&self) -> pi_core::ProviderId {
            "openai".into()
        }
    }
    struct ChatCatalog;
    #[pi_plugin::provider_plugin]
    impl ProviderPlugin for ChatCatalog {
        fn id(&self) -> PluginId {
            "chat-catalog".into()
        }
        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            context.register_provider(Arc::new(ChatProvider))?;
            let mut model = ModelSpec::new("openai", "gpt-6-luna", "Chat Luna", "openai-responses");
            model.base_url = Some("http://chat-provider.test/v1".into());
            model.context_window = 123456;
            context.register_model(model)
        }
    }

    #[test]
    fn builtins_add_typed_catalogs_without_replacing_existing_chat_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let options = crate::ModelsPluginOptions::new(directory.path().join("models.json"))
            .with_builtin_specialized_models();
        let plugin = crate::ModelsPlugin::load(options).unwrap();
        let (_, _, before) = RegistriesBuilder::new()
            .register_plugin_sets(vec![], vec![Arc::new(ChatCatalog)])
            .unwrap();
        let (_, _, after) = RegistriesBuilder::new()
            .register_plugin_sets(vec![], vec![Arc::new(ChatCatalog), Arc::new(plugin)])
            .unwrap();
        let runtime = after.model_runtime();
        assert_eq!(runtime.models(), before.model_runtime().models());
        assert_eq!(runtime.models_of_type(ModelType::Classifier, None).len(), 4);
        assert_eq!(runtime.models_of_type(ModelType::Image, None).len(), 2);
        assert!(
            runtime
                .available_models_of_type(ModelType::Classifier, None)
                .is_empty()
        );
        assert!(
            matches!(runtime.model_of_type(ModelType::Classifier,&"openai".into(),&"gpt-6-luna".into()),Some(AnyModelSpec::Classifier(model)) if model.api=="openai-decisions" && model.context_window==922000)
        );
    }

    #[test]
    fn file_routing_overrides_specialized_defaults_and_typed_custom_models_upsert() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        std::fs::write(&path,json!({"providers":{"typesafe":{"baseUrl":"http://local-proxy.test/v2","models":[{"type":"classifier","id":"custom"}]}}}).to_string()).unwrap();
        let plugin = crate::ModelsPlugin::load(
            crate::ModelsPluginOptions::new(&path).with_builtin_specialized_models(),
        )
        .unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(vec![], vec![Arc::new(plugin)])
            .unwrap();
        let models = registries
            .model_runtime()
            .models_of_type(ModelType::Classifier, Some(&"typesafe".into()));
        assert_eq!(models.len(), 2);
        for model in models {
            assert!(
                matches!(model,AnyModelSpec::Classifier(model) if model.api=="typesafe-system-one" && model.base_url.as_deref()==Some("http://local-proxy.test/v2"))
            );
        }
    }
}
