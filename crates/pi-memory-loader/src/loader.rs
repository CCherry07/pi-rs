use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use crate::config::read_document;
use crate::{
    MemoryConfigError, MemoryLoaderOptions, MemoryProviderConfig, MemoryProviderFactory,
    MemoryProviderInitializeContext, MemoryProviderInitializeError, PreparedMemoryProvider,
};

#[derive(Debug, Error)]
pub enum MemoryLoadError {
    #[error(transparent)]
    Config(#[from] MemoryConfigError),
    #[error("duplicate memory provider factory: {0}")]
    DuplicateProvider(String),
    #[error("memory.json selects unknown provider {provider}; registered providers: {registered}")]
    UnknownProvider {
        provider: String,
        registered: String,
    },
    #[error("memory provider {provider} failed to initialize: {source}")]
    Initialize {
        provider: String,
        #[source]
        source: MemoryProviderInitializeError,
    },
    #[error("memory provider factory {factory} returned provider {actual}")]
    IdentityMismatch { factory: String, actual: String },
}

/// Loads and initializes the selected memory provider for a candidate
/// generation. This is a construction Loader, not a runtime lifecycle Driver.
pub struct MemoryLoader {
    options: MemoryLoaderOptions,
    factories: Vec<Arc<dyn MemoryProviderFactory>>,
}

impl MemoryLoader {
    pub fn new(options: MemoryLoaderOptions) -> Self {
        Self {
            options,
            factories: Vec::new(),
        }
    }

    pub fn provider_factory(mut self, factory: impl MemoryProviderFactory + 'static) -> Self {
        self.factories.push(Arc::new(factory));
        self
    }

    pub fn provider_factory_arc(mut self, factory: Arc<dyn MemoryProviderFactory>) -> Self {
        self.factories.push(factory);
        self
    }

    pub async fn load(self) -> Result<Option<PreparedMemoryProvider>, MemoryLoadError> {
        let (source_path, document) = read_document(&self.options.agent_dir)?;
        if !document.enabled() {
            return Ok(None);
        }

        let mut factories = BTreeMap::new();
        for factory in self.factories {
            let id = factory.id().to_string();
            if factories.insert(id.clone(), factory).is_some() {
                return Err(MemoryLoadError::DuplicateProvider(id));
            }
        }

        let selected = document.provider().to_string();
        let factory = factories
            .get(&selected)
            .ok_or_else(|| MemoryLoadError::UnknownProvider {
                provider: selected.clone(),
                registered: factories.keys().cloned().collect::<Vec<_>>().join(", "),
            })?;
        let recall_options = document.recall_options(self.options.recall_options);
        let context = MemoryProviderInitializeContext::new(
            self.options.cwd,
            self.options.agent_dir,
            self.options.session_roots,
            recall_options,
            self.options.project_trusted,
        );
        let config =
            MemoryProviderConfig::new(selected.clone(), source_path, document.selected_config());
        let provider = factory
            .initialize(&context, &config)
            .await
            .map_err(|source| MemoryLoadError::Initialize {
                provider: selected.clone(),
                source,
            })?;
        if provider.memory_provider_id() != selected {
            return Err(MemoryLoadError::IdentityMismatch {
                factory: selected,
                actual: provider.memory_provider_id().to_string(),
            });
        }
        Ok(Some(PreparedMemoryProvider::new(provider)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use pi_core::{AgentPlugin, PluginId};
    use pi_session::SessionPlugin;
    use serde_json::{Value, json};

    use super::*;
    use crate::{MemoryProviderInitializeError, MemoryProviderPlugin, MemoryRecallOptions};

    struct FakeProvider {
        id: String,
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for FakeProvider {
        fn id(&self) -> PluginId {
            PluginId::new("memory")
        }
    }

    #[async_trait]
    impl SessionPlugin for FakeProvider {
        fn id(&self) -> PluginId {
            PluginId::new("memory")
        }
    }

    impl MemoryProviderPlugin for FakeProvider {
        fn memory_provider_id(&self) -> &str {
            &self.id
        }
    }

    struct FakeFactory {
        id: &'static str,
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<Option<Value>>>,
        seen_project_trust: Arc<Mutex<Option<bool>>>,
    }

    type FactoryFixture = (
        FakeFactory,
        Arc<AtomicUsize>,
        Arc<Mutex<Option<Value>>>,
        Arc<Mutex<Option<bool>>>,
    );

    #[async_trait]
    impl MemoryProviderFactory for FakeFactory {
        fn id(&self) -> &str {
            self.id
        }

        async fn initialize(
            &self,
            context: &MemoryProviderInitializeContext,
            config: &MemoryProviderConfig,
        ) -> Result<Arc<dyn MemoryProviderPlugin>, MemoryProviderInitializeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.seen.lock().unwrap() = config.raw().cloned();
            *self.seen_project_trust.lock().unwrap() = Some(context.project_trusted());
            Ok(Arc::new(FakeProvider {
                id: self.id.to_string(),
            }))
        }
    }

    fn factory(id: &'static str) -> FactoryFixture {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(None));
        let seen_project_trust = Arc::new(Mutex::new(None));
        (
            FakeFactory {
                id,
                calls: Arc::clone(&calls),
                seen: Arc::clone(&seen),
                seen_project_trust: Arc::clone(&seen_project_trust),
            },
            calls,
            seen,
            seen_project_trust,
        )
    }

    #[tokio::test]
    async fn initializes_only_the_selected_factory_with_only_its_subtree() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("memory.json"),
            r#"{
                "version": 1,
                "provider": "remote",
                "providers": {
                    "local": {"secret": "must-not-leak"},
                    "remote": {"endpoint": "https://memory.example"}
                }
            }"#,
        )
        .unwrap();
        let (local, local_calls, _, _) = factory("local");
        let (remote, remote_calls, remote_seen, remote_trust) = factory("remote");

        let mut options = MemoryLoaderOptions::new(directory.path(), directory.path());
        options.project_trusted = true;
        let prepared = MemoryLoader::new(options)
            .provider_factory(local)
            .provider_factory(remote)
            .load()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(prepared.provider_id(), "remote");
        assert_eq!(local_calls.load(Ordering::SeqCst), 0);
        assert_eq!(remote_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *remote_seen.lock().unwrap(),
            Some(json!({"endpoint": "https://memory.example"}))
        );
        assert_eq!(*remote_trust.lock().unwrap(), Some(true));
    }

    #[tokio::test]
    async fn missing_config_uses_the_default_provider_and_disabled_config_initializes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let (hermes, calls, seen, _) = factory("hermes");
        let options = MemoryLoaderOptions::new(directory.path(), directory.path());

        let prepared = MemoryLoader::new(options.clone())
            .provider_factory(hermes)
            .load()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(prepared.provider_id(), "hermes");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*seen.lock().unwrap(), None);

        std::fs::write(
            directory.path().join("memory.json"),
            r#"{"version": 1, "enabled": false, "provider": "missing"}"#,
        )
        .unwrap();
        assert!(MemoryLoader::new(options).load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_and_unknown_provider_ids_fail_before_initialization() {
        let directory = tempfile::tempdir().unwrap();
        let options = MemoryLoaderOptions::new(directory.path(), directory.path());
        let (first, calls, _, _) = factory("local");
        let (second, _, _, _) = factory("local");
        let error = MemoryLoader::new(options.clone())
            .provider_factory(first)
            .provider_factory(second)
            .load()
            .await
            .unwrap_err();
        assert!(matches!(error, MemoryLoadError::DuplicateProvider(id) if id == "local"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        std::fs::write(
            directory.path().join("memory.json"),
            r#"{"version": 1, "provider": "remote"}"#,
        )
        .unwrap();
        let error = MemoryLoader::new(options).load().await.unwrap_err();
        assert!(
            matches!(error, MemoryLoadError::UnknownProvider { provider, .. } if provider == "remote")
        );
    }

    #[test]
    fn recall_defaults_remain_stable() {
        assert_eq!(MemoryRecallOptions::default().max_records, 8);
    }
}
