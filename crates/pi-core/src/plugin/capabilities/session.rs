//! Session and generation-scoped capabilities exposed to plugins.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::types::unbound;
use super::{
    CompactOptions, ContextUsage, ForkOptions, NavigateTreeOptions, NewSessionOptions,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, SendMessageOptions, SendUserMessageOptions,
};
use crate::{CommandSpec, CustomMessageContent, CustomMessageInput, ToolSpec};

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

    async fn reload(&self, _scope: PluginContextScope) -> PluginContextResult<()> {
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

            pub fn system_prompt(&self) -> PluginContextResult<String> {
                self.handle.access()?.system_prompt()
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

    pub async fn reload(&self) -> PluginContextResult<()> {
        let access = self.handle.access()?;
        access.reload(self.handle.scope()).await
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
