//! Durable v4 session history, context projection and pure recovery reduction.
//!
//! This module uses foundational values without importing Runtime, Management or plugins.
//! SessionStore commits validated mutations and retains the existing JSONL wire format.
mod journal;
mod projection;
mod provenance;
mod types;
pub use journal::{
    EffectiveLaneConfiguration, ExactSessionIdResolution, JsonlSessionRepo, LaneOperationState,
    LaneReductionInput, LaneReductionResult, LaneState, LaneStepState, NewestOwnEntryState,
    OperationTargetState, RecordLogCorruption, RecordLogCorruptionReason, RecordLogSlice,
    SessionStore, TerminalFailureSource, TerminalFailureState, ToolBatchCallState, ToolBatchState,
    aggregate_document_usage, aggregate_session_usage, reduce_lane_state, session_entry_usage,
    validate_record_log,
};
pub use projection::{
    ContextEntryTransform, CustomEntryContextMessageProjector, SessionContext,
    SessionContextBuildOptions, SessionModel, agent_message_to_provider_message,
    agent_message_to_runtime_message, annotate_compaction, build_context_entries,
    build_session_context, default_context_entry_transform, project_session_entries,
    session_entry_to_context_messages,
};
pub use types::*;

pub use journal::{reduce_lane_operation, validate_session_id};
use pi_utils::time::unix_timestamp_ms as now_ms;
pub use projection::validate_context_edit;
pub use provenance::{
    CUSTOM_TYPE as ISOLATED_CONTEXT_TYPE, InheritedSessionContext, IsolatedContextSeed,
    ORIGIN_CUSTOM_TYPE as ISOLATED_ORIGIN_TYPE, fork_entries,
    is_seed as is_inherited_context_entry, retain_recent_turns,
};
fn next_unique_id(_kind: &str) -> String {
    uuid::Uuid::now_v7().to_string()
}
