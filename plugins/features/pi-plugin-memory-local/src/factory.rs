use std::sync::Arc;

use async_trait::async_trait;
use pi_memory_loader::{
    MemoryProviderConfig, MemoryProviderFactory, MemoryProviderInitializeContext,
    MemoryProviderInitializeError, MemoryProviderPlugin,
};

use crate::LOCAL_MEMORY_PROVIDER_ID;
use crate::embedding::initialization::{LocalMemoryProviderConfig, LocalMemoryProviderInitializer};
use crate::plugin::LocalMemoryPlugin;
use crate::runtime::LocalMemoryRuntime;

/// Factory for the bundled SQLite/FTS5/sqlite-vec/FastEmbed memory provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalMemoryProviderFactory;

#[async_trait]
impl MemoryProviderFactory for LocalMemoryProviderFactory {
    fn id(&self) -> &str {
        LOCAL_MEMORY_PROVIDER_ID
    }

    async fn initialize(
        &self,
        context: &MemoryProviderInitializeContext,
        config: &MemoryProviderConfig,
    ) -> Result<Arc<dyn MemoryProviderPlugin>, MemoryProviderInitializeError> {
        let config = config.deserialize_or_default::<LocalMemoryProviderConfig>()?;
        let initializer = LocalMemoryProviderInitializer::new(
            context.agent_dir().join("memory").join("memory.sqlite3"),
            context.agent_dir().join("models").join("embeddings"),
            config.initialization,
        );
        let provider = initializer.initialize().await?;
        let runtime = LocalMemoryRuntime::new(
            provider,
            context.cwd(),
            context.recall_options(),
            initializer,
            context.session_roots().to_vec(),
        );
        Ok(Arc::new(LocalMemoryPlugin::new(runtime)))
    }
}

#[cfg(test)]
mod tests {
    use pi_memory_loader::{MemoryLoader, MemoryLoaderOptions};

    use super::*;
    use crate::LOCAL_MEMORY_PLUGIN_ID;

    #[tokio::test]
    async fn initializes_the_local_provider_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("memory.json"),
            r#"{"version": 1, "provider": "local"}"#,
        )
        .unwrap();

        let prepared = MemoryLoader::new(MemoryLoaderOptions::new(directory.path(), &agent_dir))
            .provider_factory(LocalMemoryProviderFactory)
            .load()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(prepared.provider_id(), LOCAL_MEMORY_PROVIDER_ID);
        assert_eq!(
            prepared.agent_plugin().id().as_str(),
            LOCAL_MEMORY_PLUGIN_ID
        );
        assert_eq!(
            prepared.session_plugin().id().as_str(),
            LOCAL_MEMORY_PLUGIN_ID
        );
        assert!(agent_dir.join("memory").join("memory.sqlite3").is_file());
    }

    #[tokio::test]
    async fn owns_validation_of_its_opaque_configuration() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("memory.json"),
            r#"{
                "version": 1,
                "provider": "local",
                "providers": {"local": {"unknownLocalOption": true}}
            }"#,
        )
        .unwrap();

        let error = MemoryLoader::new(MemoryLoaderOptions::new(directory.path(), directory.path()))
            .provider_factory(LocalMemoryProviderFactory)
            .load()
            .await
            .unwrap_err();

        assert!(error.to_string().contains("unknownLocalOption"));
        assert!(error.to_string().contains("local"));
    }
}
