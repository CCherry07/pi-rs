use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::AgentPlugin;
use pi_session::SessionPlugin;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

use crate::MemoryRecallOptions;

/// Host-owned context supplied once while preparing a memory generation.
#[derive(Debug, Clone)]
pub struct MemoryProviderInitializeContext {
    cwd: PathBuf,
    agent_dir: PathBuf,
    session_roots: Arc<[PathBuf]>,
    recall_options: MemoryRecallOptions,
}

impl MemoryProviderInitializeContext {
    pub(crate) fn new(
        cwd: PathBuf,
        agent_dir: PathBuf,
        session_roots: Vec<PathBuf>,
        recall_options: MemoryRecallOptions,
    ) -> Self {
        Self {
            cwd,
            agent_dir,
            session_roots: Arc::from(session_roots),
            recall_options,
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    pub fn session_roots(&self) -> &[PathBuf] {
        &self.session_roots
    }

    pub fn recall_options(&self) -> MemoryRecallOptions {
        self.recall_options
    }
}

/// The selected provider's opaque configuration from `memory.json`.
///
/// The host does not require this value to be an object and never exposes
/// sibling provider configurations. Its schema belongs entirely to the
/// provider implementation.
#[derive(Debug, Clone)]
pub struct MemoryProviderConfig {
    pub provider_id: String,
    pub source_path: PathBuf,
    pub raw: Option<Value>,
}

impl MemoryProviderConfig {
    pub(crate) fn new(provider_id: String, source_path: PathBuf, raw: Option<Value>) -> Self {
        Self {
            provider_id,
            source_path,
            raw,
        }
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn raw(&self) -> Option<&Value> {
        self.raw.as_ref()
    }

    /// Convenience for providers whose missing subtree means default policy.
    pub fn deserialize_or_default<T>(&self) -> Result<T, MemoryProviderInitializeError>
    where
        T: DeserializeOwned + Default,
    {
        self.raw.as_ref().map_or_else(
            || Ok(T::default()),
            |raw| {
                serde_json::from_value(raw.clone()).map_err(|source| {
                    Box::new(MemoryProviderConfigDecodeError {
                        provider: self.provider_id.clone(),
                        path: self.source_path.clone(),
                        source,
                    }) as MemoryProviderInitializeError
                })
            },
        )
    }
}

#[derive(Debug, Error)]
#[error("invalid configuration for memory provider {provider} in {}: {source}", path.display())]
struct MemoryProviderConfigDecodeError {
    provider: String,
    path: PathBuf,
    #[source]
    source: serde_json::Error,
}

/// Provider-owned initialization failure retained as the Loader's source.
pub type MemoryProviderInitializeError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// One initialized provider that participates directly in Pi's existing
/// Agent and Session plugin systems.
///
/// This marker Interface adds only the provider identity used by
/// `memory.json`. Lifecycle behavior comes from the ordinary `AgentPlugin` and
/// `SessionPlugin` Interfaces; it is not mirrored here.
pub trait MemoryProviderPlugin: AgentPlugin + SessionPlugin {
    fn memory_provider_id(&self) -> &str;
}

/// Construction Seam implemented by each memory provider package.
#[async_trait]
pub trait MemoryProviderFactory: Send + Sync {
    fn id(&self) -> &str;

    async fn initialize(
        &self,
        context: &MemoryProviderInitializeContext,
        config: &MemoryProviderConfig,
    ) -> Result<Arc<dyn MemoryProviderPlugin>, MemoryProviderInitializeError>;
}

/// Fully initialized provider retained by the candidate generation.
#[derive(Clone)]
pub struct PreparedMemoryProvider {
    provider: Arc<dyn MemoryProviderPlugin>,
}

impl PreparedMemoryProvider {
    pub(crate) fn new(provider: Arc<dyn MemoryProviderPlugin>) -> Self {
        Self { provider }
    }

    pub fn provider_id(&self) -> &str {
        self.provider.memory_provider_id()
    }

    pub fn agent_plugin(&self) -> Arc<dyn AgentPlugin> {
        self.provider.clone()
    }

    pub fn session_plugin(&self) -> Arc<dyn SessionPlugin> {
        self.provider.clone()
    }
}

impl std::fmt::Debug for PreparedMemoryProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedMemoryProvider")
            .field("provider", &self.provider.memory_provider_id())
            .finish()
    }
}
