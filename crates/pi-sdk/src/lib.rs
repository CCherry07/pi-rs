#![forbid(unsafe_code)]

//! Domain-neutral agent composition over the shared session and generation lifecycle.
//!
//! Hosts supply their model, system prompt, and plugin factories explicitly. Creating this
//! host performs no credential, settings, project instruction, skill, or package discovery.
//! The first-party Coding product is available from the separate `pi-coding` crate.

mod host;
pub mod projects;

pub use host::{AgentHost, AgentHostBuilder, AgentSessionFactory};
pub use pi_core::{ModelSelection, ThinkingLevel, WorkspaceSnapshot, WorkspaceSpec};
pub use pi_runtime::{
    PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt, SystemPromptFactory,
    SystemPromptRenderer,
};
pub use pi_session::{
    AgentSessionOptions, MultiSessionManager, MultiSessionManagerError, PiSession, SessionInput,
};
