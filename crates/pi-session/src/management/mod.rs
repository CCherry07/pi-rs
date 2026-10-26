//! Managed session ownership, replacement and child lifetime.
mod observation;
mod options;
mod paths;
mod replacement;
mod session;
pub use observation::{SessionObservation, SessionResetReason, SessionUpdate, SessionVersion};
pub use options::{CreateSessionOptions, OpenSessionOptions, SessionSource};
mod isolated;
pub use isolated::{IsolatedSessionObservation, IsolatedSessionUsageSnapshot};
pub use replacement::{
    AgentSessionReplacement, PreparedSessionGeneration, SessionGenerationActivation,
    SessionGenerationFactory, SessionGenerationOverlay, SessionGenerationRequest,
};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

use self::paths::comparable_path;
use crate::{AgentSessionInitialState, ForkPosition, SessionError};
use isolated::IsolatedSessionRegistry;
use replacement::{ResolvedSessionTransition, SessionSlot, SessionTarget};

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
    slot: SessionSlot,
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
                    parent_session_id: parent_session
                        .as_deref()
                        .map(|path| manager.resolve_parent_session_id(path))
                        .transpose()?,
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
    target: SessionTarget,
    existing: ExistingSessionPolicy,
    generation_overlay: SessionGenerationOverlay,
    initial_state: Option<AgentSessionInitialState>,
    initial_context: Option<crate::history::IsolatedContextSeed>,
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

    /// Creates and activates a new session, rejecting already-active paths.
    pub async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<PiSession, MultiSessionManagerError> {
        self.acquire(
            SessionTarget::Create {
                workspace: options.workspace.snapshot(),
                metadata: options.metadata,
                path: options.path,
                parent_session_id: None,
                session_id: options.session_id,
            },
            ExistingSessionPolicy::Reject,
            options.generation_overlay,
        )
        .await
    }

    /// Opens a path or preloaded log, reusing its handle if it is already active.
    /// Options apply only when constructing a new handle.
    pub async fn open_session(
        &self,
        source: impl Into<SessionSource>,
        options: OpenSessionOptions,
    ) -> Result<PiSession, MultiSessionManagerError> {
        let target = match source.into() {
            SessionSource::Path(path) => SessionTarget::open(path),
            SessionSource::Store(store) => SessionTarget::Reuse { store },
        };
        self.acquire(
            target,
            ExistingSessionPolicy::Reuse,
            options.generation_overlay,
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
            if let Err(error) = session.slot.shutdown().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), |error| Err(error.into()))
    }

    async fn acquire(
        &self,
        target: SessionTarget,
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
        target: SessionTarget,
        existing: ExistingSessionPolicy,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
        initial_context: Option<crate::history::IsolatedContextSeed>,
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
        let runtime = SessionSlot::create_with_overlay_and_initial_state(
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
            slot: runtime,
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
            if let Err(error) = session.slot.shutdown().await
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
        let replacement = self.owner.slot.transition(transition).await?;
        if replacement == AgentSessionReplacement::Replaced {
            self.manager.factory.session_registered(&self.owner);
        }
        Ok(replacement)
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

    fn resolve_parent_session_id(&self, path: &Path) -> Result<String, MultiSessionManagerError> {
        if let Some(session) = self.session_at_path(&comparable_path(path)) {
            return Ok(session.id());
        }
        Ok(crate::SessionStore::read(path)
            .map_err(SessionError::from)?
            .header
            .id)
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
mod tests;
