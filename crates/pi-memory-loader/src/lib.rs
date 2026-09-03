#![forbid(unsafe_code)]

//! Host-side semantic-memory provider configuration and construction.

mod config;
mod loader;
mod options;
mod provider;

pub use config::MemoryConfigError;
pub use loader::{MemoryLoadError, MemoryLoader};
pub use options::{MemoryLoaderOptions, MemoryRecallOptions};
pub use provider::{
    MemoryProviderConfig, MemoryProviderFactory, MemoryProviderInitializeContext,
    MemoryProviderInitializeError, MemoryProviderPlugin, PreparedMemoryProvider,
};
