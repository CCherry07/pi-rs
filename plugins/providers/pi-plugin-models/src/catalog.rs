//! Built-in classifier/image catalog generated from the fixed legacy Pi oracle.
//!
//! Regenerate with `python3 scripts/sync-specialized-model-catalog.py` and verify
//! with its `--check` mode. The generator reads only checked-in files.

use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn builtin_model_configs() -> BTreeMap<String, Value> {
    serde_json::from_str(include_str!("data/specialized.json"))
        .expect("checked-in specialized model catalog")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pi_core::{
        AbortHandle, AnyModelSpec, ClassifierContext, ContentBlock, ImagesContext, ModelSpec,
        ModelType, PluginId, TextContent,
    };
    use pi_plugin::{
        Provider, ProviderCallContext, ProviderPlugin, ProviderRegisterContext, RegistriesBuilder,
        Result,
    };
    use pi_provider::{HttpResponse, HttpTransport, TransportError};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct ChatProvider;
    #[async_trait]
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
    fn generated_builtins_register_complete_typed_metadata_without_replacing_chat() {
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
        assert_eq!(
            runtime.models_of_type(ModelType::Classifier, None).len(),
            46
        );
        assert_eq!(runtime.models_of_type(ModelType::Image, None).len(), 63);

        let classifier = runtime
            .model_of_type(
                ModelType::Classifier,
                &"openai".into(),
                &"gpt-6-luna".into(),
            )
            .unwrap();
        assert!(matches!(classifier, AnyModelSpec::Classifier(model)
            if model.api == "openai-decisions"
                && model.context_window == 922_000
                && model.input == vec![pi_core::ModelInput::Text, pi_core::ModelInput::Image]
                && model.cost.tiers[0].input_tokens_above == 272_000));

        let image = runtime
            .model_of_type(
                ModelType::Image,
                &"openrouter".into(),
                &"black-forest-labs/flux-3-image".into(),
            )
            .unwrap();
        assert!(matches!(image, AnyModelSpec::Image(model)
            if model.api == "openrouter-images"
                && model.output == vec![pi_core::ModelInput::Image]
                && model.input_limits.unwrap().images.unwrap().resize.unwrap().max_width == Some(2000)));
        assert!(
            runtime
                .model_of_type(
                    ModelType::Classifier,
                    &"vercel-ai-gateway".into(),
                    &"typesafe-ai/jev-1.14.0".into(),
                )
                .is_some()
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
            assert!(matches!(model,AnyModelSpec::Classifier(model)
                if model.api=="typesafe-system-one"
                    && model.base_url.as_deref()==Some("http://local-proxy.test/v2")));
        }
    }

    type CapturedRequest = (String, Value);

    #[derive(Default)]
    struct CatalogTransport {
        requests: Mutex<Vec<CapturedRequest>>,
    }

    #[async_trait]
    impl HttpTransport for CatalogTransport {
        async fn post_json(
            &self,
            url: &str,
            _headers: &BTreeMap<String, String>,
            body: &Value,
            _signal: pi_core::AbortSignal,
        ) -> std::result::Result<HttpResponse, TransportError> {
            self.requests
                .lock()
                .unwrap()
                .push((url.to_string(), body.clone()));
            let response = if url.ends_with("/decisions") {
                json!({"answers":[{"name":"approved","type":"predicate","probability":0.9}]})
            } else {
                assert!(url.ends_with("/chat/completions"), "{url}");
                json!({"id":"image-response","choices":[{"message":{"images":[
                    {"image_url":{"url":"data:image/png;base64,cG5n"}}
                ]}}]})
            };
            let bytes = response.to_string().into_bytes();
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".into()),
                headers: vec![],
                body: Box::pin(futures::stream::once(async move { Ok(bytes) })),
            })
        }
    }

    #[tokio::test]
    async fn generated_entries_drive_real_specialized_protocol_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let transport = Arc::new(CatalogTransport::default());
        let options = crate::ModelsPluginOptions::new(directory.path().join("models.json"))
            .with_builtin_specialized_models()
            .runtime_api_key("openai", "openai-fixture")
            .runtime_api_key("openrouter", "openrouter-fixture");
        let plugin = crate::ModelsPlugin::load_with_transport(options, transport.clone()).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(vec![], vec![Arc::new(plugin)])
            .unwrap();
        let runtime = registries.model_runtime();

        let AnyModelSpec::Classifier(classifier) = runtime
            .model_of_type(
                ModelType::Classifier,
                &"openai".into(),
                &"gpt-6-luna".into(),
            )
            .unwrap()
        else {
            panic!("classifier model")
        };
        let classifier_context: ClassifierContext = serde_json::from_value(json!({
            "state":{"text":"ship it"},
            "questions":{"approved":{"type":"bool","instructions":"Approve?","criteria":{"true":"yes","false":"no"}}}
        })).unwrap();
        let classifier_result = registries
            .provider(&classifier.provider)
            .unwrap()
            .classify(
                classifier.clone(),
                classifier_context,
                ProviderCallContext::without_plugins(
                    "/",
                    classifier.provider.clone(),
                    classifier.id.clone(),
                ),
                AbortHandle::new().1,
            )
            .await
            .unwrap();
        assert_eq!(
            classifier_result.stop_reason,
            pi_core::ModelCallStopReason::Stop
        );

        let AnyModelSpec::Image(image) = runtime
            .model_of_type(
                ModelType::Image,
                &"openrouter".into(),
                &"black-forest-labs/flux-3-image".into(),
            )
            .unwrap()
        else {
            panic!("image model")
        };
        let image_result = registries
            .provider(&image.provider)
            .unwrap()
            .generate_images(
                image.clone(),
                ImagesContext {
                    input: vec![ContentBlock::Text(TextContent::new("draw a tree"))],
                },
                ProviderCallContext::without_plugins("/", image.provider.clone(), image.id.clone()),
                AbortHandle::new().1,
            )
            .await
            .unwrap();
        assert_eq!(image_result.stop_reason, pi_core::ModelCallStopReason::Stop);
        assert!(
            matches!(&image_result.output[0], ContentBlock::Image(value) if value.mime_type == "image/png")
        );

        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].0, "https://api.openai.com/v1/decisions");
        assert_eq!(requests[0].1["model"], "gpt-6-luna");
        assert_eq!(requests[0].1["questions"][0]["name"], "approved");
        assert_eq!(
            requests[1].0,
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(requests[1].1["model"], "black-forest-labs/flux-3-image");
        assert_eq!(requests[1].1["modalities"], json!(["image"]));
    }
}
