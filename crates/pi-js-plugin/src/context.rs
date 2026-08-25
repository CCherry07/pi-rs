use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use pi_session::{
    AgentSession, AgentSessionReplacement, ForkPosition, MAIN_LANE, PiSession, SessionDocument,
    SessionRecord, WeakPiSession, build_context_entries, estimate_context_tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

/// The capability set attached to one JavaScript callback invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionContextScope {
    Base,
    Command,
}

/// Synchronous, side-effect-free reads exposed by `NativeExtensionContext.query`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextQuery {
    Cwd,
    IsProjectTrusted,
    Model,
    ScopedModels,
    Models,
    AvailableModels,
    ProviderDisplayName { provider: String },
    ThinkingLevel,
    IsIdle,
    HasPendingMessages,
    ContextUsage,
    SystemPrompt,
    SystemPromptOptions,
    SessionCwd,
    SessionDir,
    SessionId,
    SessionFile,
    SessionLeafId,
    SessionLeafEntry,
    SessionEntry { id: String },
    SessionLabel { id: String },
    SessionBranch { from_id: Option<String> },
    SessionContextEntries,
    SessionHeader,
    SessionEntries,
    SessionTree,
    SessionName,
}

/// Fire-and-forget commands exposed by `NativeExtensionContext.notify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextNotification {
    Abort,
    Compact {
        #[serde(default)]
        custom_instructions: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionForkPosition {
    Before,
    At,
}

/// Awaited commands exposed by `NativeExtensionContext.request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextRequest {
    WaitForIdle,
    NewSession {
        #[serde(default)]
        parent_session: Option<String>,
    },
    Fork {
        entry_id: String,
        #[serde(default = "default_fork_position")]
        position: ExtensionForkPosition,
    },
    NavigateTree {
        target_id: String,
        #[serde(default)]
        summarize: bool,
        #[serde(default)]
        custom_instructions: Option<String>,
        #[serde(default)]
        replace_instructions: bool,
        #[serde(default)]
        label: Option<String>,
    },
    SwitchSession {
        session_path: PathBuf,
    },
    Reload,
}

fn default_fork_position() -> ExtensionForkPosition {
    ExtensionForkPosition::Before
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionContextError {
    #[error("JavaScript extension context has retired")]
    Retired,
    #[error("JavaScript extension context is not bound to a session")]
    Unbound,
    #[error("{0} is only available in an extension command context")]
    CommandOnly(&'static str),
    #[error("JavaScript extension context capability is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid JavaScript extension context operation: {0}")]
    Invalid(String),
    #[error("JavaScript extension context operation failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait ExtensionContextAccess: Send + Sync {
    fn query(
        &self,
        scope: ExtensionContextScope,
        query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError>;

    fn notify(
        &self,
        scope: ExtensionContextScope,
        notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError>;

    async fn request(
        &self,
        scope: ExtensionContextScope,
        request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError>;

    fn retire(&self) {}
}

struct ExtensionContextEpochInner {
    active: AtomicBool,
    access: Arc<dyn ExtensionContextAccess>,
}

/// Generation-owned lifetime guard for native extension contexts.
#[derive(Clone)]
pub struct ExtensionContextEpoch {
    inner: Arc<ExtensionContextEpochInner>,
}

impl ExtensionContextEpoch {
    pub fn new(access: Arc<dyn ExtensionContextAccess>) -> Self {
        Self {
            inner: Arc::new(ExtensionContextEpochInner {
                active: AtomicBool::new(true),
                access,
            }),
        }
    }

    pub fn unavailable() -> Self {
        Self::new(Arc::new(UnavailableExtensionContextAccess))
    }

    pub fn handle(&self, scope: ExtensionContextScope) -> ExtensionContextHandle {
        ExtensionContextHandle {
            epoch: self.clone(),
            scope,
        }
    }

    pub fn retire(&self) {
        if self.inner.active.swap(false, Ordering::AcqRel) {
            self.inner.access.retire();
        }
    }

    pub(crate) fn query_for_adapter(
        &self,
        query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError> {
        if !self.inner.active.load(Ordering::Acquire) {
            return Err(ExtensionContextError::Retired);
        }
        self.inner
            .access
            .query(ExtensionContextScope::Command, query)
    }
}

/// Cloneable native handle passed directly to one Node callback.
#[derive(Clone)]
pub struct ExtensionContextHandle {
    epoch: ExtensionContextEpoch,
    scope: ExtensionContextScope,
}

impl ExtensionContextHandle {
    pub fn query(&self, query: ExtensionContextQuery) -> Result<Value, ExtensionContextError> {
        self.ensure_active()?;
        self.epoch.inner.access.query(self.scope, query)
    }

    pub fn notify(
        &self,
        notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError> {
        self.ensure_active()?;
        self.epoch.inner.access.notify(self.scope, notification)
    }

    pub async fn request(
        &self,
        request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError> {
        self.ensure_active()?;
        let result = self.epoch.inner.access.request(self.scope, request).await;
        self.ensure_active()?;
        result
    }

    pub fn scope(&self) -> ExtensionContextScope {
        self.scope
    }

    fn ensure_active(&self) -> Result<(), ExtensionContextError> {
        self.epoch
            .inner
            .active
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or(ExtensionContextError::Retired)
    }
}

pub(crate) struct UnavailableExtensionContextAccess;

#[async_trait]
impl ExtensionContextAccess for UnavailableExtensionContextAccess {
    fn query(
        &self,
        _scope: ExtensionContextScope,
        _query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError> {
        Err(ExtensionContextError::Unbound)
    }

    fn notify(
        &self,
        _scope: ExtensionContextScope,
        _notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError> {
        Err(ExtensionContextError::Unbound)
    }

    async fn request(
        &self,
        _scope: ExtensionContextScope,
        _request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError> {
        Err(ExtensionContextError::Unbound)
    }
}

#[derive(Default)]
struct ExtensionSessionBindingState {
    session: Option<WeakPiSession>,
    project_trust_by_session_id: HashMap<String, bool>,
}

/// Stable outer-session capability shared by every JavaScript generation.
#[derive(Clone)]
pub struct ExtensionSessionBinding {
    state: Arc<Mutex<ExtensionSessionBindingState>>,
    shutdown: watch::Sender<bool>,
}

impl Default for ExtensionSessionBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionSessionBinding {
    pub fn new() -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            state: Arc::new(Mutex::new(ExtensionSessionBindingState::default())),
            shutdown,
        }
    }

    pub fn bind(&self, session: PiSession) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session = Some(session.downgrade());
    }

    pub async fn wait_for_shutdown(&self) {
        let mut receiver = self.shutdown.subscribe();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }

    fn session(&self) -> Option<PiSession> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .as_ref()
            .and_then(WeakPiSession::upgrade)
    }

    fn register_project_trust(&self, session_id: String, trusted: bool) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project_trust_by_session_id
            .insert(session_id, trusted);
    }

    fn project_trust(&self, session_id: &str) -> Option<bool> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project_trust_by_session_id
            .get(session_id)
            .copied()
    }

    fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

/// Production context implementation backed by one prepared generation and
/// the stable outer [`PiSession`] handle once application startup completes.
pub struct SessionExtensionContextAccess {
    project_trusted: bool,
    generation_session: RwLock<Option<Weak<AgentSession>>>,
    binding: ExtensionSessionBinding,
    runtime: tokio::runtime::Handle,
}

impl SessionExtensionContextAccess {
    pub fn new(project_trusted: bool, binding: ExtensionSessionBinding) -> Self {
        Self {
            project_trusted,
            generation_session: RwLock::new(None),
            binding,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    pub fn bind_generation_session(&self, session: Arc<AgentSession>) {
        self.binding
            .register_project_trust(session.log().header().id, self.project_trusted);
        *self
            .generation_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(&session));
    }

    fn session(&self) -> Result<Arc<AgentSession>, ExtensionContextError> {
        let generation = self
            .generation_session
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(session) = &generation
            && !session.is_closed()
        {
            return Ok(Arc::clone(session));
        }
        self.binding
            .session()
            .map(|session| session.current())
            .or(generation)
            .ok_or(ExtensionContextError::Unbound)
    }

    fn pi_session(&self) -> Result<PiSession, ExtensionContextError> {
        self.binding.session().ok_or(ExtensionContextError::Unbound)
    }

    fn document(&self) -> Result<(Arc<AgentSession>, SessionDocument), ExtensionContextError> {
        let session = self.session()?;
        let document = session
            .log()
            .load()
            .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
        Ok((session, document))
    }
}

#[async_trait]
impl ExtensionContextAccess for SessionExtensionContextAccess {
    fn query(
        &self,
        scope: ExtensionContextScope,
        query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError> {
        let session = self.session()?;
        let runtime = session.runtime();
        let snapshot = session.snapshot();
        match query {
            ExtensionContextQuery::Cwd => value(runtime.cwd()),
            ExtensionContextQuery::IsProjectTrusted => {
                let id = session.log().header().id;
                Ok(json!(
                    self.binding
                        .project_trust(&id)
                        .unwrap_or(self.project_trusted)
                ))
            }
            ExtensionContextQuery::Model => {
                let state = snapshot.agent;
                value(runtime.model(&state.provider_id, &state.model_id))
            }
            ExtensionContextQuery::ScopedModels => Ok(json!([])),
            ExtensionContextQuery::Models => value(runtime.models()),
            ExtensionContextQuery::AvailableModels => value(runtime.available_models()),
            ExtensionContextQuery::ProviderDisplayName { provider } => Ok(json!(
                runtime
                    .provider_name(&pi_core::ProviderId::new(&provider))
                    .unwrap_or(provider)
            )),
            ExtensionContextQuery::ThinkingLevel => value(snapshot.agent.thinking_level),
            ExtensionContextQuery::IsIdle => Ok(json!(
                !runtime.agent().state().is_running
                    && snapshot.compaction.is_none()
                    && snapshot.bash.is_none()
            )),
            ExtensionContextQuery::HasPendingMessages => Ok(json!(
                !snapshot.queue.steering.is_empty() || !snapshot.queue.follow_up.is_empty()
            )),
            ExtensionContextQuery::ContextUsage => {
                let Some(context_window) = session.active_context_window() else {
                    return Ok(Value::Null);
                };
                let document = session
                    .log()
                    .load()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                let context = document
                    .context()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                let tokens = estimate_context_tokens(&context.messages).tokens;
                let percent = if context_window == 0 {
                    0.0
                } else {
                    tokens as f64 / context_window as f64 * 100.0
                };
                Ok(json!({
                    "tokens": tokens,
                    "contextWindow": context_window,
                    "percent": percent,
                }))
            }
            ExtensionContextQuery::SystemPrompt => Ok(json!(snapshot.agent.system_prompt)),
            ExtensionContextQuery::SystemPromptOptions => {
                require_command(scope, "getSystemPromptOptions")?;
                runtime
                    .prompt_options()
                    .map_or_else(|| Ok(json!({ "cwd": runtime.cwd() })), value)
            }
            ExtensionContextQuery::SessionCwd => value(runtime.cwd()),
            ExtensionContextQuery::SessionDir => value(
                session
                    .log()
                    .path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("")),
            ),
            ExtensionContextQuery::SessionId => Ok(json!(session.log().header().id)),
            ExtensionContextQuery::SessionFile => {
                if session.log().is_materialized() {
                    value(session.log().path())
                } else {
                    Ok(Value::Null)
                }
            }
            ExtensionContextQuery::SessionLeafId => Ok(json!(session.log().leaf_id())),
            ExtensionContextQuery::SessionLeafEntry => {
                let (_, document) = self.document()?;
                let entry = document
                    .leaf_id(MAIN_LANE)
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?
                    .and_then(|id| document.entries.iter().find(|entry| entry.id == id));
                value(entry)
            }
            ExtensionContextQuery::SessionEntry { id } => {
                let (_, document) = self.document()?;
                value(document.entries.iter().find(|entry| entry.id == id))
            }
            ExtensionContextQuery::SessionLabel { id } => {
                let (_, document) = self.document()?;
                value(document.labels.get(&id))
            }
            ExtensionContextQuery::SessionBranch { from_id } => {
                let (_, document) = self.document()?;
                let branch = document
                    .branch_at(
                        from_id
                            .as_deref()
                            .or_else(|| document.leaf_id(MAIN_LANE).ok().flatten()),
                    )
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                value(branch)
            }
            ExtensionContextQuery::SessionContextEntries => {
                let (_, document) = self.document()?;
                let branch = document
                    .branch()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                value(build_context_entries(
                    &branch,
                    &pi_session::SessionContextBuildOptions::default(),
                ))
            }
            ExtensionContextQuery::SessionHeader => {
                let (_, document) = self.document()?;
                value(document.header)
            }
            ExtensionContextQuery::SessionEntries => {
                let (_, document) = self.document()?;
                value(document.entries)
            }
            ExtensionContextQuery::SessionTree => {
                let (_, document) = self.document()?;
                value(session_tree(&document))
            }
            ExtensionContextQuery::SessionName => {
                let (_, document) = self.document()?;
                value(document.name)
            }
        }
    }

    fn notify(
        &self,
        _scope: ExtensionContextScope,
        notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError> {
        match notification {
            ExtensionContextNotification::Abort => self.session()?.abort(),
            ExtensionContextNotification::Compact {
                custom_instructions,
            } => {
                let session = self.session()?;
                self.runtime.spawn(async move {
                    let _ = session.compact(custom_instructions).await;
                });
            }
            ExtensionContextNotification::Shutdown => {
                if let Ok(session) = self.pi_session() {
                    session.abort();
                }
                self.binding.request_shutdown();
            }
        }
        Ok(())
    }

    async fn request(
        &self,
        scope: ExtensionContextScope,
        request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError> {
        let operation = request.operation_name();
        require_command(scope, operation)?;
        match request {
            ExtensionContextRequest::WaitForIdle => {
                self.session()?.runtime().wait_for_idle().await;
                Ok(Value::Null)
            }
            ExtensionContextRequest::NewSession { parent_session } => {
                if parent_session.is_some() {
                    return Err(ExtensionContextError::Unavailable(
                        "newSession({ parentSession })".to_string(),
                    ));
                }
                let pi_session = self.pi_session()?;
                let current = pi_session.current();
                let directory = current
                    .log()
                    .path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let path = directory.join(format!("{}.jsonl", uuid::Uuid::now_v7()));
                let replacement = pi_session
                    .new_session(current.runtime().cwd(), path)
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                replacement_value(replacement)
            }
            ExtensionContextRequest::Fork { entry_id, position } => {
                let position = match position {
                    ExtensionForkPosition::Before => ForkPosition::Before,
                    ExtensionForkPosition::At => ForkPosition::At,
                };
                let replacement = self
                    .pi_session()?
                    .fork_session(entry_id, position)
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                replacement_value(replacement)
            }
            ExtensionContextRequest::NavigateTree {
                target_id,
                summarize,
                custom_instructions,
                replace_instructions,
                label,
            } => {
                if summarize || custom_instructions.is_some() || replace_instructions {
                    return Err(ExtensionContextError::Unavailable(
                        "summarized tree navigation".to_string(),
                    ));
                }
                let session = self.session()?;
                session
                    .checkout(Some(&target_id))
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                if let Some(label) = label {
                    session
                        .log()
                        .set_label(&target_id, Some(label))
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                }
                Ok(json!({ "cancelled": false }))
            }
            ExtensionContextRequest::SwitchSession { session_path } => {
                let replacement = self
                    .pi_session()?
                    .resume_session(session_path)
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                replacement_value(replacement)
            }
            ExtensionContextRequest::Reload => {
                self.pi_session()?
                    .reload()
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                Ok(Value::Null)
            }
        }
    }

    fn retire(&self) {
        self.generation_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

impl ExtensionContextRequest {
    fn operation_name(&self) -> &'static str {
        match self {
            Self::WaitForIdle => "waitForIdle",
            Self::NewSession { .. } => "newSession",
            Self::Fork { .. } => "fork",
            Self::NavigateTree { .. } => "navigateTree",
            Self::SwitchSession { .. } => "switchSession",
            Self::Reload => "reload",
        }
    }
}

fn require_command(
    scope: ExtensionContextScope,
    operation: &'static str,
) -> Result<(), ExtensionContextError> {
    if scope == ExtensionContextScope::Command {
        Ok(())
    } else {
        Err(ExtensionContextError::CommandOnly(operation))
    }
}

fn replacement_value(replacement: AgentSessionReplacement) -> Result<Value, ExtensionContextError> {
    Ok(json!({
        "cancelled": replacement == AgentSessionReplacement::Cancelled,
    }))
}

fn value<T: Serialize>(value: T) -> Result<Value, ExtensionContextError> {
    serde_json::to_value(value).map_err(|error| ExtensionContextError::Failed(error.to_string()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionSessionTreeNode {
    entry: SessionRecord,
    children: Vec<ExtensionSessionTreeNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn session_tree(document: &SessionDocument) -> Vec<ExtensionSessionTreeNode> {
    let ids = document
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<HashSet<_>>();
    let mut children = HashMap::<Option<String>, Vec<SessionRecord>>::new();
    for entry in &document.entries {
        let parent = entry
            .parent_id
            .as_ref()
            .filter(|parent| parent.as_str() != entry.id && ids.contains(parent.as_str()))
            .cloned();
        children.entry(parent).or_default().push(entry.clone());
    }
    for entries in children.values_mut() {
        entries.sort_by_key(|entry| entry.timestamp_ms);
    }

    fn build(
        entry: SessionRecord,
        children: &mut HashMap<Option<String>, Vec<SessionRecord>>,
        labels: &HashMap<String, String>,
    ) -> ExtensionSessionTreeNode {
        let descendants = children
            .remove(&Some(entry.id.clone()))
            .unwrap_or_default()
            .into_iter()
            .map(|child| build(child, children, labels))
            .collect();
        let label = labels.get(&entry.id).cloned();
        ExtensionSessionTreeNode {
            entry,
            children: descendants,
            label,
        }
    }

    children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| build(entry, &mut children, &document.labels))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticAccess;

    #[async_trait]
    impl ExtensionContextAccess for StaticAccess {
        fn query(
            &self,
            _scope: ExtensionContextScope,
            _query: ExtensionContextQuery,
        ) -> Result<Value, ExtensionContextError> {
            Ok(json!("ok"))
        }

        fn notify(
            &self,
            _scope: ExtensionContextScope,
            _notification: ExtensionContextNotification,
        ) -> Result<(), ExtensionContextError> {
            Ok(())
        }

        async fn request(
            &self,
            _scope: ExtensionContextScope,
            _request: ExtensionContextRequest,
        ) -> Result<Value, ExtensionContextError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn retired_epoch_rejects_stored_handles() {
        let epoch = ExtensionContextEpoch::new(Arc::new(StaticAccess));
        let handle = epoch.handle(ExtensionContextScope::Base);
        assert_eq!(
            handle.query(ExtensionContextQuery::Cwd).unwrap(),
            json!("ok")
        );

        epoch.retire();
        assert!(matches!(
            handle.query(ExtensionContextQuery::Cwd),
            Err(ExtensionContextError::Retired)
        ));
    }

    #[test]
    fn context_operations_use_camel_case_tags() {
        assert_eq!(
            serde_json::to_value(ExtensionContextQuery::SessionEntry {
                id: "entry-1".to_string(),
            })
            .unwrap(),
            json!({ "type": "sessionEntry", "id": "entry-1" })
        );
        assert_eq!(
            serde_json::to_value(ExtensionContextRequest::SwitchSession {
                session_path: PathBuf::from("session.jsonl"),
            })
            .unwrap(),
            json!({ "type": "switchSession", "sessionPath": "session.jsonl" })
        );
    }
}
