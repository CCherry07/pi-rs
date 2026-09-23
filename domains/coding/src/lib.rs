//! Coding product composition for the shared Pi runtime and session framework.

mod builtin_providers;
mod composition;
mod configuration;
mod credentials;
mod dynamic_providers;
mod features;
mod generation;
mod host;
pub mod plugins;
mod project_trust;
mod prompt;
mod resources;
mod runtime_inventory;
pub mod skills;
#[cfg(test)]
mod test_support;

pub use credentials::{StoredCredential, read_credentials, read_stored_credential};
pub use features::Features;
pub use generation::ProductSessionFactory;
pub use host::{Pi, PiBuilder};
pub use pi_plugin_memory_hermes::curator;
pub use project_trust::{
    ProjectTrustError, ProjectTrustEvaluation, ProjectTrustOption, ProjectTrustPromptRequest,
    ProjectTrustService,
};

mod config;
pub(crate) use config::expand_tilde_path;
pub use config::{Config, default_agent_dir};

pub use pi_sdk::projects;
mod workspace_prompt;

mod session;
pub use session::{CodingCompactionPolicy, CodingShellExecutor};
