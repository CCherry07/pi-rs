#![forbid(unsafe_code)]

//! Pi v4 session tree, mutation journal, JSONL storage, and runtime adapter.
//!
//! The durable core mirrors `legacy/pi/packages/agent/src/harness/session`:
//! entries, lane records, lane pointers, and global facts share one sequence;
//! context is projected from a selected branch; and a JSONL tail is repaired
//! only when the final append is syntactically torn.

mod agent_session;
mod agent_session_runtime;
mod compaction;
mod context;
mod event;
mod jsonl;
mod memory;
mod model_runtime_services;
mod multi_session_manager;
mod reducer;
mod repo;
mod session;
mod session_plugin;
mod state;
mod types;

pub use agent_session::{
    AgentSession, AgentSessionOptions, PROMPT_SNAPSHOT_CUSTOM_TYPE, PreparedAgentSession,
    PromptSnapshot, RESOURCE_DIAGNOSTIC_CUSTOM_TYPE, ResourceSnapshot, SessionRuntimeInventory,
    ShellExecutionOptions, SubmitOutcome, read_prompt_snapshot,
};
pub use agent_session_runtime::{
    AgentSessionReplacement, AgentSessionRuntime, AgentSessionRuntimeError,
    AgentSessionRuntimeFactory, AgentSessionRuntimeRequest, AgentSessionRuntimeTarget,
};
pub use compaction::*;
pub use context::{
    ContextEntryTransform, CustomEntryContextMessageProjector, SessionContext,
    SessionContextBuildOptions, SessionModel, agent_message_to_provider_message,
    agent_message_to_runtime_message, build_context_entries, build_session_context,
    default_context_entry_transform, session_entry_to_context_messages,
};
pub use event::{
    AgentSessionEvent, AgentSessionSnapshot, AgentSessionSubscription, BashExecutionSnapshot,
    CompactionSnapshot, QueueSnapshot, RevisionedAgentSessionEvent,
};
pub use jsonl::SessionLog;
pub use memory::{InMemorySession, InMemorySessionRepo};
pub use model_runtime_services::{
    InitialModelRequest, InitialModelResolveError, InitialModelResolver, InitialModelSelection,
    InitialModelSource, ModelRuntimeServices,
};
pub use multi_session_manager::{
    MultiSessionManager, MultiSessionManagerError, PiSession, WeakPiSession,
};
pub use reducer::*;
pub use repo::{JsonlSessionRepo, list_jsonl_session_metadata, load_jsonl_session};
pub use session::{DefaultIdGenerator, IdGenerator, Session, SessionStorage, SessionView};
pub use session_plugin::{
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCompactEvent,
    SessionCompactFailedEvent, SessionForkPosition, SessionHook, SessionIdentity,
    SessionInfoChangedEvent, SessionPlugin, SessionPluginContext, SessionPluginDiagnostic,
    SessionPluginDriver, SessionPluginError, SessionPluginReloadReport, SessionPlugins,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason,
    SessionSwitchReason, SessionTreeEvent, SessionTreeSummary, TreePreparation,
};
pub use types::*;

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
