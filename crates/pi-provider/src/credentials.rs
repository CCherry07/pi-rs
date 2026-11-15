//! Request-time credential resolution without vendor or storage policy.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortSignal, ProviderId};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderRequest,
    ProviderStream,
};

#[derive(Clone, PartialEq, Eq)]
pub enum CredentialKind {
    ApiKey,
    OAuth,
    Bearer,
}

/// Secret-bearing request data. Deliberately does not implement Debug.
#[derive(Clone, PartialEq)]
pub struct ProviderCredential {
    pub secret: String,
    pub kind: CredentialKind,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[async_trait]
pub trait CredentialSource: Send + Sync {
    async fn resolve(
        &self,
        signal: &AbortSignal,
    ) -> Result<Option<ProviderCredential>, ProviderError>;
}

type ProviderFactory =
    dyn Fn(Option<&ProviderCredential>) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync;
type CachedProvider = Option<(Option<ProviderCredential>, Arc<dyn Provider>)>;

/// Keeps connection state while credentials are unchanged. A rotated credential
/// replaces only this adapter's transport state, never the published registries.
pub struct CredentialProvider {
    initial: Arc<dyn Provider>,
    source: Arc<dyn CredentialSource>,
    factory: Arc<ProviderFactory>,
    cached: Mutex<CachedProvider>,
}

impl CredentialProvider {
    pub fn new(
        initial: Arc<dyn Provider>,
        source: Arc<dyn CredentialSource>,
        factory: impl Fn(Option<&ProviderCredential>) -> Result<Arc<dyn Provider>, ProviderError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            initial,
            source,
            factory: Arc::new(factory),
            cached: Mutex::new(None),
        }
    }
}

#[async_trait]
impl Provider for CredentialProvider {
    fn id(&self) -> ProviderId {
        self.initial.id()
    }
    fn name(&self) -> String {
        self.initial.name()
    }
    fn availability(&self) -> ProviderAvailability {
        self.initial.availability()
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let credential = self.source.resolve(&signal).await?;
        let provider = {
            let mut cached = self
                .cached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((previous, provider)) = &*cached {
                if previous == &credential {
                    Some(Arc::clone(provider))
                } else {
                    None
                }
            } else {
                None
            }
            .map_or_else(
                || {
                    let provider = (self.factory)(credential.as_ref())?;
                    *cached = Some((credential, Arc::clone(&provider)));
                    Ok::<_, ProviderError>(provider)
                },
                Ok,
            )?
        };
        provider.stream(request, context, signal).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{AbortHandle, ModelId, ThinkingLevel};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Source(Arc<Mutex<Option<ProviderCredential>>>);
    #[async_trait]
    impl CredentialSource for Source {
        async fn resolve(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<ProviderCredential>, ProviderError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }
    type Observations = Arc<Mutex<Vec<(String, usize)>>>;
    struct Connection {
        key: String,
        requests: AtomicUsize,
        observations: Observations,
    }
    #[async_trait]
    impl Provider for Connection {
        fn id(&self) -> ProviderId {
            "test".into()
        }
        async fn stream(
            &self,
            _: ProviderRequest,
            _: ProviderCallContext,
            _: AbortSignal,
        ) -> Result<ProviderStream, ProviderError> {
            self.observations.lock().unwrap().push((
                self.key.clone(),
                self.requests.fetch_add(1, Ordering::SeqCst) + 1,
            ));
            Ok(Box::pin(futures::stream::empty()))
        }
    }
    #[tokio::test]
    async fn preserves_connection_state_until_credentials_rotate() {
        let credential = |key: &str| {
            Some(ProviderCredential {
                secret: key.into(),
                kind: CredentialKind::OAuth,
                metadata: BTreeMap::new(),
            })
        };
        let source = Arc::new(Mutex::new(credential("old")));
        let observations: Observations = Arc::default();
        let output = observations.clone();
        let initial = Arc::new(Connection {
            key: "initial".into(),
            requests: AtomicUsize::new(0),
            observations: observations.clone(),
        });
        let provider = CredentialProvider::new(
            initial,
            Arc::new(Source(source.clone())),
            move |credential| {
                Ok(Arc::new(Connection {
                    key: credential.unwrap().secret.clone(),
                    requests: AtomicUsize::new(0),
                    observations: output.clone(),
                }))
            },
        );
        for key in ["old", "old", "new"] {
            *source.lock().unwrap() = credential(key);
            let (_, signal) = AbortHandle::new();
            let request = ProviderRequest {
                model: ModelId::new("model"),
                model_spec: None,
                system_prompt: String::new(),
                messages: vec![],
                tools: vec![],
                thinking_level: ThinkingLevel::Off,
                thinking_budgets: None,
                max_output_tokens: None,
                headers: BTreeMap::new(),
                sampling_params: BTreeMap::new(),
                session_id: Some("session".into()),
            };
            let _ = provider
                .stream(
                    request,
                    ProviderCallContext::without_plugins("/", "test".into(), "model".into()),
                    signal,
                )
                .await
                .unwrap();
        }
        assert_eq!(
            *observations.lock().unwrap(),
            vec![("old".into(), 1), ("old".into(), 2), ("new".into(), 1)]
        );
    }
}
