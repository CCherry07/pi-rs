//! Coding product composition for the shared Pi runtime and session framework.

mod auth_check;
mod builtin_providers;
mod composition;
mod configuration;
mod credentials;
mod dynamic_mcp;
mod dynamic_providers;
mod features;
mod generation;
mod mcp;
mod pi;
pub mod plugins;
mod project_trust;
mod prompt;
mod resources;
mod runtime_inventory;
pub mod skills;
#[cfg(test)]
mod test_support;
mod tool_result_images;

pub use auth_check::{AuthCheckOptions, AuthCheckResult, AuthCheckStatus, check_auth};
pub use credentials::{
    StoredCredential, modify_credentials, read_credentials, read_stored_credential,
};
pub use features::Features;
pub use generation::ProductSessionFactory;
pub use mcp::mcp_client_auth;
pub use pi::{Pi, PiBuilder};
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

pub use pi_plugin_gateways::{gateway_definitions, kimi_oauth};
