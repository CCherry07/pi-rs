//! Stable session lifecycle plugin contract.
//!
//! Agent and Session hooks share one Plugin instance. Persistence stays in pi-session.

use std::path::PathBuf;

use crate::{AbortSignal, PluginId};
use pi_core::session::{
    BranchSummaryEntry, CompactionEntry, CompactionPreparation, CompactionReason, SessionRecord,
};
use pi_plugin::{
    ContextParts, ModelsContext, PluginContextHandle, SessionContext as PluginSessionContext,
    UiContext,
};

pub use pi_plugin::ForkPosition as SessionForkPosition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub parent_session_id: Option<String>,
}

/// Session metadata supplied to the unified driver for each lifecycle dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDispatchContext {
    pub identity: SessionIdentity,
    pub generation: u64,
}

#[derive(Clone)]
pub struct SessionPluginContext {
    plugin_id: PluginId,
    generation: u64,
    identity: SessionIdentity,
    workspace: crate::WorkspaceSnapshot,
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
            workspace: context.workspace_or_cwd(&session.cwd),
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

    pub fn workspace(&self) -> &crate::WorkspaceSnapshot {
        &self.workspace
    }

    pub fn cwd(&self) -> &std::path::Path {
        self.workspace.cwd()
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
            workspace: context.workspace_or_cwd(&identity.cwd),
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

define_hooks! {
    SessionHook {
        Start => "session_start",
        InfoChanged => "session_info_changed",
        BeforeSwitch => "session_before_switch",
        BeforeFork => "session_before_fork",
        BeforeCompact => "session_before_compact",
        Compact => "session_compact",
        CompactFailed => "session_compact_failed",
        Shutdown => "session_shutdown",
        BeforeTree => "session_before_tree",
        Tree => "session_tree",
    }
}

pub(super) const SESSION_HOOKS: [SessionHook; 10] = [
    SessionHook::Start,
    SessionHook::InfoChanged,
    SessionHook::BeforeSwitch,
    SessionHook::BeforeFork,
    SessionHook::BeforeCompact,
    SessionHook::Compact,
    SessionHook::CompactFailed,
    SessionHook::Shutdown,
    SessionHook::BeforeTree,
    SessionHook::Tree,
];

/// Exact Session callbacks, derived by the Plugin attribute or a validated JS manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionHookInterests(u16);
impl SessionHookInterests {
    pub const fn from_hooks(hooks: &[SessionHook]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < hooks.len() {
            bits |= 1 << (hooks[index] as usize);
            index += 1;
        }
        Self(bits)
    }
    pub const fn contains(self, hook: SessionHook) -> bool {
        self.0 & (1 << (hook as usize)) != 0
    }
}
