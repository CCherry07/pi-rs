//! Session and generation-scoped capabilities exposed to plugins.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::unbound;
use super::{
    CompactOptions, ContextUsage, ForkOptions, NavigateTreeOptions, NewSessionOptions,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, SendMessageOptions, SendUserMessageOptions,
};
use crate::{
    AbortSignal, AssistantMessage, CommandSpec, CustomMessageContent, CustomMessageInput, Message,
    ModelSelection, ThinkingLevel, ToolSpec,
};

/// Stable identity for one independently running session launched from a
/// plugin context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IsolatedSessionId(String);

impl IsolatedSessionId {
    #[doc(hidden)]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Input for a fresh session that runs independently of the caller's current
/// [`crate::SessionContext`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedSessionRequest {
    pub input: CustomMessageContent,
    #[serde(default)]
    pub options: IsolatedSessionOptions,
}

/// A single tool-free provider completion requested by a plugin.
///
/// It deliberately does not alter the parent transcript or emit Agent
/// lifecycle events. An omitted model uses the active session model; provider
/// credentials are still resolved at request time. An omitted thinking level
/// inherits the session's current selection.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectCompletionRequest {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub model: Option<ModelSelection>,
    pub thinking_level: Option<ThinkingLevel>,
    pub max_output_tokens: Option<u64>,
}

/// Transient execution provenance, set by the host rather than inferred from
/// prompts, paths, or the persisted session's parent pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionExecutionOrigin {
    #[default]
    User,
    Subagent,
}

/// Token and retention policy for private, in-place context compaction.
/// Policy is supplied by the caller; the runtime owns safe cut points and the
/// detached, tool-free summarization request. The first Agent request is never
/// compacted, preserving the inherited prompt-cache prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralCompactionOptions {
    pub threshold_tokens: u64,
    pub retained_head_messages: usize,
    pub retained_tail_messages: usize,
    pub retained_tail_tokens: u64,
    pub max_summary_tokens: u64,
}

/// One bounded, in-memory tool loop using the calling generation's providers.
/// Executable tools must be a subset of the parent's active tools; advertised
/// schemas stay intact. History may be copied but never mutates the parent.
/// Parent hooks, UI and nested-session capabilities are not inherited.
#[derive(Clone)]
pub struct EphemeralSessionRequest {
    /// None inherits the parent's effective (already assembled) prompt.
    pub system_prompt: Option<String>,
    /// Diagnostic label for denied tool calls; not an authorization signal.
    pub origin: String,
    pub inherit_history: bool,
    /// Optional bounded replay: summarize older text, retaining a balanced
    /// tail of at least this many structured messages. None replays verbatim.
    pub history_tail: Option<usize>,
    pub messages: Vec<Message>,
    /// Execution allowlist, not a replacement for advertised tool definitions.
    pub tools: Vec<String>,
    /// Invocation-private [`crate::AgentPlugin`] instances. The ordinary Agent
    /// driver awaits interested plugins in this vector's order; there is no
    /// separate allowlist of hook kinds.
    ///
    /// # Hooks emitted by the private Agent
    ///
    /// | Hook(s) | Behavior |
    /// | --- | --- |
    /// | `before_agent_start` | Patch the system prompt or append input messages. |
    /// | `agent_start`, `agent_end` | Observe the start/end of one Agent run. |
    /// | `turn_start`, `turn_end` | Observe each model-response/tool-processing turn. |
    /// | `context` | Patch the messages the Agent is about to send to the model. |
    /// | `message_start`, `message_update`, `message_end` | Observe messages/assistant streaming; `message_end` may replace a message with one of the same role. |
    /// | `tool_call` | Patch validated arguments or block a tool call before execution. Hook replacements are not revalidated, matching Pi. |
    /// | `tool_result` | Inspect or patch a tool's result after execution. |
    /// | `tool_execution_start`, `tool_execution_update`, `tool_execution_end` | Observe tool dispatch/progress/completion without patching the result. |
    ///
    /// Hooks follow actual events, not a mandatory checklist. `message_update`
    /// requires assistant stream events; `tool_execution_update` requires tool
    /// progress. `tool_call` runs after argument preparation and validation.
    /// `tool_result` is skipped when a call is rejected before execution, e.g.
    /// by the execution allowlist, invalid arguments, or a blocking hook.
    ///
    /// # Entry-point boundaries
    ///
    /// - `input` and `agent_settled` may be declared but are not emitted: this
    ///   entry bypasses product input and session-settled orchestration.
    /// - `SessionPlugin` lifecycle hooks (`session_*`) are not attached here;
    ///   the private Agent has no managed session or session log.
    /// - `register()` is not called. Tools/commands come from the immutable
    ///   pinned generation; duplicate private plugin IDs fail before a provider
    ///   request. Parent Agent hooks are not inherited. Provider hooks are
    ///   reused from that generation, not supplied through this field.
    /// - Callback contexts expose event data, `run_id`, `cwd`, and cancellation,
    ///   but their `session`, `models`, and `ui` capabilities are unavailable.
    ///   Prompt/context patches affect only the private Agent.
    ///
    /// Cancellation, timeout, or dropping the run future may bypass terminal
    /// hooks such as `agent_end`. Use plugin-owned `Drop`/RAII for cleanup, not
    /// an end callback alone. Construct fresh stateful instances per execution:
    /// cloning this request clones the plugin Arcs, not their underlying state.
    pub plugins: Vec<Arc<dyn crate::AgentPlugin>>,
    pub model: Option<ModelSelection>,
    pub thinking_level: Option<ThinkingLevel>,
    pub max_tool_iterations: usize,
    pub max_input_tokens: Option<u64>,
    /// None disables automatic compaction. No summary, compaction record or
    /// compressor state is ever written to the parent session.
    pub compaction: Option<EphemeralCompactionOptions>,
    pub timeout: std::time::Duration,
}

impl std::fmt::Debug for EphemeralSessionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EphemeralSessionRequest")
            .field("system_prompt", &self.system_prompt)
            .field("origin", &self.origin)
            .field("inherit_history", &self.inherit_history)
            .field("history_tail", &self.history_tail)
            .field("messages", &self.messages)
            .field("tools", &self.tools)
            .field(
                "plugins",
                &self
                    .plugins
                    .iter()
                    .map(|plugin| plugin.id())
                    .collect::<Vec<_>>(),
            )
            .field("model", &self.model)
            .field("thinking_level", &self.thinking_level)
            .field("max_tool_iterations", &self.max_tool_iterations)
            .field("max_input_tokens", &self.max_input_tokens)
            .field("compaction", &self.compaction)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemeralSessionStatus {
    Completed,
    Aborted,
    TimedOut,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct EphemeralSessionOutcome {
    pub messages: Vec<Message>,
    pub status: EphemeralSessionStatus,
}

/// Initial runtime selections for a fresh isolated session.
///
/// An omitted field inherits the corresponding selection from the calling
/// session. `Some(Vec::new())` explicitly starts with no active tools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedSessionOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

impl IsolatedSessionRequest {
    pub fn new(input: CustomMessageContent) -> Self {
        Self {
            input,
            options: IsolatedSessionOptions::default(),
        }
    }

    pub fn options(mut self, options: IsolatedSessionOptions) -> Self {
        self.options = options;
        self
    }
}

/// Terminal result of an independently running session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedSessionOutcome {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub aborted: bool,
}

/// Generation-bound control handle for one independently running session.
///
/// The launched session is owned by the product session manager. This handle
/// deliberately exposes no concrete `AgentSession` or `PiSession` value.
#[derive(Clone)]
pub struct IsolatedSessionHandle {
    id: IsolatedSessionId,
    context: PluginContextHandle,
}

impl IsolatedSessionHandle {
    fn new(id: IsolatedSessionId, context: PluginContextHandle) -> Self {
        Self { id, context }
    }

    pub fn id(&self) -> &IsolatedSessionId {
        &self.id
    }

    pub async fn wait(&self) -> PluginContextResult<IsolatedSessionOutcome> {
        let access = self.context.access()?;
        access
            .wait_for_isolated_session(self.context.scope(), self.id.clone())
            .await
    }

    pub fn abort(&self) -> PluginContextResult<()> {
        self.context
            .access()?
            .abort_isolated_session(self.context.scope(), self.id.clone())
    }
}

/// Stable semantic category for one entry exposed through a [`SessionSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEntryKind {
    Message,
    CustomMessage,
    ModelChange,
    ThinkingLevelChange,
    ActiveToolsChange,
    Compaction,
    BranchSummary,
    Custom,
    Unknown(String),
}

/// Plugin-facing view of one session entry.
///
/// Stable entry metadata is typed while `raw` preserves the complete Pi wire
/// value, including fields introduced by newer hosts or other extensions.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntryView {
    inner: Arc<SessionEntryViewInner>,
}

#[derive(Debug, PartialEq)]
struct SessionEntryViewInner {
    id: String,
    parent_id: Option<String>,
    timestamp_ms: i64,
    kind: SessionEntryKind,
    raw: Value,
}

impl SessionEntryView {
    #[doc(hidden)]
    pub fn new(
        id: String,
        parent_id: Option<String>,
        timestamp_ms: i64,
        kind: SessionEntryKind,
        raw: Value,
    ) -> Self {
        Self {
            inner: Arc::new(SessionEntryViewInner {
                id,
                parent_id,
                timestamp_ms,
                kind,
                raw,
            }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.inner.parent_id.as_deref()
    }

    pub fn timestamp_ms(&self) -> i64 {
        self.inner.timestamp_ms
    }

    pub fn kind(&self) -> &SessionEntryKind {
        &self.inner.kind
    }

    pub fn raw(&self) -> &Value {
        &self.inner.raw
    }

    pub fn into_raw(self) -> Value {
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => inner.raw,
            Err(inner) => inner.raw.clone(),
        }
    }
}

/// Immutable, typed view of the current session history for plugin callbacks.
///
/// A snapshot performs the session read once, after which entry, branch, leaf,
/// label, and header queries are local and cannot observe mixed revisions.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    id: String,
    cwd: PathBuf,
    directory: PathBuf,
    file: Option<PathBuf>,
    name: Option<String>,
    leaf_id: Option<String>,
    entries: Vec<SessionEntryView>,
    branch: Vec<SessionEntryView>,
    labels: BTreeMap<String, String>,
    raw_header: Value,
}

impl SessionSnapshot {
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)]
    pub fn new(
        id: String,
        cwd: PathBuf,
        directory: PathBuf,
        file: Option<PathBuf>,
        name: Option<String>,
        leaf_id: Option<String>,
        entries: Vec<SessionEntryView>,
        branch: Vec<SessionEntryView>,
        labels: BTreeMap<String, String>,
        raw_header: Value,
    ) -> Self {
        Self {
            id,
            cwd,
            directory,
            file,
            name,
            leaf_id,
            entries,
            branch,
            labels,
            raw_header,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn entries(&self) -> &[SessionEntryView] {
        &self.entries
    }

    pub fn branch(&self) -> &[SessionEntryView] {
        &self.branch
    }

    pub fn entry(&self, id: &str) -> Option<&SessionEntryView> {
        self.entries.iter().find(|entry| entry.id() == id)
    }

    pub fn leaf(&self) -> Option<&SessionEntryView> {
        self.leaf_id.as_deref().and_then(|id| self.entry(id))
    }

    pub fn leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn label(&self, id: &str) -> Option<&str> {
        self.labels.get(id).map(String::as_str)
    }

    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }

    pub fn raw_header(&self) -> &Value {
        &self.raw_header
    }
}

/// Session and generation access implemented by the owning product layer.
#[doc(hidden)]
#[async_trait]
pub trait SessionContextAccess: Send + Sync {
    fn session_snapshot(&self) -> PluginContextResult<SessionSnapshot> {
        unbound()
    }

    fn cwd(&self) -> PluginContextResult<PathBuf> {
        unbound()
    }

    fn is_project_trusted(&self) -> PluginContextResult<bool> {
        unbound()
    }

    fn is_idle(&self) -> PluginContextResult<bool> {
        unbound()
    }

    fn has_pending_messages(&self) -> PluginContextResult<bool> {
        unbound()
    }

    fn context_usage(&self) -> PluginContextResult<Option<ContextUsage>> {
        unbound()
    }

    fn system_prompt(&self) -> PluginContextResult<String> {
        unbound()
    }

    fn system_prompt_options(&self, _scope: PluginContextScope) -> PluginContextResult<Value> {
        unbound()
    }

    fn session_cwd(&self) -> PluginContextResult<PathBuf> {
        unbound()
    }

    fn session_dir(&self) -> PluginContextResult<PathBuf> {
        unbound()
    }

    fn session_id(&self) -> PluginContextResult<String> {
        unbound()
    }

    fn execution_origin(&self) -> PluginContextResult<SessionExecutionOrigin> {
        unbound()
    }

    fn session_file(&self) -> PluginContextResult<Option<PathBuf>> {
        unbound()
    }

    fn session_leaf_id(&self) -> PluginContextResult<Option<String>> {
        unbound()
    }

    fn session_leaf_entry(&self) -> PluginContextResult<Option<Value>> {
        unbound()
    }

    fn session_entry(&self, _id: &str) -> PluginContextResult<Option<Value>> {
        unbound()
    }

    fn session_label(&self, _id: &str) -> PluginContextResult<Option<String>> {
        unbound()
    }

    fn session_branch(&self, _from_id: Option<&str>) -> PluginContextResult<Vec<Value>> {
        unbound()
    }

    fn session_context_entries(&self) -> PluginContextResult<Vec<Value>> {
        unbound()
    }

    fn session_header(&self) -> PluginContextResult<Value> {
        unbound()
    }

    fn session_entries(&self) -> PluginContextResult<Vec<Value>> {
        unbound()
    }

    fn session_tree(&self) -> PluginContextResult<Vec<Value>> {
        unbound()
    }

    fn session_name(&self) -> PluginContextResult<Option<String>> {
        unbound()
    }

    fn active_tools(&self) -> PluginContextResult<Vec<String>> {
        unbound()
    }

    fn all_tools(&self) -> PluginContextResult<Vec<ToolSpec>> {
        unbound()
    }

    fn commands(&self) -> PluginContextResult<Vec<CommandSpec>> {
        unbound()
    }

    fn abort(&self) -> PluginContextResult<()> {
        unbound()
    }

    fn compact(&self, _options: CompactOptions) -> PluginContextResult<()> {
        unbound()
    }

    fn shutdown(&self) -> PluginContextResult<()> {
        unbound()
    }

    fn send_message(
        &self,
        _message: CustomMessageInput,
        _options: SendMessageOptions,
    ) -> PluginContextResult<()> {
        unbound()
    }

    fn send_user_message(
        &self,
        _content: CustomMessageContent,
        _options: SendUserMessageOptions,
    ) -> PluginContextResult<()> {
        unbound()
    }

    fn append_entry(&self, _custom_type: String, _data: Option<Value>) -> PluginContextResult<()> {
        unbound()
    }

    fn set_session_name(&self, _name: String) -> PluginContextResult<()> {
        unbound()
    }

    fn set_label(&self, _entry_id: String, _label: Option<String>) -> PluginContextResult<()> {
        unbound()
    }

    fn set_active_tools(&self, _tool_names: Vec<String>) -> PluginContextResult<()> {
        unbound()
    }

    async fn wait_for_idle(&self, _scope: PluginContextScope) -> PluginContextResult<()> {
        unbound()
    }

    async fn send_message_and_wait(
        &self,
        _scope: PluginContextScope,
        _message: CustomMessageInput,
        _options: SendMessageOptions,
    ) -> PluginContextResult<()> {
        unbound()
    }

    async fn send_user_message_and_wait(
        &self,
        _scope: PluginContextScope,
        _content: CustomMessageContent,
        _options: SendUserMessageOptions,
    ) -> PluginContextResult<()> {
        unbound()
    }

    /// Runs a tool-free completion without changing the parent session.
    async fn complete(
        &self,
        _scope: PluginContextScope,
        _request: DirectCompletionRequest,
        _signal: AbortSignal,
    ) -> PluginContextResult<AssistantMessage> {
        unbound()
    }

    async fn run_ephemeral(
        &self,
        _scope: PluginContextScope,
        _request: EphemeralSessionRequest,
        _signal: AbortSignal,
    ) -> PluginContextResult<EphemeralSessionOutcome> {
        unbound()
    }

    async fn launch_isolated_session(
        &self,
        _scope: PluginContextScope,
        _request: IsolatedSessionRequest,
    ) -> PluginContextResult<IsolatedSessionId> {
        unbound()
    }

    async fn wait_for_isolated_session(
        &self,
        _scope: PluginContextScope,
        _id: IsolatedSessionId,
    ) -> PluginContextResult<IsolatedSessionOutcome> {
        unbound()
    }

    fn abort_isolated_session(
        &self,
        _scope: PluginContextScope,
        _id: IsolatedSessionId,
    ) -> PluginContextResult<()> {
        unbound()
    }

    async fn new_session(
        &self,
        _scope: PluginContextScope,
        _options: NewSessionOptions,
    ) -> PluginContextResult<PluginContextReplacement> {
        unbound()
    }

    async fn fork(
        &self,
        _scope: PluginContextScope,
        _entry_id: String,
        _options: ForkOptions,
    ) -> PluginContextResult<PluginContextReplacement> {
        unbound()
    }

    async fn navigate_tree(
        &self,
        _scope: PluginContextScope,
        _target_id: String,
        _options: NavigateTreeOptions,
    ) -> PluginContextResult<bool> {
        unbound()
    }

    async fn switch_session(
        &self,
        _scope: PluginContextScope,
        _session_path: PathBuf,
    ) -> PluginContextResult<PluginContextReplacement> {
        unbound()
    }

    async fn reload(
        &self,
        _scope: PluginContextScope,
    ) -> PluginContextResult<PluginContextReplacement> {
        unbound()
    }
}

/// Current-session capabilities exposed to ordinary callbacks.
///
/// Storage-specific entry variants remain erased at the core seam; their
/// concrete implementation stays in `pi-session`.
#[derive(Clone)]
pub struct SessionContext {
    handle: PluginContextHandle,
}

/// Current-session capabilities exposed to command callbacks.
#[derive(Clone)]
pub struct CommandSessionContext {
    handle: PluginContextHandle,
}

macro_rules! impl_session_context {
    ($context:ident) => {
        impl $context {
            pub(super) fn from_handle(handle: PluginContextHandle) -> Self {
                Self { handle }
            }

            #[doc(hidden)]
            pub fn handle_for_adapter(&self) -> PluginContextHandle {
                self.handle.clone()
            }

            pub fn cwd(&self) -> PluginContextResult<PathBuf> {
                self.handle.access()?.session_cwd()
            }

            /// Captures identity and history once for coherent, typed reads.
            pub fn snapshot(&self) -> PluginContextResult<SessionSnapshot> {
                self.handle.access()?.session_snapshot()
            }

            pub fn directory(&self) -> PluginContextResult<PathBuf> {
                self.handle.access()?.session_dir()
            }

            pub fn id(&self) -> PluginContextResult<String> {
                self.handle.access()?.session_id()
            }

            pub fn execution_origin(&self) -> PluginContextResult<SessionExecutionOrigin> {
                self.handle.access()?.execution_origin()
            }

            pub fn file(&self) -> PluginContextResult<Option<PathBuf>> {
                self.handle.access()?.session_file()
            }

            pub fn leaf_id(&self) -> PluginContextResult<Option<String>> {
                self.handle.access()?.session_leaf_id()
            }

            pub fn leaf_entry(&self) -> PluginContextResult<Option<Value>> {
                self.handle.access()?.session_leaf_entry()
            }

            pub fn entry(&self, id: impl AsRef<str>) -> PluginContextResult<Option<Value>> {
                self.handle.access()?.session_entry(id.as_ref())
            }

            pub fn label(&self, id: impl AsRef<str>) -> PluginContextResult<Option<String>> {
                self.handle.access()?.session_label(id.as_ref())
            }

            pub fn branch(&self, from_id: Option<String>) -> PluginContextResult<Vec<Value>> {
                self.handle.access()?.session_branch(from_id.as_deref())
            }

            pub fn context_entries(&self) -> PluginContextResult<Vec<Value>> {
                self.handle.access()?.session_context_entries()
            }

            pub fn header(&self) -> PluginContextResult<Value> {
                self.handle.access()?.session_header()
            }

            pub fn entries(&self) -> PluginContextResult<Vec<Value>> {
                self.handle.access()?.session_entries()
            }

            pub fn tree(&self) -> PluginContextResult<Vec<Value>> {
                self.handle.access()?.session_tree()
            }

            pub fn name(&self) -> PluginContextResult<Option<String>> {
                self.handle.access()?.session_name()
            }

            pub fn active_tools(&self) -> PluginContextResult<Vec<String>> {
                self.handle.access()?.active_tools()
            }

            pub fn tools(&self) -> PluginContextResult<Vec<ToolSpec>> {
                self.handle.access()?.all_tools()
            }

            pub fn commands(&self) -> PluginContextResult<Vec<CommandSpec>> {
                self.handle.access()?.commands()
            }

            pub fn is_project_trusted(&self) -> PluginContextResult<bool> {
                self.handle.access()?.is_project_trusted()
            }

            pub fn is_idle(&self) -> PluginContextResult<bool> {
                self.handle.access()?.is_idle()
            }

            pub fn has_pending_messages(&self) -> PluginContextResult<bool> {
                self.handle.access()?.has_pending_messages()
            }

            pub fn abort(&self) -> PluginContextResult<()> {
                self.handle.access()?.abort()
            }

            pub fn shutdown(&self) -> PluginContextResult<()> {
                self.handle.access()?.shutdown()
            }

            pub fn context_usage(&self) -> PluginContextResult<Option<ContextUsage>> {
                self.handle.access()?.context_usage()
            }

            pub fn compact(&self, options: CompactOptions) -> PluginContextResult<()> {
                self.handle.access()?.compact(options)
            }

            /// Appends an extension-defined entry to the canonical session
            /// journal without projecting it into provider context.
            pub fn append_entry(
                &self,
                custom_type: impl Into<String>,
                data: Option<Value>,
            ) -> PluginContextResult<()> {
                self.handle.access()?.append_entry(custom_type.into(), data)
            }

            pub fn system_prompt(&self) -> PluginContextResult<String> {
                self.handle.access()?.system_prompt()
            }

            /// Runs a tool-free provider completion without changing this
            /// session's transcript or emitting Agent lifecycle events.
            pub async fn complete(
                &self,
                request: DirectCompletionRequest,
                signal: AbortSignal,
            ) -> PluginContextResult<AssistantMessage> {
                let access = self.handle.access()?;
                access.complete(self.handle.scope(), request, signal).await
            }

            /// Runs a bounded ephemeral session without acquiring a managed
            /// session slot. Safe to await from compaction/shutdown hooks.
            pub async fn run_ephemeral(
                &self,
                request: EphemeralSessionRequest,
                signal: AbortSignal,
            ) -> PluginContextResult<EphemeralSessionOutcome> {
                self.handle
                    .access()?
                    .run_ephemeral(self.handle.scope(), request, signal)
                    .await
            }

            /// Launches a fresh session without replacing the current one.
            pub async fn launch_isolated_session(
                &self,
                request: IsolatedSessionRequest,
            ) -> PluginContextResult<IsolatedSessionHandle> {
                let access = self.handle.access()?;
                let id = access
                    .launch_isolated_session(self.handle.scope(), request)
                    .await?;
                Ok(IsolatedSessionHandle::new(id, self.handle.clone()))
            }
        }
    };
}

impl_session_context!(SessionContext);
impl_session_context!(CommandSessionContext);

impl CommandSessionContext {
    pub fn system_prompt_options(&self) -> PluginContextResult<Value> {
        self.handle
            .access()?
            .system_prompt_options(self.handle.scope())
    }

    pub async fn wait_for_idle(&self) -> PluginContextResult<()> {
        let access = self.handle.access()?;
        access.wait_for_idle(self.handle.scope()).await
    }

    pub async fn create(
        &self,
        options: NewSessionOptions,
    ) -> PluginContextResult<SessionReplacement> {
        let access = self.handle.access()?;
        let replacement = access.new_session(self.handle.scope(), options).await?;
        Self::replace(replacement)
    }

    pub async fn fork(
        &self,
        entry_id: impl Into<String>,
        options: ForkOptions,
    ) -> PluginContextResult<SessionReplacement> {
        let access = self.handle.access()?;
        let replacement = access
            .fork(self.handle.scope(), entry_id.into(), options)
            .await?;
        Self::replace(replacement)
    }

    pub async fn navigate(
        &self,
        target_id: impl Into<String>,
        options: NavigateTreeOptions,
    ) -> PluginContextResult<bool> {
        let access = self.handle.access()?;
        access
            .navigate_tree(self.handle.scope(), target_id.into(), options)
            .await
    }

    pub async fn switch(
        &self,
        session_path: impl Into<PathBuf>,
    ) -> PluginContextResult<SessionReplacement> {
        let access = self.handle.access()?;
        let replacement = access
            .switch_session(self.handle.scope(), session_path.into())
            .await?;
        Self::replace(replacement)
    }

    /// Rebuilds the product generation and returns capabilities bound to it.
    ///
    /// The current command context retires as part of the replacement. Use
    /// the returned context for every operation after this call.
    pub async fn reload(&self) -> PluginContextResult<ReplacedSessionContext> {
        let access = self.handle.access()?;
        let replacement = access.reload(self.handle.scope()).await?;
        if replacement.cancelled {
            return Err(PluginContextError::Failed(
                "session reload unexpectedly reported cancellation".to_string(),
            ));
        }
        let handle = replacement.context.ok_or_else(|| {
            PluginContextError::Failed(
                "session reload did not return a product context".to_string(),
            )
        })?;
        Ok(ReplacedSessionContext::new(handle))
    }

    fn replace(replacement: PluginContextReplacement) -> PluginContextResult<SessionReplacement> {
        if replacement.cancelled {
            Ok(SessionReplacement::Cancelled)
        } else {
            let handle = replacement.context.ok_or_else(|| {
                PluginContextError::Failed(
                    "session replacement did not return a product context".to_string(),
                )
            })?;
            Ok(SessionReplacement::Replaced(ReplacedSessionContext::new(
                handle,
            )))
        }
    }
}

/// Fresh product capabilities bound to a successfully replaced session.
#[derive(Clone)]
pub struct ReplacedSessionContext {
    pub session: CommandSessionContext,
    pub models: super::CommandModelsContext,
    pub ui: super::UiContext,
    handle: PluginContextHandle,
}

impl ReplacedSessionContext {
    pub(super) fn new(handle: PluginContextHandle) -> Self {
        Self {
            session: CommandSessionContext::from_handle(handle.clone()),
            models: super::CommandModelsContext::from_handle(handle.clone()),
            ui: super::UiContext::new(handle.clone()),
            handle,
        }
    }

    pub async fn send_message(
        &self,
        message: CustomMessageInput,
        options: SendMessageOptions,
    ) -> PluginContextResult<()> {
        let access = self.handle.access()?;
        access
            .send_message_and_wait(self.handle.scope(), message, options)
            .await
    }

    pub async fn send_user_message(
        &self,
        content: CustomMessageContent,
        options: SendUserMessageOptions,
    ) -> PluginContextResult<()> {
        let access = self.handle.access()?;
        access
            .send_user_message_and_wait(self.handle.scope(), content, options)
            .await
    }
}

#[derive(Clone)]
pub enum SessionReplacement {
    Cancelled,
    Replaced(ReplacedSessionContext),
}
