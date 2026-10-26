#![forbid(unsafe_code)]

//! Product session runtime, managed generations and plugin capability adapters.
//!
//! Durable values and storage live in the internal `history` module and are re-exported
//! here. `AgentSession` owns one generation's activity; `PiSession` follows managed
//! replacements and exposes an ordered observation stream across those generations.

extern crate self as pi_session;

#[doc(hidden)]
pub use async_trait::async_trait as __plugin_async_trait;

mod error;
mod history;
mod management;
mod runtime;
pub use error::SessionError;
pub use history::*;
pub mod plugin;

pub use runtime::{SessionCatalog, SessionHistory};

pub use management::{
    AgentSessionReplacement, PreparedSessionGeneration, SessionGenerationActivation,
    SessionGenerationFactory, SessionGenerationOverlay, SessionGenerationRequest,
};
pub(crate) use runtime::PreparedAgentSession;
pub use runtime::compaction::{
    ContextUsageEstimate, GenericCompactionPolicy, SUMMARIZATION_SYSTEM_PROMPT,
    SessionCompactionPolicy, calculate_context_tokens, current_session_context_tokens,
    estimate_context_tokens,
};
pub(crate) use runtime::compaction::{compact, estimate_session_context_tokens, estimate_tokens};
pub use runtime::{AgentSession, PromptOutcome, SessionInput, ShellExecutionOptions};

pub use management::{IsolatedSessionObservation, IsolatedSessionUsageSnapshot};
pub use management::{SessionObservation, SessionResetReason, SessionUpdate, SessionVersion};
pub use runtime::events::{
    AgentSessionEvent, AgentSessionSnapshot, AgentSessionSubscription, AutoRetrySnapshot,
    BashExecutionSnapshot, CompactionSnapshot, QueueSnapshot, RevisionedAgentSessionEvent,
};

pub use management::{
    CreateSessionOptions, MultiSessionManager, MultiSessionManagerError, OpenSessionOptions,
    PiSession, SessionSource, WeakPiSession,
};
pub use pi_plugin::{ForkPosition, NoticeLevel};
pub use plugin::{
    PiPluginContext, PluginContextBinding, PluginProviderMutation, PluginProviderMutationAccess,
    PluginUiBridge,
};
pub use plugin::{
    PluginError, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCompactEvent,
    SessionCompactFailedEvent, SessionForkPosition, SessionHook, SessionIdentity,
    SessionInfoChangedEvent, SessionPluginContext, SessionShutdownEvent, SessionShutdownReason,
    SessionStartEvent, SessionStartReason, SessionSwitchReason, SessionTreeEvent,
    SessionTreeSummary, TreePreparation,
};
pub use runtime::SessionShellExecutor;
pub use runtime::{
    AgentSessionInitialModelSource, AgentSessionInitialState, AgentSessionOptions,
    AutoRetrySettings, InitialModelRequest, InitialModelResolveError, InitialModelSelection,
    RestoredSessionConfiguration, RestoredToolSelection, SessionRuntimeInventory,
    resolve_model_scope, validate_initial_model_scope,
};

pub(crate) use pi_utils::time::unix_timestamp_ms as now_ms;

pub(crate) fn next_unique_id(_kind: &str) -> String {
    uuid::Uuid::now_v7().to_string()
}
