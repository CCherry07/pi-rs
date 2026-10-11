#![forbid(unsafe_code)]

//! Domain-neutral agent composition over the shared session and generation lifecycle.
//!
//! Applications supply their model, system prompt, and plugin factories explicitly. Creating this
//! harness performs no credential, settings, project instruction, skill, or package discovery.
//! The first-party Coding product is available from the separate `pi-coding` crate.

mod harness;
pub mod projects;

pub use harness::{AgentHarness, AgentHarnessBuilder};
pub use pi_core::{ModelSelection, ThinkingLevel, WorkspaceSnapshot, WorkspaceSpec};
pub use pi_runtime::{
    PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt, SystemPromptFactory,
    SystemPromptRenderer,
};
pub use pi_session::{
    AgentSessionOptions, MultiSessionManager, MultiSessionManagerError, PiSession, SessionInput,
};
