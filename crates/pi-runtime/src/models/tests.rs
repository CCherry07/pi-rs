use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, ClassifierAnswer, ClassifierModelSpec, ClassifierQuestion, ContentBlock,
    ImageContent, ImageModelSpec, ModelSpec, TextContent, Usage,
};
use pi_plugin::{
    BeforeProviderRequestEvent, PluginContextError, PluginContextScope, PluginError, PluginId,
    Provider, ProviderPlugin, ProviderPluginContext, ProviderRegisterContext,
};
use serde_json::{Value, json};

use super::*;

#[derive(Clone, Copy)]
enum Behavior {
    Success,
    Failure,
    Wait,
}

#[derive(Default)]
struct Observed {
    calls: Mutex<Vec<(AnyModelSpec, Value)>>,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct FixtureProvider {
    observed: Arc<Observed>,
    behavior: Behavior,
}

impl FixtureProvider {
    async fn observe(
        &self,
        model: AnyModelSpec,
        context: ProviderCallContext,
        signal: &AbortSignal,
    ) -> Result<(), ProviderError> {
        assert_eq!(context.provider_id(), model.provider());
        assert_eq!(context.model_id(), model.id());
        let payload = context
            .before_provider_request(signal, json!({"model": model.id()}))
            .await?;
        self.observed.calls.lock().unwrap().push((model, payload));
        self.observed.started.notify_one();
        match self.behavior {
            Behavior::Success => Ok(()),
            Behavior::Failure => Err(ProviderError::Failure("fixture failure".into())),
            Behavior::Wait => {
                self.observed.release.notified().await;
                Ok(())
            }
        }
    }
}

#[async_trait]
impl Provider for FixtureProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("typed")
    }

    async fn classify(
        &self,
        model: ClassifierModelSpec,
        _: ClassifierContext,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ClassifierResult, ProviderError> {
        self.observe(AnyModelSpec::Classifier(model.clone()), context, &signal)
            .await?;
        Ok(ClassifierResult {
            api: model.api,
            provider: model.provider,
            model: model.id,
            answers: [(
                "quality".into(),
                ClassifierAnswer::Score {
                    score: 1.0,
                    confidence: 0.9,
                },
            )]
            .into_iter()
            .collect(),
            usage: Some(Usage {
                input: 3,
                output: 2,
                total_tokens: 5,
                ..Default::default()
            }),
            stop_reason: ModelCallStopReason::Stop,
            error_message: None,
            timestamp_ms: 123,
        })
    }

    async fn generate_images(
        &self,
        model: ImageModelSpec,
        _: ImagesContext,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ImagesResult, ProviderError> {
        self.observe(AnyModelSpec::Image(model.clone()), context, &signal)
            .await?;
        Ok(ImagesResult {
            api: model.api,
            provider: model.provider,
            model: model.id,
            output: vec![ContentBlock::Image(ImageContent {
                data: "cG5n".into(),
                mime_type: "image/png".into(),
            })],
            response_id: Some("image-response".into()),
            usage: None,
            stop_reason: ModelCallStopReason::Stop,
            error_message: None,
            timestamp_ms: 456,
        })
    }
}

struct FixturePlugin {
    build: usize,
    provider: Arc<FixtureProvider>,
}

#[pi_plugin::provider_plugin]
impl ProviderPlugin for FixturePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("typed-fixture")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.provider.clone())?;
        context.register_model(ModelSpec::new("typed", "same", "Chat", "chat"))?;
        let mut classifier = ClassifierModelSpec::new(
            "typed",
            "same",
            format!("classifier-{}", self.build),
            "classify",
        );
        classifier.base_url = Some(format!("https://generation-{}.invalid", self.build));
        context.register_classifier_model(classifier)?;
        context.register_image_model(ImageModelSpec::new("typed", "same", "Image", "images"))?;
        context.register_image_model(ImageModelSpec::new(
            "typed",
            "only-image",
            "Image Only",
            "images",
        ))
    }

    async fn before_provider_request(
        &self,
        context: ProviderPluginContext,
        event: BeforeProviderRequestEvent,
    ) -> Result<Option<Value>, PluginError> {
        let mut payload = event.payload;
        payload["generation"] = json!(context.generation());
        payload["build"] = json!(self.build);
        Ok(Some(payload))
    }
}

fn runtime(behavior: Behavior) -> (PiRuntime, Arc<Observed>) {
    let observed = Arc::new(Observed::default());
    let builds = AtomicUsize::new(0);
    let fixture = observed.clone();
    let runtime = PiRuntime::builder()
        .agent_options(pi_agent::AgentOptions {
            provider_id: ProviderId::new("typed"),
            model_id: ModelId::new("same"),
            ..Default::default()
        })
        .provider_plugin_factory(move || FixturePlugin {
            build: builds.fetch_add(1, Ordering::SeqCst) + 1,
            provider: Arc::new(FixtureProvider {
                observed: fixture.clone(),
                behavior,
            }),
        })
        .build()
        .unwrap();
    (runtime, observed)
}

fn classifier_request() -> ClassifierContext {
    ClassifierContext {
        state: serde_json::Map::new(),
        images: Vec::new(),
        questions: [(
            "quality".into(),
            ClassifierQuestion::Score {
                instructions: "Evaluate quality".into(),
                criteria: vec!["poor".into(), "good".into()],
            },
        )]
        .into_iter()
        .collect(),
    }
}

fn image_request() -> ImagesContext {
    ImagesContext {
        input: vec![ContentBlock::Text(TextContent::new("Draw a tree"))],
    }
}

#[tokio::test]
async fn specialized_operations_resolve_canonical_models_and_generation_hooks() {
    let (runtime, observed) = runtime(Behavior::Success);
    let (_, signal) = AbortHandle::new();
    let provider = ProviderId::new("typed");
    let id = ModelId::new("same");
    let Some(AnyModelSpec::Classifier(mut catalog_copy)) =
        runtime.model_of_type(ModelType::Classifier, &provider, &id)
    else {
        panic!("missing classifier")
    };
    catalog_copy.base_url = Some("https://script-controlled.invalid".into());
    let result = runtime
        .classify(
            catalog_copy.provider,
            catalog_copy.id,
            classifier_request(),
            signal.clone(),
        )
        .await
        .unwrap();
    assert_eq!(result.stop_reason, ModelCallStopReason::Stop);
    assert_eq!(result.usage.unwrap().total_tokens, 5);
    let images = runtime
        .generate_images(provider.clone(), id, image_request(), signal)
        .await
        .unwrap();
    assert_eq!(images.stop_reason, ModelCallStopReason::Stop);
    assert!(
        matches!(&images.output[0], ContentBlock::Image(image) if image.mime_type == "image/png")
    );
    assert_eq!(runtime.models().len(), 1);
    assert_eq!(runtime.models()[0].name, "Chat");
    assert_eq!(
        runtime
            .models_of_type(ModelType::Image, Some(&provider))
            .len(),
        2
    );
    let calls = observed.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let AnyModelSpec::Classifier(model) = &calls[0].0 else {
        panic!("wrong operation")
    };
    assert_eq!(
        model.base_url.as_deref(),
        Some("https://generation-1.invalid")
    );
    for (_, payload) in &*calls {
        assert_eq!(payload["generation"], 1);
        assert_eq!(payload["build"], 1);
    }
}

#[tokio::test]
async fn invalid_wrong_type_and_unsupported_image_requests_do_not_dispatch() {
    let (runtime, observed) = runtime(Behavior::Success);
    let (_, signal) = AbortHandle::new();
    let provider = ProviderId::new("typed");
    let id = ModelId::new("same");
    let mut empty = classifier_request();
    empty.questions.clear();
    assert!(matches!(
        runtime
            .classify(provider.clone(), id.clone(), empty, signal.clone())
            .await,
        Err(RuntimeError::Input(_))
    ));
    assert!(matches!(
        runtime
            .classify(
                provider.clone(),
                ModelId::new("only-image"),
                classifier_request(),
                signal.clone()
            )
            .await,
        Err(RuntimeError::Provider(_))
    ));
    assert!(matches!(
        runtime
            .generate_images(
                provider.clone(),
                id.clone(),
                ImagesContext { input: vec![] },
                signal.clone()
            )
            .await,
        Err(RuntimeError::Input(_))
    ));
    let image = ImageContent {
        data: "cG5n".into(),
        mime_type: "image/png".into(),
    };
    let mut request = classifier_request();
    request.images.push(image.clone());
    assert_eq!(
        runtime
            .classify(provider.clone(), id.clone(), request, signal.clone())
            .await
            .unwrap()
            .stop_reason,
        ModelCallStopReason::Error
    );
    assert_eq!(
        runtime
            .generate_images(
                provider,
                id,
                ImagesContext {
                    input: vec![ContentBlock::Image(image)]
                },
                signal
            )
            .await
            .unwrap()
            .stop_reason,
        ModelCallStopReason::Error
    );
    assert!(observed.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_failures_and_cancellation_resolve_typed_results() {
    let (runtime, observed) = runtime(Behavior::Failure);
    let provider = ProviderId::new("typed");
    let id = ModelId::new("same");
    let (abort, signal) = AbortHandle::new();
    let classified = runtime
        .classify(
            provider.clone(),
            id.clone(),
            classifier_request(),
            signal.clone(),
        )
        .await
        .unwrap();
    let images = runtime
        .generate_images(
            provider.clone(),
            id.clone(),
            image_request(),
            signal.clone(),
        )
        .await
        .unwrap();
    assert_eq!(classified.stop_reason, ModelCallStopReason::Error);
    assert!(
        classified
            .error_message
            .unwrap()
            .contains("fixture failure")
    );
    assert_eq!(images.stop_reason, ModelCallStopReason::Error);
    abort.abort();
    assert_eq!(
        runtime
            .classify(
                provider.clone(),
                id.clone(),
                classifier_request(),
                signal.clone()
            )
            .await
            .unwrap()
            .stop_reason,
        ModelCallStopReason::Aborted
    );
    assert_eq!(
        runtime
            .generate_images(provider, id, image_request(), signal)
            .await
            .unwrap()
            .stop_reason,
        ModelCallStopReason::Aborted
    );
    assert_eq!(
        observed.calls.lock().unwrap().len(),
        2,
        "pre-aborted calls must not reach the provider"
    );
}

#[tokio::test]
async fn classifier_pins_its_generation_while_reload_rebuilds_the_catalog() {
    let (runtime, observed) = runtime(Behavior::Wait);
    let old_context = runtime.plugin_context_handle(PluginContextScope::Base);
    let (_, signal) = AbortHandle::new();
    let mut execution = Box::pin(runtime.classify(
        ProviderId::new("typed"),
        ModelId::new("same"),
        classifier_request(),
        signal,
    ));
    tokio::select! {
        result = &mut execution => panic!("classifier ended before release: {result:?}"),
        () = observed.started.notified() => {},
    }
    tokio::time::timeout(Duration::from_secs(1), runtime.reload())
        .await
        .expect("model operation blocked reload")
        .unwrap();
    assert!(old_context.access_for_adapter().is_ok());
    let Some(AnyModelSpec::Classifier(model)) = runtime.model_of_type(
        ModelType::Classifier,
        &ProviderId::new("typed"),
        &ModelId::new("same"),
    ) else {
        panic!("missing rebuilt classifier")
    };
    assert_eq!(model.name, "classifier-2");
    observed.release.notify_one();
    assert_eq!(
        execution.await.unwrap().stop_reason,
        ModelCallStopReason::Stop
    );
    assert!(matches!(
        old_context.access_for_adapter(),
        Err(PluginContextError::Retired)
    ));
    let calls = observed.calls.lock().unwrap();
    assert_eq!(calls[0].1["generation"], 1);
    assert_eq!(calls[0].1["build"], 1);
}

#[tokio::test]
async fn external_abort_settles_a_running_specialized_provider() {
    let (runtime, observed) = runtime(Behavior::Wait);
    let (abort, signal) = AbortHandle::new();
    let execution = runtime.generate_images(
        ProviderId::new("typed"),
        ModelId::new("same"),
        image_request(),
        signal,
    );
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(execution, async {
            observed.started.notified().await;
            abort.abort();
        })
    })
    .await
    .expect("image request ignored cancellation");
    assert_eq!(result.unwrap().stop_reason, ModelCallStopReason::Aborted);
}
