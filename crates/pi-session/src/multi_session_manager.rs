use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

#[cfg(test)]
use async_trait::async_trait;
use pi_core::{CustomMessageContent, CustomMessageInput, Message, ModelSelection};
use pi_plugin::{
    IsolatedFollowUpReceipt, IsolatedMessageReceipt, IsolatedSessionId, IsolatedSessionOptions,
    IsolatedSessionOutcome, IsolatedSessionRequest, IsolatedSessionTurnId, PluginContextError,
};
use tokio::sync::watch;

use crate::agent_session_runtime::{
    AgentSessionRuntime, AgentSessionRuntimeTarget, ResolvedSessionTransition,
};
use crate::isolated_session::IsolatedSessionRegistry;
use crate::journal::comparable_path;
use crate::{
    AgentSession, AgentSessionInitialModelSource, AgentSessionInitialState,
    AgentSessionReplacement, ForkPosition, IsolatedSessionObservation, SessionError,
    SessionGenerationFactory, SessionGenerationOverlay, SessionLog,
};
#[cfg(test)]
use crate::{PreparedSessionGeneration, SessionGenerationActivation, SessionGenerationRequest};

/// Owns and coordinates multiple active Pi sessions.
///
/// The active-session table is intentionally private. Frontends keep the
/// returned [`PiSession`] handles and do not coordinate a separate registry.
#[derive(Clone)]
pub struct MultiSessionManager {
    inner: Arc<MultiSessionManagerInner>,
}

struct MultiSessionManagerInner {
    factory: Arc<dyn SessionGenerationFactory>,
    sessions: Mutex<HashMap<String, PiSession>>,
    isolated_sessions: IsolatedSessionRegistry,
    operation_gate: Arc<tokio::sync::RwLock<()>>,
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
    Session(#[from] SessionError),
    #[error("multi-session manager is closed")]
    Closed,
    #[error("session is not managed by this multi-session manager")]
    UnknownSession,
    #[error("session path is already active: {0}")]
    SessionAlreadyActive(PathBuf),
    #[error("invalid isolated session request: {0}")]
    InvalidIsolatedRequest(String),
}

enum SessionReplacementRequest {
    New {
        cwd: PathBuf,
        path: PathBuf,
        parent_session: Option<PathBuf>,
    },
    Resume {
        path: PathBuf,
    },
    Fork {
        entry_id: String,
        position: ForkPosition,
    },
    Reload,
}

impl SessionReplacementRequest {
    fn resolve(
        self,
        owner: &PiSession,
        manager: &MultiSessionManagerInner,
    ) -> Result<ResolvedSessionTransition, MultiSessionManagerError> {
        match self {
            Self::New {
                cwd,
                path,
                parent_session,
            } => {
                manager.ensure_path_available(owner, &path)?;
                Ok(ResolvedSessionTransition::New {
                    cwd,
                    path,
                    parent_session,
                })
            }
            Self::Resume { path } => {
                manager.ensure_path_available(owner, &path)?;
                Ok(ResolvedSessionTransition::Resume { path })
            }
            Self::Fork { entry_id, position } => {
                Ok(ResolvedSessionTransition::Fork { entry_id, position })
            }
            Self::Reload => Ok(ResolvedSessionTransition::Reload),
        }
    }
}

/// Owns every manager-level capability for one session replacement until the
/// nested runtime transaction either commits or returns an error.
struct ManagedSessionReplacement {
    owner: PiSession,
    manager: Arc<MultiSessionManagerInner>,
    _operation: tokio::sync::OwnedRwLockWriteGuard<()>,
}

/// Polls a lifecycle transaction in place and detaches only its unfinished
/// remainder when the requesting future is dropped.
struct CompleteOnDrop<T: Send + 'static> {
    future: Option<Pin<Box<dyn Future<Output = T> + Send>>>,
    runtime: tokio::runtime::Handle,
    polling: bool,
}

#[derive(Clone, Copy)]
enum UnclaimedSessionPolicy {
    Retain,
    Close,
}

struct SessionAcquisition {
    target: AgentSessionRuntimeTarget,
    existing: ExistingSessionPolicy,
    generation_overlay: SessionGenerationOverlay,
    initial_state: Option<AgentSessionInitialState>,
    initial_context: Option<crate::isolated_context::IsolatedContextSeed>,
    unclaimed: UnclaimedSessionPolicy,
}

struct AcquiredSession<G: Send + 'static> {
    session: Option<PiSession>,
    operation: Option<G>,
    manager: Arc<MultiSessionManagerInner>,
    policy: UnclaimedSessionPolicy,
    runtime: tokio::runtime::Handle,
}

impl<G: Send + 'static> AcquiredSession<G> {
    fn claim(mut self) -> (PiSession, G) {
        (
            self.session
                .take()
                .expect("acquired session must be available until claimed"),
            self.operation
                .take()
                .expect("acquisition guard must be available until claimed"),
        )
    }
}

impl<G: Send + 'static> Drop for AcquiredSession<G> {
    fn drop(&mut self) {
        if !matches!(self.policy, UnclaimedSessionPolicy::Close) {
            return;
        }
        let Some(session) = self.session.take() else {
            return;
        };
        // Release the shared acquisition guard before waiting for exclusive
        // cleanup. The registered path remains owned by the manager meanwhile.
        drop(self.operation.take());
        let manager = Arc::clone(&self.manager);
        drop(self.runtime.spawn(async move {
            let _operation = Arc::clone(&manager.operation_gate).write_owned().await;
            if manager.ensure_managed(&session).is_ok() {
                let _ = manager.close_session_tree_locked(&session).await;
            }
        }));
    }
}

impl<T: Send + 'static> Unpin for CompleteOnDrop<T> {}

impl<T: Send + 'static> Future for CompleteOnDrop<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.polling = true;
        let result = this
            .future
            .as_mut()
            .expect("completed lifecycle future must not be polled again")
            .as_mut()
            .poll(context);
        this.polling = false;
        if result.is_ready() {
            this.future.take();
        }
        result
    }
}

impl<T: Send + 'static> Drop for CompleteOnDrop<T> {
    fn drop(&mut self) {
        // A panic while polling must unwind normally; repolling a panicked
        // future is invalid. Ordinary cancellation happens between polls.
        if self.polling {
            return;
        }
        if let Some(future) = self.future.take() {
            drop(self.runtime.spawn(future));
        }
    }
}

fn complete_on_drop<T>(future: impl Future<Output = T> + Send + 'static) -> CompleteOnDrop<T>
where
    T: Send + 'static,
{
    CompleteOnDrop {
        future: Some(Box::pin(future)),
        runtime: tokio::runtime::Handle::current(),
        polling: false,
    }
}

impl MultiSessionManager {
    pub fn new<F>(factory: F) -> Self
    where
        F: SessionGenerationFactory + 'static,
    {
        Self {
            inner: Arc::new(MultiSessionManagerInner {
                factory: Arc::new(factory),
                sessions: Mutex::new(HashMap::new()),
                isolated_sessions: IsolatedSessionRegistry::default(),
                operation_gate: Arc::new(tokio::sync::RwLock::new(())),
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

    /// Creates a session from a resolved environment, with optional product metadata.
    pub async fn create_session_with_workspace(
        &self,
        workspace: pi_core::WorkspaceSpec,
        path: impl Into<PathBuf>,
        session_id: Option<String>,
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<PiSession, MultiSessionManagerError> {
        let mut target = AgentSessionRuntimeTarget::create(workspace.cwd(), path)
            .with_workspace(workspace, metadata);
        if let AgentSessionRuntimeTarget::Create { session_id: id, .. } = &mut target {
            *id = session_id;
        }
        self.acquire(
            target,
            ExistingSessionPolicy::Reject,
            SessionGenerationOverlay::default(),
        )
        .await
    }

    pub async fn create_session_with_id(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            AgentSessionRuntimeTarget::create_with_id(cwd, path, session_id),
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

    /// Opens a session from a journal that has already been replayed and
    /// validated. Product frontends use this when persisted metadata is needed
    /// before generation construction, so the JSONL file is not parsed twice.
    pub async fn open_session_from_log(
        &self,
        log: SessionLog,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            AgentSessionRuntimeTarget::Reuse { log },
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
        let _operation = self.inner.operation_gate.write().await;
        self.inner.ensure_open()?;
        self.inner.close_session_tree_locked(session).await
    }

    pub async fn shutdown(&self) -> Result<(), MultiSessionManagerError> {
        let _operation = self.inner.operation_gate.write().await;
        // Drain on repeated calls too: a previously cancelled shutdown future
        // must leave the registry's task ownership available for the next caller.
        self.inner.closed.store(true, Ordering::Release);
        self.inner.isolated_sessions.drain_all().await;
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
        let manager = Arc::clone(&self.inner);
        let operation = Arc::clone(&manager.operation_gate).write_owned().await;
        manager.ensure_open()?;
        let acquired = manager
            .acquire_with_guard(
                operation,
                SessionAcquisition {
                    target,
                    existing,
                    generation_overlay,
                    initial_state: None,
                    initial_context: None,
                    unclaimed: UnclaimedSessionPolicy::Retain,
                },
            )
            .await?;
        let (session, _operation) = acquired.claim();
        Ok(session)
    }
}

impl MultiSessionManagerInner {
    /// Once the lifecycle guard has been acquired, construction, activation,
    /// and registration finish together even if the requesting future is
    /// dropped. The returned guard lets isolated-session callers retain their
    /// shared exclusion through launch readiness.
    async fn acquire_with_guard<G>(
        self: Arc<Self>,
        operation: G,
        acquisition: SessionAcquisition,
    ) -> Result<AcquiredSession<G>, MultiSessionManagerError>
    where
        G: Send + 'static,
    {
        complete_on_drop(async move {
            let SessionAcquisition {
                target,
                existing,
                generation_overlay,
                initial_state,
                initial_context,
                unclaimed,
            } = acquisition;
            let manager = Arc::clone(&self);
            let session = self
                .acquire_locked(
                    target,
                    existing,
                    generation_overlay,
                    initial_state,
                    initial_context,
                )
                .await?;
            Ok(AcquiredSession {
                session: Some(session),
                operation: Some(operation),
                manager,
                policy: unclaimed,
                runtime: tokio::runtime::Handle::current(),
            })
        })
        .await
    }

    async fn acquire_locked(
        self: &Arc<Self>,
        target: AgentSessionRuntimeTarget,
        existing: ExistingSessionPolicy,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
        initial_context: Option<crate::isolated_context::IsolatedContextSeed>,
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
            Arc::clone(&self.factory),
            target,
            generation_overlay,
            initial_state,
            initial_context,
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
                .owned_sessions(session.registration_id());
            pending.extend(children);
            sessions.push(session);
        }
        self.isolated_sessions.drain_sessions(&sessions).await;
        for session in &sessions {
            self.isolated_sessions
                .remove_session(session.registration_id());
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(session.registration_id());
        }
        let mut first_error = None;
        for session in sessions.into_iter().rev() {
            if let Err(error) = session.runtime.shutdown().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), |error| Err(error.into()))
    }
}

impl ManagedSessionReplacement {
    async fn begin(owner: PiSession) -> Result<Self, MultiSessionManagerError> {
        let manager = owner.manager()?;
        let operation = Arc::clone(&manager.operation_gate).write_owned().await;
        manager.ensure_open()?;
        manager.ensure_managed(&owner)?;
        Ok(Self {
            owner,
            manager,
            _operation: operation,
        })
    }

    /// Dropping the caller's future detaches the unfinished transaction with
    /// its owned path guard through the last lifecycle await and publication.
    async fn run(
        self,
        request: SessionReplacementRequest,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        complete_on_drop(self.execute(request)).await
    }

    async fn execute(
        self,
        request: SessionReplacementRequest,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let transition = request.resolve(&self.owner, &self.manager)?;
        let replacement = self.owner.runtime.transition(transition).await?;
        if replacement == AgentSessionReplacement::Replaced {
            self.manager.factory.session_registered(&self.owner);
        }
        Ok(replacement)
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
        // Isolated paths are UUID-derived, so child preparations may run in
        // parallel while session replacement, close, and shutdown remain
        // excluded by the write side of this gate.
        let operation = Arc::clone(&manager.operation_gate).read_owned().await;
        manager.ensure_open()?;
        if !manager
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(self.registration_id())
        {
            return Err(MultiSessionManagerError::UnknownSession);
        }
        let parent = self.current();
        let initial_context = match request.options.context {
            pi_plugin::IsolatedContextMode::Fresh => {
                if request.options.fork_point.is_some() || request.options.fork_turns.is_some() {
                    return Err(MultiSessionManagerError::InvalidIsolatedRequest(
                        "fresh context cannot use an isolated fork point or fork turn limit".into(),
                    ));
                }
                None
            }
            pi_plugin::IsolatedContextMode::Fork => {
                if request.options.fork_turns == Some(0) {
                    return Err(MultiSessionManagerError::InvalidIsolatedRequest(
                        "fork turn limit must be positive".into(),
                    ));
                }
                if !parent.log().is_materialized() {
                    return Err(MultiSessionManagerError::InvalidIsolatedRequest(
                        "cannot fork an unsaved session; wait for the first assistant response or use fresh context".to_string(),
                    ));
                }
                let seed = match &request.options.fork_point {
                    Some(fork_point) => {
                        parent.isolated_context_seed_at(fork_point, request.options.fork_turns)
                    }
                    None => parent.isolated_context_seed(request.options.fork_turns),
                };
                Some(seed.map_err(|error| {
                    MultiSessionManagerError::InvalidIsolatedRequest(error.to_string())
                })?)
            }
        };
        let fresh_context = initial_context.is_none();
        let initial_state = resolve_isolated_initial_state(&parent, request.options)?;
        let path = isolated_session_path(&self.path());
        let acquired = Arc::clone(&manager)
            .acquire_with_guard(
                operation,
                SessionAcquisition {
                    target: AgentSessionRuntimeTarget::create(self.cwd(), path).with_workspace(
                        parent.runtime().workspace().spec().clone(),
                        parent.log().header().metadata,
                    ),
                    existing: ExistingSessionPolicy::Reject,
                    generation_overlay: SessionGenerationOverlay::default()
                        .with_execution_origin(pi_plugin::SessionExecutionOrigin::Subagent),
                    initial_state: Some(initial_state),
                    initial_context,
                    unclaimed: UnclaimedSessionPolicy::Close,
                },
            )
            .await?;
        let (child, _operation) = acquired.claim();
        if fresh_context {
            let origin = crate::SessionEntry::Custom(crate::CustomEntry {
                custom_type: crate::isolated_context::ORIGIN_CUSTOM_TYPE.into(),
                data: Some(serde_json::json!({ "parentSessionId": parent.log().header().id })),
            });
            if let Err(error) = child.current().log().append_session_record(origin) {
                manager.close_session_tree_locked(&child).await?;
                return Err(MultiSessionManagerError::InvalidIsolatedRequest(
                    error.to_string(),
                ));
            }
        }
        Ok(manager
            .isolated_sessions
            .launch(self.registration_id().to_owned(), child, request.input)
            .await)
    }

    /// Reattaches one persisted direct child as an idle isolated session.
    ///
    /// This never starts or replays a model turn. The target must live in this
    /// owner's isolated-session directory and carry durable provenance naming
    /// the current owner session.
    pub async fn restore_isolated_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<IsolatedSessionId, MultiSessionManagerError> {
        let path = path.into();
        let expected_directory = comparable_path(&isolated_session_directory(&self.path()));
        let comparable = comparable_path(&path);
        if comparable.parent() != Some(expected_directory.as_path()) {
            return Err(MultiSessionManagerError::InvalidIsolatedRequest(format!(
                "restored child path must be directly inside {}",
                expected_directory.display()
            )));
        }

        let manager = self.manager()?;
        let operation = Arc::clone(&manager.operation_gate).read_owned().await;
        manager.ensure_open()?;
        manager.ensure_managed(self)?;
        let parent_session_id = self.id();

        if let Some(active) = manager.session_at_path(&comparable) {
            validate_isolated_parent(&active, &parent_session_id)?;
            return manager
                .isolated_sessions
                .restored_id(self.registration_id(), &active)
                .ok_or(MultiSessionManagerError::SessionAlreadyActive(comparable));
        }

        let acquired = Arc::clone(&manager)
            .acquire_with_guard(
                operation,
                SessionAcquisition {
                    target: AgentSessionRuntimeTarget::open(&path),
                    existing: ExistingSessionPolicy::Reject,
                    generation_overlay: SessionGenerationOverlay::default()
                        .with_execution_origin(pi_plugin::SessionExecutionOrigin::Subagent),
                    initial_state: None,
                    initial_context: None,
                    unclaimed: UnclaimedSessionPolicy::Close,
                },
            )
            .await?;
        let (child, _operation) = acquired.claim();
        if let Err(error) = validate_isolated_parent(&child, &parent_session_id) {
            manager.close_session_tree_locked(&child).await?;
            return Err(error);
        }
        Ok(manager
            .isolated_sessions
            .restore(self.registration_id().to_owned(), child))
    }

    pub async fn wait_for_isolated_session(
        &self,
        id: &IsolatedSessionId,
    ) -> Result<IsolatedSessionOutcome, PluginContextError> {
        self.isolated_session_waiter(id)?.await
    }

    pub(crate) fn isolated_session_waiter(
        &self,
        id: &IsolatedSessionId,
    ) -> Result<
        impl Future<Output = Result<IsolatedSessionOutcome, PluginContextError>>
        + Send
        + 'static
        + use<>,
        PluginContextError,
    > {
        let waiting = self
            .manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .wait(self.registration_id(), id);
        Ok(async move { waiting.await.map_err(PluginContextError::Failed) })
    }

    pub(crate) fn isolated_session_turn_waiter(
        &self,
        id: &IsolatedSessionId,
        turn_id: &IsolatedSessionTurnId,
    ) -> Result<
        impl Future<Output = Result<IsolatedSessionOutcome, PluginContextError>>
        + Send
        + 'static
        + use<>,
        PluginContextError,
    > {
        let waiting = self
            .manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .wait_turn(self.registration_id(), id, turn_id);
        Ok(async move { waiting.await.map_err(PluginContextError::Failed) })
    }

    pub fn send_to_isolated_session(
        &self,
        id: &IsolatedSessionId,
        content: CustomMessageContent,
    ) -> Result<IsolatedMessageReceipt, PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .send_message(self.registration_id(), id, content)
            .map_err(PluginContextError::Failed)
    }

    /// Delivers a typed custom message to an owned isolated session without
    /// starting a new turn. This is used by first-party session features that
    /// need durable projection metadata in addition to visible content.
    #[doc(hidden)]
    pub fn send_custom_to_isolated_session(
        &self,
        id: &IsolatedSessionId,
        message: CustomMessageInput,
    ) -> Result<IsolatedMessageReceipt, PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .send_custom_message(self.registration_id(), id, message)
            .map_err(PluginContextError::Failed)
    }

    /// Delivers a typed custom message to this session's active run, or starts
    /// a new run when it is idle.
    #[doc(hidden)]
    pub fn send_custom_message(
        &self,
        message: CustomMessageInput,
    ) -> Result<IsolatedMessageReceipt, PluginContextError> {
        let session = self.current();
        let message = Message::custom(message.into_message(crate::now_ms()));
        if session.runtime().agent().is_running() {
            match session.enqueue_message(message.clone(), crate::QueueKind::Steer) {
                Ok(crate::SubmitOutcome::Queued { .. }) => {
                    return Ok(IsolatedMessageReceipt {
                        accepted_as: pi_plugin::IsolatedMessageDelivery::Steer,
                        turn_id: None,
                    });
                }
                Ok(_) | Err(SessionError::Busy) => {}
                Err(error) => return Err(PluginContextError::Failed(error.to_string())),
            }
        }
        tokio::spawn(async move {
            let _ = session.prompt(vec![message]).await;
        });
        Ok(IsolatedMessageReceipt {
            accepted_as: pi_plugin::IsolatedMessageDelivery::Mailbox,
            turn_id: None,
        })
    }

    pub async fn follow_up_isolated_session(
        &self,
        id: &IsolatedSessionId,
        content: CustomMessageContent,
    ) -> Result<IsolatedFollowUpReceipt, PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .follow_up(self.registration_id(), id, content)
            .await
            .map_err(PluginContextError::Failed)
    }

    pub fn abort_isolated_session_turn(
        &self,
        id: &IsolatedSessionId,
        turn_id: &IsolatedSessionTurnId,
    ) -> Result<(), PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .abort_turn(self.registration_id(), id, turn_id)
            .map_err(PluginContextError::Failed)
    }

    pub fn abort_isolated_session(&self, id: &IsolatedSessionId) -> Result<(), PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .abort(self.registration_id(), id)
            .map_err(PluginContextError::Failed)
    }

    /// Returns a read-only observation handle for a live isolated child.
    ///
    /// Ownership is checked against this session just like wait and abort.
    /// The child remains lifecycle-owned by the multi-session manager.
    pub fn observe_isolated_session(
        &self,
        id: &IsolatedSessionId,
    ) -> Result<IsolatedSessionObservation, PluginContextError> {
        self.manager()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .isolated_sessions
            .observe(self.registration_id(), id)
            .map_err(PluginContextError::Failed)
    }

    pub async fn new_session(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        self.replace(SessionReplacementRequest::New {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: None,
        })
        .await
    }

    pub async fn new_session_with_parent(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        parent_session: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        self.replace(SessionReplacementRequest::New {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: Some(parent_session.into()),
        })
        .await
    }

    pub async fn resume_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        self.replace(SessionReplacementRequest::Resume { path: path.into() })
            .await
    }

    pub async fn fork_session(
        &self,
        entry_id: impl Into<String>,
        position: ForkPosition,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        self.replace(SessionReplacementRequest::Fork {
            entry_id: entry_id.into(),
            position,
        })
        .await
    }

    pub async fn reload(&self) -> Result<(), MultiSessionManagerError> {
        self.replace(SessionReplacementRequest::Reload).await?;
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

    async fn replace(
        &self,
        request: SessionReplacementRequest,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        ManagedSessionReplacement::begin(self.clone())
            .await?
            .run(request)
            .await
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

fn isolated_session_path(owner: &Path) -> PathBuf {
    isolated_session_directory(owner).join(format!("{}.jsonl", uuid::Uuid::now_v7()))
}

fn isolated_session_directory(owner: &Path) -> PathBuf {
    let stem = owner
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session");
    owner
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(stem)
        .join("isolated")
}

fn validate_isolated_parent(
    child: &PiSession,
    expected_parent_session_id: &str,
) -> Result<(), MultiSessionManagerError> {
    let actual = child
        .current()
        .log()
        .shared_document()?
        .isolated_parent_session_id()?;
    if actual.as_deref() == Some(expected_parent_session_id) {
        Ok(())
    } else {
        Err(MultiSessionManagerError::InvalidIsolatedRequest(format!(
            "restored child belongs to parent {:?}, not {:?}",
            actual.as_deref(),
            expected_parent_session_id
        )))
    }
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

    fn ensure_managed(&self, session: &PiSession) -> Result<(), MultiSessionManagerError> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(session.registration_id())
            .then_some(())
            .ok_or(MultiSessionManagerError::UnknownSession)
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

#[cfg(test)]
mod tests {
    use pi_plugin::Plugin;
    use std::sync::atomic::AtomicUsize;

    use pi_agent::AgentOptions;
    use pi_core::{
        ContentBlock, CustomMessageContent, Message, ModelId, ProviderId, ResponseMetadata,
        StopReason, StreamEvent, Usage,
    };
    use pi_runtime::PiRuntime;
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

    use super::*;
    use crate::{
        AgentSessionOptions, PluginError, SessionPluginContext, SessionShutdownEvent,
        SessionStartEvent,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReplacementPause {
        Prepare,
        Shutdown,
        Start,
    }

    #[derive(Clone, Default)]
    struct ReplacementGates {
        pause: Arc<Mutex<Option<ReplacementPause>>>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        registered: Arc<tokio::sync::Notify>,
        activation_committed: Arc<AtomicBool>,
        shutdowns: Arc<AtomicUsize>,
    }

    impl ReplacementGates {
        fn pause_at(&self, phase: ReplacementPause) {
            *self
                .pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(phase);
        }

        async fn wait_at(&self, phase: ReplacementPause) {
            let paused = *self
                .pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if paused == Some(phase) {
                self.entered.notify_one();
                self.release.notified().await;
            }
        }

        fn resume(&self) {
            *self
                .pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            self.release.notify_one();
        }
    }

    struct GatedLifecyclePlugin(ReplacementGates);

    struct GatedActivation(Arc<AtomicBool>);

    impl SessionGenerationActivation for GatedActivation {
        fn commit(self: Box<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[pi_plugin::plugin]
    impl Plugin for GatedLifecyclePlugin {
        fn id(&self) -> pi_core::PluginId {
            pi_core::PluginId::new("replacement-gate")
        }

        async fn session_start(
            &self,
            _context: &SessionPluginContext,
            _event: &SessionStartEvent,
        ) -> Result<(), PluginError> {
            self.0.wait_at(ReplacementPause::Start).await;
            Ok(())
        }

        async fn session_shutdown(
            &self,
            _context: &SessionPluginContext,
            _event: &SessionShutdownEvent,
        ) -> Result<(), PluginError> {
            self.0.shutdowns.fetch_add(1, Ordering::AcqRel);
            self.0.wait_at(ReplacementPause::Shutdown).await;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct GatedGenerationFactory(ReplacementGates);

    #[async_trait]
    impl SessionGenerationFactory for GatedGenerationFactory {
        fn session_registered(&self, _session: &PiSession) {
            self.0.registered.notify_one();
        }

        async fn prepare_generation(
            &self,
            request: SessionGenerationRequest,
        ) -> Result<PreparedSessionGeneration, SessionError> {
            self.0.wait_at(ReplacementPause::Prepare).await;
            let runtime = request
                .generation_overlay
                .apply_to(PiRuntime::builder())
                .plugin(GatedLifecyclePlugin(self.0.clone()))
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd: request.cwd,
                    ..AgentOptions::default()
                })
                .build()?;
            let options = AgentSessionOptions::default();
            Ok(PreparedSessionGeneration::new(runtime, options)
                .with_activation(GatedActivation(Arc::clone(&self.0.activation_committed))))
        }
    }

    fn test_manager() -> MultiSessionManager {
        test_manager_with_turns([])
    }

    fn text_turn_with_usage(text: &str, total_tokens: u64) -> ScriptedTurn {
        ScriptedTurn::Events(vec![
            StreamEvent::Start {
                metadata: ResponseMetadata::new("scripted".into(), "test".into(), "scripted", 0),
            },
            StreamEvent::TextStart { content_index: 0 },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: text.to_string(),
            },
            StreamEvent::TextEnd {
                content_index: 0,
                text_signature: None,
            },
            StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage {
                    input: total_tokens,
                    total_tokens,
                    ..Usage::default()
                },
            },
        ])
    }

    fn test_manager_with_turns(
        turns: impl IntoIterator<Item = ScriptedTurn>,
    ) -> MultiSessionManager {
        let turns = turns.into_iter().collect::<Vec<_>>();
        MultiSessionManager::new(move |request: SessionGenerationRequest| {
            let turns = turns.clone();
            async move {
                let runtime = request
                    .generation_overlay
                    .apply_to(PiRuntime::builder())
                    .provider_plugin(ScriptedProviderPlugin::scripted(turns))
                    .agent_options(AgentOptions {
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        cwd: request.cwd,
                        ..AgentOptions::default()
                    })
                    .build()?;
                Ok(PreparedSessionGeneration::new(
                    runtime,
                    AgentSessionOptions::default(),
                ))
            }
        })
    }

    #[tokio::test]
    async fn cancelling_launch_before_readiness_stops_the_unclaimed_child() {
        use std::future::Future;
        use std::task::Poll;

        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("owner.jsonl"))
            .await
            .unwrap();
        let child = manager
            .create_session(directory.path(), directory.path().join("child.jsonl"))
            .await
            .unwrap();
        let id = pi_plugin::IsolatedSessionId::new(child.registration_id().to_string());
        let mut launch = Box::pin(manager.inner.isolated_sessions.launch(
            owner.registration_id().to_string(),
            child.clone(),
            CustomMessageContent::Text("wait".into()),
        ));
        std::future::poll_fn(|context| {
            assert!(launch.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(launch);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            owner.wait_for_isolated_session(&id),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(!child.current().runtime().agent().is_running());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fresh_isolated_session_runs_without_replacing_its_owner() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([text_turn_with_usage("isolated answer", 37)]);
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
        assert_eq!(outcome.usage.total_tokens, 37);
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
        let observed = owner.observe_isolated_session(&isolated_id).unwrap();
        let usage = observed.usage_snapshot().unwrap();
        assert_eq!(usage.usage.total_tokens, 37);
        assert!(usage.context_tokens.is_some());
        assert_eq!(
            owner.current().runtime().execution_origin(),
            pi_plugin::SessionExecutionOrigin::User
        );
        assert_eq!(
            child.current().runtime().execution_origin(),
            pi_plugin::SessionExecutionOrigin::Subagent
        );
        child.reload().await.unwrap();
        assert_eq!(
            child.current().runtime().execution_origin(),
            pi_plugin::SessionExecutionOrigin::Subagent
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
            pi_plugin::SessionExecutionOrigin::Subagent
        );
        assert_eq!(
            owner.current().runtime().execution_origin(),
            pi_plugin::SessionExecutionOrigin::User
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
    async fn isolated_session_reuses_identity_across_turns_and_reports_usage_deltas() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([
            text_turn_with_usage("first answer", 11),
            text_turn_with_usage("follow-up answer", 7),
        ]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("primary.jsonl"))
            .await
            .unwrap();
        let id = owner
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "first task".into(),
            )))
            .await
            .unwrap();
        let first = owner.wait_for_isolated_session(&id).await.unwrap();
        assert_eq!(first.usage.total_tokens, 11);

        let message = owner
            .send_to_isolated_session(&id, CustomMessageContent::Text("mailbox context".into()))
            .unwrap();
        assert_eq!(
            message.accepted_as,
            pi_plugin::IsolatedMessageDelivery::Mailbox
        );
        let custom = owner
            .send_custom_to_isolated_session(
                &id,
                CustomMessageInput {
                    custom_type: "agent_message".into(),
                    content: CustomMessageContent::Text("typed mailbox context".into()),
                    display: true,
                    details: Some(serde_json::json!({"sourceRecordId":"event-1"})),
                },
            )
            .unwrap();
        assert_eq!(
            custom.accepted_as,
            pi_plugin::IsolatedMessageDelivery::Mailbox
        );
        let follow_up = owner
            .follow_up_isolated_session(
                &id,
                CustomMessageContent::Text("continue in the same session".into()),
            )
            .await
            .unwrap();
        assert!(follow_up.started);
        assert_ne!(follow_up.turn_id.as_str(), id.as_str());
        let second = owner
            .isolated_session_turn_waiter(&id, &follow_up.turn_id)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(second.session_id, first.session_id);
        assert_eq!(second.usage.total_tokens, 7);
        assert!(second.messages.iter().any(|message| {
            matches!(message, Message::Assistant(assistant)
            if assistant.content.iter().any(|content| {
                matches!(content, ContentBlock::Text(text) if text.text == "follow-up answer")
            }))
        }));
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == id.as_str())
            .unwrap();
        let document = child.current().log().load().unwrap();
        let context = document.context().unwrap();
        assert!(format!("{:?}", context.messages).contains("mailbox context"));
        let context = format!("{:?}", context.messages);
        assert!(context.contains("typed mailbox context"));
        assert!(context.contains("event-1"));
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
                        ..IsolatedSessionOptions::default()
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
    async fn fork_context_rejects_an_unsaved_parent_before_creating_a_child() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager();
        let owner = manager
            .create_session(directory.path(), directory.path().join("unsaved.jsonl"))
            .await
            .unwrap();
        let error = owner
            .launch_isolated_session(
                IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into()))
                    .options(IsolatedSessionOptions {
                        context: pi_plugin::IsolatedContextMode::Fork,
                        ..Default::default()
                    }),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot fork an unsaved session"));
        assert_eq!(manager.sessions().len(), 1);
        assert!(!owner.path().exists());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fork_turn_limit_keeps_recent_complete_turns_and_rejects_invalid_combinations() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([
            ScriptedTurn::Text("answer one".into()),
            ScriptedTurn::Text("answer two".into()),
            ScriptedTurn::Text("answer three".into()),
            ScriptedTurn::Text("child answer".into()),
        ]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("parent.jsonl"))
            .await
            .unwrap();
        owner.current().prompt("user one").await.unwrap();
        owner.current().prompt("user two").await.unwrap();
        owner.current().prompt("user three").await.unwrap();

        let id = owner
            .launch_isolated_session(
                IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into()))
                    .options(IsolatedSessionOptions {
                        context: pi_plugin::IsolatedContextMode::Fork,
                        fork_turns: Some(2),
                        ..Default::default()
                    }),
            )
            .await
            .unwrap();
        owner.wait_for_isolated_session(&id).await.unwrap();
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == id.as_str())
            .unwrap();
        let inherited = child
            .current()
            .log()
            .load()
            .unwrap()
            .inherited_context()
            .unwrap()
            .unwrap();
        let inherited = serde_json::to_string(&inherited.messages).unwrap();
        assert!(!inherited.contains("user one"));
        assert!(!inherited.contains("answer one"));
        assert!(inherited.contains("user two"));
        assert!(inherited.contains("answer two"));
        assert!(inherited.contains("user three"));
        assert!(inherited.contains("answer three"));

        let before_count = manager.sessions().len();
        for options in [
            IsolatedSessionOptions {
                context: pi_plugin::IsolatedContextMode::Fresh,
                fork_turns: Some(1),
                ..Default::default()
            },
            IsolatedSessionOptions {
                context: pi_plugin::IsolatedContextMode::Fork,
                fork_turns: Some(0),
                ..Default::default()
            },
        ] {
            assert!(
                owner
                    .launch_isolated_session(
                        IsolatedSessionRequest::new(CustomMessageContent::Text("invalid".into()))
                            .options(options)
                    )
                    .await
                    .is_err()
            );
            assert_eq!(manager.sessions().len(), before_count);
        }
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fork_point_excludes_later_compaction_and_rejects_invalid_sources() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::Text("answer".into())]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("parent.jsonl"))
            .await
            .unwrap();
        assert!(owner.current().isolated_fork_point().unwrap().is_none());
        owner.current().prompt("original history").await.unwrap();
        let fork_point = owner.current().isolated_fork_point().unwrap().unwrap();
        let expected = owner
            .current()
            .log()
            .load()
            .unwrap()
            .context()
            .unwrap()
            .messages;
        owner
            .current()
            .log()
            .append_session_record(crate::SessionEntry::Compaction(crate::CompactionEntry {
                summary: "later context must not leak".into(),
                retained_tail: vec![],
                tokens_before: 10000,
                details: None,
                usage: None,
            }))
            .unwrap();
        let id = owner
            .launch_isolated_session(
                IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into()))
                    .options(IsolatedSessionOptions {
                        context: pi_plugin::IsolatedContextMode::Fork,
                        fork_point: Some(fork_point.clone()),
                        ..Default::default()
                    }),
            )
            .await
            .unwrap();
        owner.wait_for_isolated_session(&id).await.unwrap();
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == id.as_str())
            .unwrap();
        let document = child.current().log().load().unwrap();
        let inherited = document.inherited_context().unwrap().unwrap();
        assert_eq!(inherited.parent_session_id, owner.id());
        assert_eq!(
            inherited.parent_entry_id.as_deref(),
            Some(fork_point.parent_entry_id.as_str())
        );
        assert_eq!(inherited.messages, expected);
        assert!(
            document
                .entries
                .iter()
                .any(|record| record.id == inherited.snapshot_entry_id)
        );
        assert_eq!(
            serde_json::to_value(&document.context().unwrap().messages[..expected.len()]).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let before_count = manager.sessions().len();
        for (mode, source) in [
            (pi_plugin::IsolatedContextMode::Fresh, fork_point.clone()),
            (
                pi_plugin::IsolatedContextMode::Fork,
                pi_plugin::IsolatedForkPoint {
                    parent_session_id: "foreign".into(),
                    ..fork_point.clone()
                },
            ),
            (
                pi_plugin::IsolatedContextMode::Fork,
                pi_plugin::IsolatedForkPoint {
                    parent_entry_id: "missing".into(),
                    ..fork_point
                },
            ),
        ] {
            assert!(
                owner
                    .launch_isolated_session(
                        IsolatedSessionRequest::new(CustomMessageContent::Text("invalid".into()))
                            .options(IsolatedSessionOptions {
                                context: mode,
                                fork_point: Some(source),
                                ..Default::default()
                            })
                    )
                    .await
                    .is_err()
            );
            assert_eq!(manager.sessions().len(), before_count);
        }
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn forked_context_survives_reload_without_importing_configuration_or_usage() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager_with_turns([ScriptedTurn::Text("answer".into())]);
        let owner = manager
            .create_session(directory.path(), directory.path().join("parent.jsonl"))
            .await
            .unwrap();
        owner.current().prompt("parent history").await.unwrap();
        let extension = crate::AgentMessage::custom(serde_json::json!({
            "role": "user", "content": [{"type": "text", "text": "retained user"},
                {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}],
            "timestamp": 2, "futureExtension": {"preserve": true}
        }))
        .unwrap();
        owner
            .current()
            .log()
            .append_session_record(crate::SessionEntry::Compaction(crate::CompactionEntry {
                summary: "parent compacted summary".into(),
                retained_tail: vec![extension.clone()],
                tokens_before: 10000,
                details: None,
                usage: None,
            }))
            .unwrap();
        let before = owner.current().log().load().unwrap();
        let expected = before.context().unwrap().messages;
        let id = owner
            .launch_isolated_session(
                IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into()))
                    .options(IsolatedSessionOptions {
                        context: pi_plugin::IsolatedContextMode::Fork,
                        active_tools: Some(Vec::new()),
                        ..Default::default()
                    }),
            )
            .await
            .unwrap();
        let outcome = owner.wait_for_isolated_session(&id).await.unwrap();
        assert_eq!(outcome.messages.len(), 2);
        let child = manager
            .sessions()
            .into_iter()
            .find(|session| session.registration_id() == id.as_str())
            .unwrap();
        let document = child.current().log().load().unwrap();
        assert_eq!(
            serde_json::to_value(&document.context().unwrap().messages[..expected.len()]).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(document.messages().len(), 2);
        let seed_entry = document.entries.iter().find(|record| matches!(&record.entry, crate::SessionEntry::Custom(custom) if custom.custom_type == crate::isolated_context::CUSTOM_TYPE)).unwrap();
        assert!(crate::session_entry_usage(&seed_entry.entry).is_none());
        let child_history = child.current().runtime().agent().state().messages;
        child.reload().await.unwrap();
        assert_eq!(
            child.current().runtime().agent().state().messages,
            child_history
        );
        assert!(
            child
                .current()
                .runtime()
                .agent()
                .state()
                .active_tools
                .is_empty()
        );
        assert_eq!(
            owner.current().log().load().unwrap().entries,
            before.entries
        );
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

        let observation = owner.observe_isolated_session(&isolated_id).unwrap();
        assert_eq!(observation.isolated_id(), &isolated_id);
        assert_eq!(observation.cwd(), owner.cwd());
        assert!(!observation.session_id().is_empty());
        assert!(observation.snapshot().agent.is_running);
        let subscription = observation.subscribe();
        assert!(subscription.snapshot.revision <= observation.snapshot().revision);

        assert!(other.abort_isolated_session(&isolated_id).is_err());
        assert!(other.observe_isolated_session(&isolated_id).is_err());
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
        assert!(matches!(
            first.reload().await,
            Err(MultiSessionManagerError::UnknownSession)
        ));
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
    async fn opening_a_preloaded_log_preserves_the_replayed_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(
            &path,
            crate::SessionHeader::new("preloaded-session", directory.path()),
        )
        .unwrap();
        let entry_id = log
            .append_custom_entry("preloaded", Some(serde_json::json!({ "ready": true })))
            .unwrap();
        drop(log);

        let replayed = SessionLog::open_handle(&path).unwrap();
        let manager = test_manager();
        let opened = manager.open_session_from_log(replayed).await.unwrap();

        assert_eq!(opened.id(), "preloaded-session");
        assert_eq!(opened.path(), path);
        let document = opened.current().log().load().unwrap();
        assert!(document.entries.iter().any(|entry| entry.id == entry_id));
        manager.shutdown().await.unwrap();
    }

    async fn assert_cancelled_managed_replacement_finishes(phase: ReplacementPause) {
        let directory = tempfile::tempdir().unwrap();
        let gates = ReplacementGates::default();
        let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
        let original_path = directory.path().join("original.jsonl");
        let replacement_path = directory.path().join("replacement.jsonl");
        let owner = manager
            .create_session(directory.path(), &original_path)
            .await
            .unwrap();
        let original = owner.current();
        let mut replacements = owner.subscribe();

        gates.pause_at(phase);
        let replacement = tokio::spawn({
            let owner = owner.clone();
            let cwd = directory.path().to_path_buf();
            let path = replacement_path.clone();
            async move { owner.new_session(cwd, path).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
            .await
            .expect("replacement did not reach the requested pause");

        replacement.abort();
        assert!(replacement.await.unwrap_err().is_cancelled());
        assert_eq!(owner.path(), original_path);

        let mut contender = tokio::spawn({
            let manager = manager.clone();
            let cwd = directory.path().to_path_buf();
            let path = replacement_path.clone();
            async move { manager.create_session(cwd, path).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut contender)
                .await
                .is_err(),
            "the replacement must retain the manager path guard after caller cancellation"
        );

        gates.resume();
        tokio::time::timeout(std::time::Duration::from_secs(2), replacements.changed())
            .await
            .expect("detached replacement did not publish")
            .unwrap();
        assert_eq!(owner.path(), replacement_path);
        assert!(original.is_closed());
        assert!(!owner.current().is_closed());

        let contender_result = tokio::time::timeout(std::time::Duration::from_secs(2), contender)
            .await
            .expect("path contender remained blocked after publication")
            .unwrap();
        let error = match contender_result {
            Ok(_) => panic!("path contender unexpectedly acquired the replacement path"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            MultiSessionManagerError::SessionAlreadyActive(_)
        ));
        assert_eq!(manager.sessions().len(), 1);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn manager_gate_serializes_replacements_before_runtime_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let gates = ReplacementGates::default();
        let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
        let original_path = directory.path().join("original.jsonl");
        let replacement_path = directory.path().join("replacement.jsonl");
        let owner = manager
            .create_session(directory.path(), &original_path)
            .await
            .unwrap();

        gates.pause_at(ReplacementPause::Prepare);
        let first = tokio::spawn({
            let owner = owner.clone();
            let cwd = directory.path().to_path_buf();
            let path = replacement_path.clone();
            async move { owner.new_session(cwd, path).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
            .await
            .expect("first replacement did not enter preparation");

        let mut second = tokio::spawn({
            let owner = owner.clone();
            async move { owner.reload().await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "a second replacement must wait before capturing the runtime session"
        );

        gates.resume();
        assert_eq!(
            first.await.unwrap().unwrap(),
            AgentSessionReplacement::Replaced
        );
        second.await.unwrap().unwrap();
        assert_eq!(owner.path(), replacement_path);
        assert!(!owner.current().is_closed());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_managed_replacement_during_prepare_still_publishes_it() {
        assert_cancelled_managed_replacement_finishes(ReplacementPause::Prepare).await;
    }

    #[tokio::test]
    async fn cancelling_managed_replacement_during_shutdown_still_publishes_it() {
        assert_cancelled_managed_replacement_finishes(ReplacementPause::Shutdown).await;
    }

    #[tokio::test]
    async fn cancelling_managed_replacement_during_start_still_publishes_it() {
        assert_cancelled_managed_replacement_finishes(ReplacementPause::Start).await;
    }

    #[tokio::test]
    async fn cancelling_managed_acquisition_during_start_still_registers_it() {
        let directory = tempfile::tempdir().unwrap();
        let gates = ReplacementGates::default();
        gates.pause_at(ReplacementPause::Start);
        let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
        let path = directory.path().join("session.jsonl");

        let acquisition = tokio::spawn({
            let manager = manager.clone();
            let cwd = directory.path().to_path_buf();
            let path = path.clone();
            async move { manager.create_session(cwd, path).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
            .await
            .expect("acquisition did not reach session_start");
        assert!(gates.activation_committed.load(Ordering::Acquire));
        assert!(manager.sessions().is_empty());

        acquisition.abort();
        let cancellation = match acquisition.await {
            Err(error) => error,
            Ok(_) => panic!("acquisition caller unexpectedly completed"),
        };
        assert!(cancellation.is_cancelled());

        let mut contender = tokio::spawn({
            let manager = manager.clone();
            let cwd = directory.path().to_path_buf();
            let path = path.clone();
            async move { manager.create_session(cwd, path).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut contender)
                .await
                .is_err(),
            "the acquisition must retain the manager lifecycle guard after caller cancellation"
        );

        gates.resume();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gates.registered.notified(),
        )
        .await
        .expect("detached acquisition did not register");
        let sessions = manager.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].path(), path);
        assert!(!sessions[0].current().is_closed());

        let contender_result = tokio::time::timeout(std::time::Duration::from_secs(2), contender)
            .await
            .expect("path contender remained blocked after registration")
            .unwrap();
        let error = match contender_result {
            Ok(_) => panic!("path contender unexpectedly acquired the registered path"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            MultiSessionManagerError::SessionAlreadyActive(_)
        ));
        manager.shutdown().await.unwrap();
    }

    async fn assert_cancelled_isolated_acquisition_closes_unclaimed_child(phase: ReplacementPause) {
        let directory = tempfile::tempdir().unwrap();
        let gates = ReplacementGates::default();
        let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
        let owner = manager
            .create_session(directory.path(), directory.path().join("owner.jsonl"))
            .await
            .unwrap();
        // Consume the owner's registration notification so the next one
        // belongs to the candidate child.
        gates.registered.notified().await;
        gates.activation_committed.store(false, Ordering::Release);
        gates.pause_at(phase);

        let launch = tokio::spawn({
            let owner = owner.clone();
            async move {
                owner
                    .launch_isolated_session(IsolatedSessionRequest::new(
                        CustomMessageContent::Text("inspect".to_string()),
                    ))
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
            .await
            .expect("isolated acquisition did not reach the requested pause");
        assert_eq!(
            gates.activation_committed.load(Ordering::Acquire),
            phase == ReplacementPause::Start
        );
        assert_eq!(manager.sessions().len(), 1);

        launch.abort();
        assert!(launch.await.unwrap_err().is_cancelled());
        gates.resume();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gates.registered.notified(),
        )
        .await
        .expect("detached child acquisition did not register before cleanup");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while gates.shutdowns.load(Ordering::Acquire) != 1 || manager.sessions().len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("unclaimed isolated child was not closed");
        assert_eq!(
            manager.sessions()[0].registration_id(),
            owner.registration_id()
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_isolated_acquisition_during_prepare_closes_the_unclaimed_child() {
        assert_cancelled_isolated_acquisition_closes_unclaimed_child(ReplacementPause::Prepare)
            .await;
    }

    #[tokio::test]
    async fn cancelling_isolated_acquisition_during_start_closes_the_unclaimed_child() {
        assert_cancelled_isolated_acquisition_closes_unclaimed_child(ReplacementPause::Start).await;
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
