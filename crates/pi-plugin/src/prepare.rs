use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginScope {
    Global,
    Project { root: PathBuf },
    ExplicitPath,
}

#[derive(Debug, Clone)]
pub struct PrepareContext {
    cwd: PathBuf,
    package_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    scope: PluginScope,
    generation: u64,
}

impl PrepareContext {
    pub fn new(
        cwd: impl Into<PathBuf>,
        package_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        scope: PluginScope,
        generation: u64,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            package_dir: package_dir.into(),
            data_dir: data_dir.into(),
            cache_dir: cache_dir.into(),
            scope,
            generation,
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn package_dir(&self) -> &Path {
        &self.package_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn scope(&self) -> &PluginScope {
        &self.scope
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[doc(hidden)]
    pub fn for_generation(&self, generation: u64) -> Self {
        let mut context = self.clone();
        context.generation = generation;
        context
    }
}

pub trait PluginFactory: Sized + Send + Sync + 'static {
    type Options: DeserializeOwned + Send + Sync + 'static;

    /// Reads plugin-owned configuration, validates effective options and constructs an unpublished instance.
    /// Return None to disable this plugin for the candidate generation.
    fn prepare(
        context: &PrepareContext,
        options: Self::Options,
    ) -> Result<Option<Self>, PrepareError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("invalid plugin options: {0}")]
    InvalidOptions(String),
    #[error("plugin initialization failed: {0}")]
    Initialization(String),
}

pub type PrepareResult<T> = Result<T, PrepareError>;

impl PrepareError {
    pub fn initialization(message: impl Into<String>) -> Self {
        Self::Initialization(message.into())
    }
}
