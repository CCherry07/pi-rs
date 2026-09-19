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

mod agent_session;
mod agent_session_runtime;
mod compaction;
mod context;
mod event;
mod isolated_context;
mod isolated_session;
mod journal;
mod multi_session_manager;
pub mod plugin;
mod plugin_context;
mod session_options;
mod types;

pub use agent_session::{
    AgentSession, PreparedAgentSession, SessionInput, ShellExecutionOptions, SubmitOutcome,
};
pub use agent_session_runtime::{
    AgentSessionInitialModelSource, AgentSessionInitialState, AgentSessionReplacement,
    PreparedSessionGeneration, SessionGenerationActivation, SessionGenerationFactory,
    SessionGenerationOverlay, SessionGenerationRequest,
};
pub(crate) use compaction::{
    CompactionError, compact, estimate_session_context_tokens, estimate_tokens, prepare_compaction,
    should_compact,
};
pub use compaction::{
    ContextUsageEstimate, SUMMARIZATION_SYSTEM_PROMPT, calculate_context_tokens,
    current_session_context_tokens, estimate_context_tokens,
};
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
pub use isolated_context::InheritedSessionContext;
pub use isolated_session::{IsolatedSessionObservation, IsolatedSessionUsageSnapshot};
pub use journal::{
    EffectiveLaneConfiguration, ExactSessionIdResolution, JsonlSessionRepo, LaneOperationState,
    LaneReductionInput, LaneReductionResult, LaneState, LaneStepState, NewestOwnEntryState,
    OperationTargetState, RecordLogCorruption, RecordLogCorruptionReason, RecordLogSlice,
    SessionLog, TerminalFailureSource, TerminalFailureState, ToolBatchCallState, ToolBatchState,
    aggregate_document_usage, aggregate_session_usage, reduce_lane_state, session_entry_usage,
    validate_record_log,
};
pub use multi_session_manager::{
    MultiSessionManager, MultiSessionManagerError, PiSession, WeakPiSession,
};
pub use pi_plugin::{ForkPosition, NoticeLevel};
pub use plugin::{
    PluginError, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCompactEvent,
    SessionCompactFailedEvent, SessionForkPosition, SessionHook, SessionIdentity,
    SessionInfoChangedEvent, SessionPluginContext, SessionShutdownEvent, SessionShutdownReason,
    SessionStartEvent, SessionStartReason, SessionSwitchReason, SessionTreeEvent,
    SessionTreeSummary, TreePreparation,
};
pub use plugin_context::{
    PiPluginContext, PluginContextBinding, PluginProviderMutation, PluginProviderMutationAccess,
    PluginUiBridge,
};
pub use session_options::{
    AgentSessionOptions, AutoRetrySettings, InitialModelRequest, InitialModelResolveError,
    InitialModelSelection, SessionRuntimeInventory, resolve_model_scope,
    validate_initial_model_scope,
};
pub use types::*;

pub(crate) use pi_utils::time::unix_timestamp_ms as now_ms;

pub(crate) fn next_unique_id(_kind: &str) -> String {
    uuid::Uuid::now_v7().to_string()
}
