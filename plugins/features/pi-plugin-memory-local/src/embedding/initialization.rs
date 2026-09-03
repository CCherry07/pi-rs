use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::OnceCell;

use super::fastembed::{FastEmbedInstallReceipt, FastEmbedModelStatus, FastEmbedModelStore};
use crate::MemoryError;
use crate::storage::{LocalMemoryProvider, MemoryEmbeddingBackfillReceipt};

const INITIALIZATION_BACKFILL_BATCH_SIZE: usize = 128;

/// Provider-owned dependency policy used during generation initialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LocalMemoryProviderInitializationMode {
    /// Use verified local assets when present, without accessing the network.
    #[default]
    Offline,
    /// Acquire missing pinned assets before publishing the provider.
    Automatic,
}

/// Provider-specific configuration embedded under `providers.local` in
/// `memory.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(crate) struct LocalMemoryProviderConfig {
    /// Controls whether initialization may acquire missing model assets.
    pub initialization: LocalMemoryProviderInitializationMode,
}

/// Factory-backed initializer for the local SQLite memory Adapter.
///
/// Database creation, model acquisition, model loading, vector-index setup,
/// and initial backfill all stay inside the provider crate. A successful
/// initialization is shared by every caller in the generation.
#[derive(Clone)]
pub(crate) struct LocalMemoryProviderInitializer {
    database_path: PathBuf,
    model_store: FastEmbedModelStore,
    mode: LocalMemoryProviderInitializationMode,
    initialized: Arc<OnceCell<Arc<LocalMemoryProvider>>>,
    initialization_issue: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for LocalMemoryProviderInitializer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalMemoryProviderInitializer")
            .field("database_path", &self.database_path)
            .field("model", &self.model_store.status())
            .field("mode", &self.mode)
            .field("initialized", &self.initialized.get().is_some())
            .finish()
    }
}

impl LocalMemoryProviderInitializer {
    pub(crate) fn new(
        database_path: impl Into<PathBuf>,
        embedding_cache_dir: impl Into<PathBuf>,
        mode: LocalMemoryProviderInitializationMode,
    ) -> Self {
        Self {
            database_path: database_path.into(),
            model_store: FastEmbedModelStore::new(embedding_cache_dir),
            mode,
            initialized: Arc::new(OnceCell::new()),
            initialization_issue: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn model_status(&self) -> FastEmbedModelStatus {
        self.model_store.status()
    }

    #[cfg(test)]
    fn initialized_provider(&self) -> Option<Arc<LocalMemoryProvider>> {
        self.initialized.get().cloned()
    }

    /// Initializes this provider once and returns the generation-ready handle.
    pub(crate) async fn initialize(&self) -> Result<Arc<LocalMemoryProvider>, MemoryError> {
        Ok(self
            .initialized
            .get_or_try_init(|| self.initialize_once())
            .await?
            .clone())
    }

    pub(crate) fn dense_active(&self) -> bool {
        self.initialized
            .get()
            .is_some_and(|provider| provider.dense_active())
    }

    pub(crate) fn initialization_issue(&self) -> Option<String> {
        self.initialization_issue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Explicitly acquire and prepare the pinned embedding model without
    /// replacing the provider already published in the current generation.
    /// The caller can backfill this prepared Adapter and then reload.
    pub(crate) async fn prepare_embedding_model(
        &self,
    ) -> Result<PreparedLocalMemoryEmbedding, MemoryError> {
        let install = self
            .model_store
            .install()
            .await
            .map_err(|error| self.initialization_error(error.to_string()))?;
        let embedder = self
            .model_store
            .embedder_if_ready()
            .map_err(|error| self.initialization_error(error.to_string()))?
            .ok_or_else(|| {
                self.initialization_error(
                    "embedding installation completed without ready assets".to_string(),
                )
            })?;
        let provider = LocalMemoryProvider::open_with_embedder(&self.database_path, embedder)?;
        Ok(PreparedLocalMemoryEmbedding {
            install,
            provider: Arc::new(provider),
        })
    }

    async fn initialize_once(&self) -> Result<Arc<LocalMemoryProvider>, MemoryError> {
        if self.mode == LocalMemoryProviderInitializationMode::Automatic {
            self.model_store
                .install()
                .await
                .map_err(|error| self.initialization_error(error.to_string()))?;
        }

        let embedder = match self.model_store.embedder_if_ready() {
            Ok(embedder) => embedder,
            Err(error) if self.mode == LocalMemoryProviderInitializationMode::Offline => {
                self.set_initialization_issue(Some(error.to_string()));
                None
            }
            Err(error) => return Err(self.initialization_error(error.to_string())),
        };

        let provider = match embedder {
            Some(embedder) => {
                match LocalMemoryProvider::open_with_embedder(&self.database_path, embedder) {
                    Ok(provider) => provider,
                    Err(error) if self.mode == LocalMemoryProviderInitializationMode::Offline => {
                        self.set_initialization_issue(Some(error.to_string()));
                        LocalMemoryProvider::open(&self.database_path)?
                    }
                    Err(error) => return Err(error),
                }
            }
            None => LocalMemoryProvider::open(&self.database_path)?,
        };
        let provider = Arc::new(provider);

        if provider.dense_active() {
            loop {
                let receipt = provider
                    .backfill_embeddings(INITIALIZATION_BACKFILL_BATCH_SIZE)
                    .await?;
                if receipt.remaining == 0 || receipt.indexed == 0 {
                    break;
                }
            }
            self.set_initialization_issue(None);
        }

        Ok(provider)
    }

    fn initialization_error(&self, message: String) -> MemoryError {
        MemoryError::Initialize {
            path: self.database_path.display().to_string(),
            message,
        }
    }

    fn set_initialization_issue(&self, issue: Option<String>) {
        *self
            .initialization_issue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = issue;
    }
}

pub(crate) struct PreparedLocalMemoryEmbedding {
    install: FastEmbedInstallReceipt,
    provider: Arc<LocalMemoryProvider>,
}

impl PreparedLocalMemoryEmbedding {
    pub(crate) fn install_receipt(&self) -> &FastEmbedInstallReceipt {
        &self.install
    }

    pub(crate) async fn backfill_embeddings(
        &self,
        limit: usize,
    ) -> Result<MemoryEmbeddingBackfillReceipt, MemoryError> {
        self.provider.backfill_embeddings(limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_initialization_is_idempotent_and_creates_the_database() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.sqlite3");
        let initializer = LocalMemoryProviderInitializer::new(
            &database,
            directory.path().join("models"),
            LocalMemoryProviderInitializationMode::Offline,
        );

        initializer.initialize().await.unwrap();
        let first = initializer.initialized_provider().unwrap();
        initializer.initialize().await.unwrap();
        let second = initializer.initialized_provider().unwrap();

        assert!(database.is_file());
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!initializer.dense_active());
    }
}
