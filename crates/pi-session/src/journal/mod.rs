//! Durable Pi v4 session journal.
//!
//! This module owns mutation sequencing, state projection, validation, JSONL
//! persistence, and repositories. The
//! crate root re-exports the stable session interface; the module layout keeps
//! persistence details local to their owning implementation.

mod jsonl;
mod paths;
mod reducer;
mod repo;
mod state;
mod usage;
mod validation;

pub use jsonl::SessionLog;
pub use reducer::{
    EffectiveLaneConfiguration, LaneOperationState, LaneReductionInput, LaneReductionResult,
    LaneState, LaneStepState, NewestOwnEntryState, OperationTargetState, RecordLogCorruption,
    RecordLogCorruptionReason, RecordLogSlice, TerminalFailureSource, TerminalFailureState,
    ToolBatchCallState, ToolBatchState, reduce_lane_state, validate_record_log,
};
pub use repo::{ExactSessionIdResolution, JsonlSessionRepo};
pub use usage::{aggregate_document_usage, aggregate_session_usage, session_entry_usage};

pub(crate) use paths::comparable_path;
pub(crate) use repo::validate_session_id;
