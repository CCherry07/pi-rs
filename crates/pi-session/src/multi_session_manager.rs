use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use tokio::sync::watch;

use crate::{
    AgentSession, AgentSessionReplacement, AgentSessionRuntime, AgentSessionRuntimeError,
    AgentSessionRuntimeFactory, AgentSessionRuntimeRequest, AgentSessionRuntimeTarget,
    ForkPosition, PreparedAgentSession, SessionError,
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
        let removed = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session.registration_id.as_ref())
            .ok_or(MultiSessionManagerError::UnknownSession)?;
        removed.runtime.shutdown().await?;
        Ok(())
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
    ) -> Result<PiSession, MultiSessionManagerError> {
        let _operation = self.inner.operation_gate.lock().await;
        self.inner.ensure_open()?;
        let path = comparable_path(target.path());
        if let Some(active) = self.inner.session_at_path(&path) {
            return match existing {
                ExistingSessionPolicy::Reuse => Ok(active),
                ExistingSessionPolicy::Reject => {
                    Err(MultiSessionManagerError::SessionAlreadyActive(path))
                }
            };
        }
        let runtime =
            AgentSessionRuntime::create(SharedFactory(Arc::clone(&self.inner.factory)), target)
                .await?;
        let registration_id: Arc<str> = Arc::from(uuid::Uuid::now_v7().to_string());
        let session = PiSession {
            registration_id: Arc::clone(&registration_id),
            runtime,
            manager: Arc::downgrade(&self.inner),
        };
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(registration_id.to_string(), session.clone());
        Ok(session)
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

    /// Imports a v4 JSONL file into the current session directory and switches
    /// this handle to the imported copy. Import uses resume lifecycle events.
    pub async fn import_session(
        &self,
        source: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, MultiSessionManagerError> {
        let source = source.into();
        let file_name = source
            .file_name()
            .ok_or_else(|| MultiSessionManagerError::InvalidImportPath(source.clone()))?;
        let current_path = self.path();
        let destination = current_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(file_name);
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
    use pi_core::{ModelId, ProviderId};
    use pi_runtime::PiRuntime;
    use pi_test_support::ScriptedProviderPlugin;

    use super::*;
    use crate::AgentSessionOptions;

    fn test_manager() -> MultiSessionManager {
        MultiSessionManager::new(|request: AgentSessionRuntimeRequest| async move {
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
            let runtime = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd,
                    ..AgentOptions::default()
                })
                .build()?;
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
        })
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
