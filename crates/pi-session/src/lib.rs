#![forbid(unsafe_code)]

//! Pi v4 session tree, mutation journal, JSONL storage, and runtime adapter.
//!
//! The durable core mirrors `legacy/pi/packages/agent/src/harness/session`:
//! entries, lane records, lane pointers, and global facts share one sequence;
//! context is projected from a selected branch; and a JSONL tail is repaired
//! only when the final append is syntactically torn.

extern crate self as pi_session;

#[doc(hidden)]
pub use async_trait::async_trait as __plugin_async_trait;
pub use pi_plugin_macros::session_plugin;

mod agent_session;
mod agent_session_runtime;
mod compaction;
mod context;
mod event;
mod isolated_session;
mod jsonl;
mod legacy_import;
mod memory;
mod model_runtime_services;
mod multi_session_manager;
pub mod plugin;
mod plugin_context;
mod reducer;
mod repo;
mod session;
mod state;
pub mod types;
mod usage;

pub use agent_session::{
    AgentSession, AgentSessionOptions, AutoRetrySettings, PROMPT_SNAPSHOT_CUSTOM_TYPE,
    PreparedAgentSession, PromptSnapshot, RESOURCE_DIAGNOSTIC_CUSTOM_TYPE, ResourceSnapshot,
    SessionRuntimeInventory, ShellExecutionOptions, SubmitOutcome, read_prompt_snapshot,
};
pub use agent_session_runtime::{
    AgentSessionInitialModelSource, AgentSessionInitialState, AgentSessionReplacement,
    AgentSessionRuntime, AgentSessionRuntimeError, AgentSessionRuntimeFactory,
    AgentSessionRuntimeRequest, AgentSessionRuntimeTarget, SessionGenerationOverlay,
};
pub use compaction::*;
pub use context::{
    ContextEntryTransform, CustomEntryContextMessageProjector, SessionContext,
    SessionContextBuildOptions, SessionModel, agent_message_to_provider_message,
    agent_message_to_runtime_message, build_context_entries, build_session_context,
    default_context_entry_transform, session_entry_to_context_messages,
};
pub use event::{
    AgentSessionEvent, AgentSessionSnapshot, AgentSessionSubscription, AutoRetrySnapshot,
    BashExecutionSnapshot, CompactionSnapshot, QueueSnapshot, RevisionedAgentSessionEvent,
};
pub use jsonl::SessionLog;
pub use legacy_import::{
    LegacySessionImportReport, SessionFileFormat, import_session_file, inspect_session_file,
};
pub use memory::{InMemorySession, InMemorySessionRepo};
pub use model_runtime_services::{
    InitialModelRequest, InitialModelResolveError, InitialModelResolver, InitialModelSelection,
    InitialModelSource, ModelRuntimeServices, resolve_model_scope,
};
pub use multi_session_manager::{
    MultiSessionManager, MultiSessionManagerError, PiSession, WeakPiSession,
};
pub use pi_core::{ForkPosition, NoticeLevel};
pub use plugin::{
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCompactEvent,
    SessionCompactFailedEvent, SessionForkPosition, SessionHook, SessionIdentity,
    SessionInfoChangedEvent, SessionPlugin, SessionPluginContext, SessionPluginDiagnostic,
    SessionPluginDriver, SessionPluginError, SessionPluginReloadReport, SessionPlugins,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason,
    SessionSwitchReason, SessionTreeEvent, SessionTreeSummary, TreePreparation,
};
pub use plugin_context::{
    PiPluginContext, PluginContextBinding, PluginProviderMutation, PluginProviderMutationAccess,
    PluginUiBridge,
};
pub use reducer::*;
pub use repo::{JsonlSessionRepo, list_jsonl_session_metadata, load_jsonl_session};
pub use session::{DefaultIdGenerator, IdGenerator, Session, SessionStorage, SessionView};
pub use types::*;
pub use usage::{aggregate_session_usage, session_entry_usage};

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

pub(crate) fn next_unique_id(_kind: &str) -> String {
    uuid::Uuid::now_v7().to_string()
}
