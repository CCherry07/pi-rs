use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use pi_core::{
    IsolatedSessionId, IsolatedSessionOptions, IsolatedSessionOutcome, IsolatedSessionRequest,
    ModelSelection, PluginContextError,
};
use tokio::sync::watch;

use crate::isolated_session::IsolatedSessionRegistry;
use crate::{
    AgentSession, AgentSessionInitialModelSource, AgentSessionInitialState,
    AgentSessionReplacement, AgentSessionRuntime, AgentSessionRuntimeError,
    AgentSessionRuntimeFactory, AgentSessionRuntimeRequest, AgentSessionRuntimeTarget,
    ForkPosition, PreparedAgentSession, SessionError, SessionFileFormat, SessionGenerationOverlay,
    inspect_session_file,
};

/// Owns and coordinates multiple active Pi sessions.
///
/// The active-session table is intentionally private. Frontends keep the
/// returned [`PiSession`] handles and do not coordinate a separate registry.
#[derive(Clone)]
pub struct MultiSessionManager {
    inner: Arc<MultiSessionManagerInner>,
}

struct MultiSessionManagerInner {
    factory: Arc<dyn AgentSessionRuntimeFactory>,
    sessions: Mutex<HashMap<String, PiSession>>,
    isolated_sessions: IsolatedSessionRegistry,
    operation_gate: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

/// A cloneable handle to one frontend-owned Pi session.
///
/// New, resume, fork, and reload replace the handle's current
/// [`AgentSession`] atomically while preserving the handle itself.
#[derive(Clone)]
pub struct PiSession {
    registration_id: Arc<str>,
    runtime: AgentSessionRuntime,
    manager: Weak<MultiSessionManagerInner>,
}

/// A non-owning handle to a managed [`PiSession`].
///
/// Upgrading succeeds while the owning [`MultiSessionManager`] still has the
/// session registered.
#[derive(Clone)]
pub struct WeakPiSession {
    registration_id: Arc<str>,
    manager: Weak<MultiSessionManagerInner>,
}

#[derive(Debug, thiserror::Error)]
pub enum MultiSessionManagerError {
    #[error(transparent)]
    Runtime(#[from] AgentSessionRuntimeError),
    #[error("multi-session manager is closed")]
    Closed,
    #[error("session is not managed by this multi-session manager")]
    UnknownSession,
    #[error("session path is already active: {0}")]
    SessionAlreadyActive(PathBuf),
    #[error("import path has no file name: {0}")]
    InvalidImportPath(PathBuf),
    #[error("cannot import over the active session file: {0}")]
    ImportWouldReplaceCurrent(PathBuf),
    #[error("invalid isolated session request: {0}")]
    InvalidIsolatedRequest(String),
}

#[derive(Clone)]
struct SharedFactory(Arc<dyn AgentSessionRuntimeFactory>);

#[async_trait]
impl AgentSessionRuntimeFactory for SharedFactory {
    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError> {
        self.0.prepare(request).await
    }
}

impl MultiSessionManager {
    pub fn new<F>(factory: F) -> Self
    where
        F: AgentSessionRuntimeFactory + 'static,
    {
        Self {
            inner: Arc::new(MultiSessionManagerInner {
                factory: Arc::new(factory),
                sessions: Mutex::new(HashMap::new()),
                isolated_sessions: IsolatedSessionRegistry::default(),
                operation_gate: tokio::sync::Mutex::new(()),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub async fn create_session(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            AgentSessionRuntimeTarget::create(cwd, path),
            ExistingSessionPolicy::Reject,
            SessionGenerationOverlay::default(),
        )
        .await
    }

    /// Creates a session with transient factories layered onto every runtime
    /// generation owned by the returned handle.
    pub async fn create_session_with_overlay(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        generation_overlay: SessionGenerationOverlay,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            AgentSessionRuntimeTarget::create(cwd, path),
            ExistingSessionPolicy::Reject,
            generation_overlay,
        )
        .await
    }

    pub async fn open_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            AgentSessionRuntimeTarget::open(path),
            ExistingSessionPolicy::Reuse,
            SessionGenerationOverlay::default(),
        )
        .await
    }

    /// Opens a persisted session with transient generation-local factories.
    pub async fn open_session_with_overlay(
        &self,
        path: impl Into<PathBuf>,
        generation_overlay: SessionGenerationOverlay,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            AgentSessionRuntimeTarget::open(path),
            ExistingSessionPolicy::Reuse,
            generation_overlay,
        )
        .await
    }

    /// Returns the currently managed handles. Ordering is unspecified.
    pub fn sessions(&self) -> Vec<PiSession> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    pub async fn close_session(&self, session: &PiSession) -> Result<(), MultiSessionManagerError> {
        let _operation = self.inner.operation_gate.lock().await;
        self.inner.ensure_open()?;
        self.inner.close_session_tree_locked(session).await
    }

    pub async fn shutdown(&self) -> Result<(), MultiSessionManagerError> {
        let _operation = self.inner.operation_gate.lock().await;
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let sessions = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>();
        let mut first_error = None;
        for session in sessions {
            if let Err(error) = session.runtime.shutdown().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), |error| Err(error.into()))
    }

    async fn acquire(
        &self,
        target: AgentSessionRuntimeTarget,
        existing: ExistingSessionPolicy,
        generation_overlay: SessionGenerationOverlay,
    ) -> Result<PiSession, MultiSessionManagerError> {
        let _operation = self.inner.operation_gate.lock().await;
        self.inner.ensure_open()?;
        self.inner
            .acquire_locked(target, existing, generation_overlay, None)
            .await
    }
}

impl MultiSessionManagerInner {
    async fn acquire_locked(
        self: &Arc<Self>,
        target: AgentSessionRuntimeTarget,
        existing: ExistingSessionPolicy,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
    ) -> Result<PiSession, MultiSessionManagerError> {
        let path = comparable_path(target.path());
        if let Some(active) = self.session_at_path(&path) {
            return match existing {
                ExistingSessionPolicy::Reuse => Ok(active),
                ExistingSessionPolicy::Reject => {
                    Err(MultiSessionManagerError::SessionAlreadyActive(path))
                }
            };
        }
        let runtime = AgentSessionRuntime::create_with_overlay_and_initial_state(
            SharedFactory(Arc::clone(&self.factory)),
            target,
            generation_overlay,
            initial_state,
        )
        .await?;
        let registration_id: Arc<str> = Arc::from(uuid::Uuid::now_v7().to_string());
        let session = PiSession {
            registration_id: Arc::clone(&registration_id),
            runtime,
            manager: Arc::downgrade(self),
        };
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(registration_id.to_string(), session.clone());
        self.factory.session_registered(&session);
        Ok(session)
    }

    async fn close_session_tree_locked(
        &self,
        root: &PiSession,
    ) -> Result<(), MultiSessionManagerError> {
        if !self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(root.registration_id())
        {
            return Err(MultiSessionManagerError::UnknownSession);
        }
        let mut pending = vec![root.clone()];
        let mut sessions = Vec::new();
        while let Some(session) = pending.pop() {
            let children = self
                .isolated_sessions
                .remove_owned(session.registration_id());
            for child in &children {
                child.abort();
            }
            pending.extend(children);
            sessions.push(session);
        }
        for session in &sessions {
            self.isolated_sessions
                .remove_session(session.registration_id());
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(session.registration_id());
        }
        for session in sessions.into_iter().rev() {
            session.runtime.shutdown().await?;
        }
        Ok(())
    }
}

impl PiSession {
    pub fn downgrade(&self) -> WeakPiSession {
        WeakPiSession {
            registration_id: Arc::clone(&self.registration_id),
            manager: self.manager.clone(),
        }
    }

    /// Returns the current generation of the underlying agent session.
    pub fn current(&self) -> Arc<AgentSession> {
        self.runtime.session()
    }

    /// Watches replacements caused by new, resume, fork, or reload.
    pub fn subscribe(&self) -> watch::Receiver<Arc<AgentSession>> {
        self.runtime.subscribe()
    }

    pub fn id(&self) -> String {
        self.current().log().header().id
    }

    pub fn path(&self) -> PathBuf {
        self.current().log().path().to_path_buf()
    }

    pub fn cwd(&self) -> PathBuf {
        self.current().runtime().cwd().to_path_buf()
    }

    pub(crate) fn registration_id(&self) -> &str {
        &self.registration_id
    }

    pub async fn launch_isolated_session(
        &self,
        request: IsolatedSessionRequest,
    ) -> Result<IsolatedSessionId, MultiSessionManagerError> {
        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        if !manager
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(self.registration_id())
        {
            return Err(MultiSessionManagerError::UnknownSession);
        }
        let initial_state = resolve_isolated_initial_state(&self.current(), request.options)?;
        let path = isolated_session_path(&self.path());
        let child = manager
            .acquire_locked(
                AgentSessionRuntimeTarget::create(self.cwd(), path),
                ExistingSessionPolicy::Reject,
                SessionGenerationOverlay::default()
                    .with_execution_origin(pi_core::SessionExecutionOrigin::Subagent),
                Some(initial_state),
            )
            .await?;
        Ok(manager
            .isolated_sessions
            .launch(self.registration_id().to_owned(), child, request.input)
            .await)
    }

    pub async fn wait_for_isolated_session(
        &self,
        id: &IsolatedSessionId,
    ) -> Result<IsolatedSessionOutcome, PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .wait(self.registration_id(), id)
            .await
            .map_err(PluginContextError::Failed)
    }

    pub fn abort_isolated_session(&self, id: &IsolatedSessionId) -> Result<(), PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .abort(self.registration_id(), id)
            .map_err(PluginContextError::Failed)
    }

    pub async fn new_session(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let cwd = cwd.into();
        let path = path.into();
        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        manager.ensure_path_available(self, &path)?;
        Ok(self.runtime.new_session(cwd, path).await?)
    }

    pub async fn new_session_with_parent(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        parent_session: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let cwd = cwd.into();
        let path = path.into();
        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        manager.ensure_path_available(self, &path)?;
        Ok(self
            .runtime
            .new_session_with_parent(cwd, path, Some(parent_session.into()))
            .await?)
    }

    pub async fn resume_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let path = path.into();
        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        manager.ensure_path_available(self, &path)?;
        Ok(self.runtime.switch_session(path).await?)
    }

    /// Imports a v4 JSONL file, or migrates a coding-agent v1-v3 file, into the
    /// current session directory and switches to it using resume lifecycle events.
    pub async fn import_session(
        &self,
        source: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let source = source.into();
        let file_name = source
            .file_name()
            .ok_or_else(|| MultiSessionManagerError::InvalidImportPath(source.clone()))?;
        let current_path = self.path();
        let mut destination = current_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(file_name);
        if comparable_path(&source) == comparable_path(&destination)
            && matches!(
                inspect_session_file(&source).map_err(AgentSessionRuntimeError::from)?,
                SessionFileFormat::Legacy { .. }
            )
        {
            destination = legacy_import_destination(&destination);
        }
        if comparable_path(&current_path) == comparable_path(&destination) {
            return Err(MultiSessionManagerError::ImportWouldReplaceCurrent(
                destination,
            ));
        }

        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        manager.ensure_path_available(self, &destination)?;
        Ok(self.runtime.import_session(source, destination).await?)
    }

    pub async fn fork_session(
        &self,
        entry_id: impl Into<String>,
        position: ForkPosition,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        Ok(self.runtime.fork_session(entry_id, position).await?)
    }

    pub async fn reload(&self) -> Result<(), MultiSessionManagerError> {
        let manager = self.manager()?;
        let _operation = manager.operation_gate.lock().await;
        manager.ensure_open()?;
        self.runtime.reload().await?;
        Ok(())
    }

    pub fn abort(&self) {
        self.runtime.abort();
    }

    fn manager(&self) -> Result<Arc<MultiSessionManagerInner>, MultiSessionManagerError> {
        self.manager
            .upgrade()
            .ok_or(MultiSessionManagerError::Closed)
    }
}

fn resolve_isolated_initial_state(
    parent: &AgentSession,
    options: IsolatedSessionOptions,
) -> Result<AgentSessionInitialState, MultiSessionManagerError> {
    let parent_state = parent.runtime().agent().state();
    let active_tools = match options.active_tools {
        None => parent_state.active_tools.clone(),
        Some(requested) => {
            let ceiling = parent_state
                .active_tools
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut seen = HashSet::new();
            let mut selected = Vec::new();
            let mut unavailable = Vec::new();
            for raw in requested {
                let tool = raw.trim();
                if tool.is_empty() {
                    return Err(MultiSessionManagerError::InvalidIsolatedRequest(
                        "active tool names must not be empty".to_string(),
                    ));
                }
                if !ceiling.contains(tool) {
                    unavailable.push(tool.to_string());
                } else if seen.insert(tool.to_string()) {
                    selected.push(tool.to_string());
                }
            }
            if !unavailable.is_empty() {
                unavailable.sort();
                unavailable.dedup();
                return Err(MultiSessionManagerError::InvalidIsolatedRequest(format!(
                    "tools are not active in the calling session: {}",
                    unavailable.join(", ")
                )));
            }
            selected
        }
    };
    let (model, model_source) = options.model.map_or_else(
        || {
            (
                ModelSelection {
                    provider: parent_state.provider_id.clone(),
                    model_id: parent_state.model_id.clone(),
                },
                AgentSessionInitialModelSource::Inherited,
            )
        },
        |model| (model, AgentSessionInitialModelSource::Requested),
    );
    if model.provider.as_str().trim().is_empty() || model.model_id.as_str().trim().is_empty() {
        return Err(MultiSessionManagerError::InvalidIsolatedRequest(
            "model provider and id must not be empty".to_string(),
        ));
    }
    Ok(AgentSessionInitialState {
        model,
        model_source,
        thinking_level: options
            .thinking_level
            .unwrap_or(parent_state.thinking_level),
        active_tools,
    })
}

fn legacy_import_destination(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session");
    let candidate = source.with_file_name(format!("{stem}.v4.jsonl"));
    if !candidate.exists() {
        return candidate;
    }
    source.with_file_name(format!("{stem}.v4-{}.jsonl", uuid::Uuid::now_v7()))
}

fn isolated_session_path(owner: &Path) -> PathBuf {
    let stem = owner
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session");
    owner
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(stem)
        .join("isolated")
        .join(format!("{}.jsonl", uuid::Uuid::now_v7()))
}

impl WeakPiSession {
    pub fn upgrade(&self) -> Option<PiSession> {
        self.manager
            .upgrade()?
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(self.registration_id.as_ref())
            .cloned()
    }

    pub(crate) fn registration_id(&self) -> &str {
        &self.registration_id
    }
}

impl MultiSessionManagerInner {
    fn ensure_open(&self) -> Result<(), MultiSessionManagerError> {
        if self.closed.load(Ordering::Acquire) {
            Err(MultiSessionManagerError::Closed)
        } else {
            Ok(())
        }
    }

    fn session_at_path(&self, path: &Path) -> Option<PiSession> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .find(|session| comparable_path(&session.path()) == path)
            .cloned()
    }

    fn ensure_path_available(
        &self,
        owner: &PiSession,
        path: &Path,
    ) -> Result<(), MultiSessionManagerError> {
        let path = comparable_path(path);
        let occupied = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|session| {
                session.registration_id != owner.registration_id
                    && comparable_path(&session.path()) == path
            });
        if occupied {
            Err(MultiSessionManagerError::SessionAlreadyActive(path))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum ExistingSessionPolicy {
    Reuse,
    Reject,
}

fn comparable_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(file_name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(file_name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

#[cfg(test)]
mod tests {
    use pi_agent::AgentOptions;
    use pi_core::{ContentBlock, CustomMessageContent, Message, ModelId, ProviderId};
    use pi_runtime::PiRuntime;
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

    use super::*;
    use crate::AgentSessionOptions;

    fn test_manager() -> MultiSessionManager {
        test_manager_with_turns([])
    }

    fn test_manager_with_turns(
        turns: impl IntoIterator<Item = ScriptedTurn>,
    ) -> MultiSessionManager {
        let turns = turns.into_iter().collect::<Vec<_>>();
        MultiSessionManager::new(move |request: AgentSessionRuntimeRequest| {
            let turns = turns.clone();
            async move {
                let initial_state = request.initial_state;
                let (cwd, path, create, reused_log) = match request.target {
                    AgentSessionRuntimeTarget::Create { cwd, path, .. } => (cwd, path, true, None),
                    AgentSessionRuntimeTarget::Open { path } => {
                        let (_, document) = crate::SessionLog::open(&path)?;
                        (document.header.cwd, path, false, None)
                    }
                    AgentSessionRuntimeTarget::Reuse { log } => {
                        let document = log.load()?;
                        (
                            document.header.cwd,
                            log.path().to_path_buf(),
                            false,
                            Some(log),
                        )
                    }
                };
                let runtime = request
                    .generation_overlay
                    .apply_to(PiRuntime::builder())
                    .provider_plugin(ScriptedProviderPlugin::scripted(turns))
                    .agent_options(AgentOptions {
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        cwd,
                        ..AgentOptions::default()
                    })
                    .build()?;
                if let Some(initial_state) = initial_state {
                    initial_state.apply_to(&runtime)?;
                }
                if create {
                    AgentSession::prepare_create_with_options(
                        runtime,
                        path,
                        AgentSessionOptions::default(),
                    )
                    .await
                } else if let Some(log) = reused_log {
                    AgentSession::prepare_reuse_with_options(
                        runtime,
                        log,
                        AgentSessionOptions::default(),
                    )
                    .await
                } else {
                    AgentSession::prepare_open_with_options(
                        runtime,
                        path,
                        AgentSessionOptions::default(),
                    )
                    .await
                }
            }
        })
    }

    #[tokio::test]
    async fn fresh_isolated_session_runs_without_replacing_its_owner() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::Text("isolated answer".to_string())]);
        let owner_path = directory.path().join("primary.jsonl");
        let owner = manager
            .create_session(directory.path(), &owner_path)
            .await
            .unwrap();
        let owner_id = owner.id();
        let owner_leaf = owner.current().log().leaf_id();

        let isolated_id = owner
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "inspect independently".to_string(),
            )))
            .await
            .unwrap();
        let outcome = owner.wait_for_isolated_session(&isolated_id).await.unwrap();

        assert_eq!(owner.id(), owner_id);
        assert_eq!(owner.path(), owner_path);
        assert_eq!(owner.current().log().leaf_id(), owner_leaf);
        assert!(!owner.current().runtime().agent().is_running());
        assert!(!outcome.aborted);
        assert!(outcome.messages.iter().any(|message| {
            matches!(message, Message::Assistant(assistant)
            if assistant.content.iter().any(|content| {
                matches!(content, ContentBlock::Text(text) if text.text == "isolated answer")
            }))
        }));
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == isolated_id.as_str())
            .unwrap();
        assert_eq!(outcome.session_id, child.id());
        assert_eq!(
            owner.current().runtime().execution_origin(),
            pi_core::SessionExecutionOrigin::User
        );
        assert_eq!(
            child.current().runtime().execution_origin(),
            pi_core::SessionExecutionOrigin::Subagent
        );
        child.reload().await.unwrap();
        assert_eq!(
            child.current().runtime().execution_origin(),
            pi_core::SessionExecutionOrigin::Subagent
        );
        let grandchild_id = child
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "nested inspection".into(),
            )))
            .await
            .unwrap();
        child
            .wait_for_isolated_session(&grandchild_id)
            .await
            .unwrap();
        let grandchild = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == grandchild_id.as_str())
            .unwrap();
        assert_eq!(
            grandchild.current().runtime().execution_origin(),
            pi_core::SessionExecutionOrigin::Subagent
        );
        assert_eq!(
            owner.current().runtime().execution_origin(),
            pi_core::SessionExecutionOrigin::User
        );
        assert!(
            child
                .path()
                .starts_with(directory.path().join("primary/isolated"))
        );
        assert!(child.path().exists());
        assert!(!owner_path.exists());

        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn isolated_initial_state_is_applied_without_mutating_the_owner() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("primary.jsonl"))
            .await
            .unwrap();
        let owner_state = owner.current().runtime().agent().state();
        let isolated_id = owner
            .launch_isolated_session(
                IsolatedSessionRequest::new(CustomMessageContent::Text("inspect".to_string()))
                    .options(IsolatedSessionOptions {
                        active_tools: Some(Vec::new()),
                        model: Some(ModelSelection::new("scripted", "child-model")),
                        thinking_level: Some(pi_core::ThinkingLevel::High),
                    }),
            )
            .await
            .unwrap();
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == isolated_id.as_str())
            .unwrap();
        let child_state = child.current().runtime().agent().state();

        assert_eq!(child_state.model_id, ModelId::new("child-model"));
        assert_eq!(child_state.thinking_level, pi_core::ThinkingLevel::High);
        assert!(child_state.active_tools.is_empty());
        let child_context = child.current().log().load().unwrap().context().unwrap();
        assert_eq!(
            child_context.model,
            Some(crate::SessionModel {
                provider: ProviderId::new("scripted"),
                model_id: ModelId::new("child-model"),
            })
        );
        assert_eq!(child_context.thinking_level, "high");
        assert_eq!(child_context.active_tool_names, Some(Vec::new()));
        let owner_after = owner.current().runtime().agent().state();
        assert_eq!(owner_after.provider_id, owner_state.provider_id);
        assert_eq!(owner_after.model_id, owner_state.model_id);
        assert_eq!(owner_after.thinking_level, owner_state.thinking_level);
        assert_eq!(owner_after.active_tools, owner_state.active_tools);

        owner.abort_isolated_session(&isolated_id).unwrap();
        owner.wait_for_isolated_session(&isolated_id).await.unwrap();
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn isolated_tools_cannot_exceed_the_calling_session_ceiling() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager();
        let owner = manager
            .create_session(directory.path(), directory.path().join("primary.jsonl"))
            .await
            .unwrap();
        let error = owner
            .launch_isolated_session(
                IsolatedSessionRequest::new(CustomMessageContent::Text("inspect".to_string()))
                    .options(IsolatedSessionOptions {
                        active_tools: Some(vec!["write".to_string()]),
                        ..IsolatedSessionOptions::default()
                    }),
            )
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("not active in the calling session")
        );
        assert_eq!(manager.sessions().len(), 1);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn isolated_session_abort_finishes_with_an_aborted_outcome() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("primary.jsonl"))
            .await
            .unwrap();
        let isolated_id = owner
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "wait".to_string(),
            )))
            .await
            .unwrap();
        owner.abort_isolated_session(&isolated_id).unwrap();
        let outcome = owner.wait_for_isolated_session(&isolated_id).await.unwrap();

        assert!(outcome.aborted);
        assert_eq!(owner.path(), directory.path().join("primary.jsonl"));
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn isolated_control_is_owner_scoped_and_closed_owners_cannot_launch() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("primary.jsonl"))
            .await
            .unwrap();
        let other = manager
            .create_session(directory.path(), directory.path().join("other.jsonl"))
            .await
            .unwrap();
        let isolated_id = owner
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "wait".to_string(),
            )))
            .await
            .unwrap();

        assert!(other.abort_isolated_session(&isolated_id).is_err());
        let foreign_wait = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            other.wait_for_isolated_session(&isolated_id),
        )
        .await
        .expect("ownership rejection must not wait for the child");
        assert!(foreign_wait.is_err());

        owner.abort_isolated_session(&isolated_id).unwrap();
        assert!(
            owner
                .wait_for_isolated_session(&isolated_id)
                .await
                .unwrap()
                .aborted
        );
        manager.close_session(&owner).await.unwrap();
        assert!(matches!(
            owner
                .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                    "orphan".to_string()
                ),))
                .await,
            Err(MultiSessionManagerError::UnknownSession)
        ));

        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn closing_an_owner_closes_its_isolated_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("primary.jsonl"))
            .await
            .unwrap();
        let isolated_id = owner
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "wait".to_string(),
            )))
            .await
            .unwrap();
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == isolated_id.as_str())
            .unwrap();

        manager.close_session(&owner).await.unwrap();

        assert!(owner.current().is_closed());
        assert!(child.current().is_closed());
        assert!(manager.sessions().is_empty());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn manager_owns_multiple_sessions_and_closes_them() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager();
        let first = manager
            .create_session(directory.path(), directory.path().join("first.jsonl"))
            .await
            .unwrap();
        let second = manager
            .create_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap();

        assert_ne!(first.id(), second.id());
        assert_eq!(manager.sessions().len(), 2);

        manager.close_session(&first).await.unwrap();
        assert!(first.current().is_closed());
        assert!(!second.current().is_closed());
        assert_eq!(manager.sessions().len(), 1);

        manager.shutdown().await.unwrap();
        assert!(second.current().is_closed());
        assert!(manager.sessions().is_empty());
        assert!(matches!(
            manager
                .create_session(directory.path(), directory.path().join("third.jsonl"))
                .await,
            Err(MultiSessionManagerError::Closed)
        ));
    }

    #[tokio::test]
    async fn weak_session_handle_does_not_keep_the_manager_or_session_alive() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager();
        let session = manager
            .create_session(directory.path(), directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let weak = session.downgrade();

        assert_eq!(weak.upgrade().unwrap().id(), session.id());
        drop(session);
        drop(manager);

        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn opening_an_active_path_reuses_its_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let manager = test_manager();
        let created = manager
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        created.current().log().materialize().unwrap();

        let opened = manager.open_session(&path).await.unwrap();

        assert_eq!(created.registration_id, opened.registration_id);
        assert_eq!(manager.sessions().len(), 1);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn transition_rejects_a_path_owned_by_another_handle() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let second_path = directory.path().join("second.jsonl");
        let manager = test_manager();
        let first = manager
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let _second = manager
            .create_session(directory.path(), &second_path)
            .await
            .unwrap();

        let error = first.resume_session(&second_path).await.unwrap_err();

        assert!(matches!(
            error,
            MultiSessionManagerError::SessionAlreadyActive(_)
        ));
        assert_eq!(first.path(), first_path);
        manager.shutdown().await.unwrap();
    }
}
