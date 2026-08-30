//! Stable session lifecycle plugin contract.
//!
//! Runtime drivers and persistence stay in sibling modules; native plugins
//! consume this contract through the `pi-session` crate.

use std::path::PathBuf;

use crate::types::{
    BranchSummaryEntry, CompactionEntry, CompactionPreparation, CompactionReason, SessionRecord,
};
use async_trait::async_trait;
use pi_core::{
    AbortSignal, ContextParts, ModelsContext, PluginContextHandle, PluginId,
    SessionContext as PluginSessionContext, UiContext,
};

pub use pi_core::ForkPosition as SessionForkPosition;

#[derive(Debug, thiserror::Error)]
pub enum SessionPluginError {
    #[error("duplicate session plugin id: {0}")]
    DuplicatePlugin(String),
    #[error("session plugin source {index} failed: {message}")]
    Load { index: usize, message: String },
    #[error("session plugin failed: {0}")]
    Failure(String),
    #[error(transparent)]
    Context(#[from] pi_core::PluginContextError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub parent_session_id: Option<String>,
}

#[derive(Clone)]
pub struct SessionPluginContext {
    plugin_id: PluginId,
    generation: u64,
    identity: SessionIdentity,
    pub session: PluginSessionContext,
    pub models: ModelsContext,
    pub ui: UiContext,
}

impl std::fmt::Debug for SessionPluginContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionPluginContext")
            .field("plugin_id", &self.plugin_id)
            .field("generation", &self.generation)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SessionPluginContext {
    fn eq(&self, other: &Self) -> bool {
        self.plugin_id == other.plugin_id
            && self.generation == other.generation
            && self.identity == other.identity
    }
}

impl Eq for SessionPluginContext {}

impl SessionPluginContext {
    #[doc(hidden)]
    pub fn unavailable_for_testing(
        plugin_id: PluginId,
        generation: u64,
        session: SessionIdentity,
    ) -> Self {
        let context = ContextParts::unavailable();
        Self {
            plugin_id,
            generation,
            identity: session,
            session: context.session,
            models: context.models,
            ui: context.ui,
        }
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    pub(crate) fn with_plugin_context(
        plugin_id: PluginId,
        generation: u64,
        identity: SessionIdentity,
        context: ContextParts,
    ) -> Self {
        Self {
            plugin_id,
            generation,
            identity,
            session: context.session,
            models: context.models,
            ui: context.ui,
        }
    }

    #[doc(hidden)]
    pub fn plugin_context_handle(&self) -> PluginContextHandle {
        self.session.handle_for_adapter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStartReason {
    Startup,
    Reload,
    New,
    Resume,
    Fork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStartEvent {
    pub reason: SessionStartReason,
    pub previous_session_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfoChangedEvent {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSwitchReason {
    New,
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBeforeSwitchEvent {
    pub reason: SessionSwitchReason,
    pub target_session_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBeforeForkEvent {
    pub entry_id: String,
    pub position: SessionForkPosition,
}

#[derive(Debug, Clone)]
pub struct SessionBeforeCompactEvent {
    pub preparation: CompactionPreparation,
    pub branch_entries: Vec<SessionRecord>,
    pub custom_instructions: Option<String>,
    pub reason: CompactionReason,
    pub will_retry: bool,
    pub signal: AbortSignal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionCompactEvent {
    pub compaction_entry: CompactionEntry,
    pub from_extension: bool,
    pub reason: CompactionReason,
    pub will_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCompactFailedEvent {
    pub reason: CompactionReason,
    pub error_message: Option<String>,
    pub aborted: bool,
    pub will_retry: bool,
    pub from_extension: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionShutdownReason {
    Quit,
    Reload,
    New,
    Resume,
    Fork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionShutdownEvent {
    pub reason: SessionShutdownReason,
    pub target_session_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreePreparation {
    pub target_id: Option<String>,
    pub old_leaf_id: Option<String>,
    pub common_ancestor_id: Option<String>,
    pub entries_to_summarize: Vec<SessionRecord>,
    pub user_wants_summary: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionBeforeTreeEvent {
    pub preparation: TreePreparation,
    pub signal: AbortSignal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeEvent {
    pub new_leaf_id: Option<String>,
    pub old_leaf_id: Option<String>,
    pub summary_entry: Option<BranchSummaryEntry>,
    pub from_extension: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionBeforeSwitchResult {
    pub cancel: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionBeforeForkResult {
    pub cancel: bool,
    pub skip_conversation_restore: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionBeforeCompactResult {
    pub cancel: bool,
    pub compaction: Option<CompactionEntry>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionBeforeTreeResult {
    pub cancel: bool,
    pub summary: Option<SessionTreeSummary>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeSummary {
    pub summary: String,
    pub details: Option<serde_json::Value>,
    pub usage: Option<pi_core::Usage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionHook {
    Start,
    InfoChanged,
    BeforeSwitch,
    BeforeFork,
    BeforeCompact,
    Compact,
    CompactFailed,
    Shutdown,
    BeforeTree,
    Tree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPluginDiagnostic {
    pub plugin_id: PluginId,
    pub generation: u64,
    pub hook: SessionHook,
    pub message: String,
}

/// Pi-style session lifecycle extension. Observer failures are isolated by
/// the `pi-session` driver; before-hook results use registration order.
#[async_trait]
pub trait SessionPlugin: Send + Sync {
    fn id(&self) -> PluginId;

    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }

    async fn session_info_changed(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionInfoChangedEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }

    async fn session_before_switch(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeSwitchEvent,
    ) -> Result<Option<SessionBeforeSwitchResult>, SessionPluginError> {
        Ok(None)
    }

    async fn session_before_fork(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeForkEvent,
    ) -> Result<Option<SessionBeforeForkResult>, SessionPluginError> {
        Ok(None)
    }

    async fn session_before_compact(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, SessionPluginError> {
        Ok(None)
    }

    async fn session_compact(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionCompactEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }

    async fn session_compact_failed(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionCompactFailedEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }

    async fn session_shutdown(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }

    async fn session_before_tree(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeTreeEvent,
    ) -> Result<Option<SessionBeforeTreeResult>, SessionPluginError> {
        Ok(None)
    }

    async fn session_tree(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionTreeEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }
}
