use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;

use crate::{
    AbortHandle, AbortSignal, AnyModelSpec, ClassifierModelSpec, ModelType, Provider,
    ProviderError, ProviderId, RegistriesBuilder,
};

struct LiveProvider {
    enabled: AtomicBool,
    calls: AtomicUsize,
    pending: AtomicBool,
    entered: tokio::sync::Notify,
    dropped: AtomicBool,
}

impl LiveProvider {
    fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            pending: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            dropped: AtomicBool::new(false),
        }
    }
}

struct QueryGuard<'a>(&'a AtomicBool);
impl Drop for QueryGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Provider for LiveProvider {
    fn id(&self) -> ProviderId {
        "live".into()
    }
    async fn filter_available_models(
        &self,
        mut models: Vec<AnyModelSpec>,
        _: AbortSignal,
    ) -> Result<Vec<AnyModelSpec>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending.load(Ordering::SeqCst) {
            let _guard = QueryGuard(&self.dropped);
            self.entered.notify_one();
            return std::future::pending().await;
        }
        if !self.enabled.load(Ordering::SeqCst) {
            return Ok(vec![]);
        }
        models.reverse();
        for model in &mut models {
            if let AnyModelSpec::Classifier(model) = model {
                model.name = "must not overwrite canonical metadata".into();
            }
        }
        models.push(AnyModelSpec::Classifier(ClassifierModelSpec::new(
            "live",
            "not-registered",
            "injected",
            "test",
        )));
        Ok(models)
    }
}

fn runtime(provider: Arc<LiveProvider>) -> super::ModelRuntime {
    let mut builder = RegistriesBuilder::new();
    builder
        .register_provider("fixture".into(), provider)
        .unwrap();
    for id in ["z", "a"] {
        builder
            .register_model_of_type(
                "fixture".into(),
                AnyModelSpec::Classifier(ClassifierModelSpec::new("live", id, id, "test")),
            )
            .unwrap();
    }
    builder.freeze().model_runtime().clone()
}

#[tokio::test]
async fn queries_current_state_and_retains_canonical_order_and_metadata() {
    let provider = Arc::new(LiveProvider::new());
    let runtime = runtime(provider.clone());
    assert!(
        runtime
            .query_available_models_of_type(ModelType::Classifier, None, AbortHandle::new().1)
            .await
            .unwrap()
            .is_empty()
    );
    provider.enabled.store(true, Ordering::SeqCst);
    let found = runtime
        .query_available_models_of_type(ModelType::Classifier, None, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(found, runtime.models_of_type(ModelType::Classifier, None));
    assert_eq!(
        found
            .iter()
            .map(|model| model.id().as_str())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    // Auth is provider-scoped, even when the requested type has no entries.
    assert!(
        runtime
            .query_available_models_of_type(ModelType::Image, None, AbortHandle::new().1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(
        runtime
            .query_available_models_of_type(
                ModelType::Image,
                Some(&"absent".into()),
                AbortHandle::new().1
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cancelled_query_drops_pending_checks_and_next_query_still_works() {
    let provider = Arc::new(LiveProvider::new());
    provider.pending.store(true, Ordering::SeqCst);
    let runtime = runtime(provider.clone());
    let (handle, signal) = AbortHandle::new();
    let query = runtime.query_available_models_of_type(ModelType::Classifier, None, signal);
    let abort = async {
        provider.entered.notified().await;
        handle.abort();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(query, abort)
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(ProviderError::Aborted)));
    assert!(provider.dropped.load(Ordering::SeqCst));
    provider.pending.store(false, Ordering::SeqCst);
    provider.enabled.store(true, Ordering::SeqCst);
    assert_eq!(
        runtime
            .query_available_models_of_type(ModelType::Classifier, None, AbortHandle::new().1)
            .await
            .unwrap()
            .len(),
        2
    );
    let (handle, signal) = AbortHandle::new();
    handle.abort();
    assert!(matches!(
        runtime
            .query_available_models_of_type(ModelType::Classifier, None, signal)
            .await,
        Err(ProviderError::Aborted)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
