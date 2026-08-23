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

/// The application-level owner of active Pi sessions.
///
/// The active-session table is intentionally private. Frontends keep the
/// returned [`PiSession`] handles and do not coordinate a separate registry.
#[derive(Clone)]
pub struct PiApplication {
    inner: Arc<ApplicationInner>,
}

struct ApplicationInner {
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
    application: Weak<ApplicationInner>,
}

#[derive(Debug, thiserror::Error)]
pub enum PiApplicationError {
    #[error(transparent)]
    Runtime(#[from] AgentSessionRuntimeError),
    #[error("Pi application is closed")]
    Closed,
    #[error("session is not managed by this Pi application")]
    UnknownSession,
    #[error("session path is already active: {0}")]
    SessionAlreadyActive(PathBuf),
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

impl PiApplication {
    pub fn new<F>(factory: F) -> Self
    where
        F: AgentSessionRuntimeFactory + 'static,
    {
        Self {
            inner: Arc::new(ApplicationInner {
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
    ) -> Result<PiSession, PiApplicationError> {
        self.acquire(
            AgentSessionRuntimeTarget::create(cwd, path),
            ExistingSessionPolicy::Reject,
        )
        .await
    }

    pub async fn open_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<PiSession, PiApplicationError> {
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

    pub async fn close_session(&self, session: &PiSession) -> Result<(), PiApplicationError> {
        let _operation = self.inner.operation_gate.lock().await;
        self.inner.ensure_open()?;
        let removed = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session.registration_id.as_ref())
            .ok_or(PiApplicationError::UnknownSession)?;
        removed.runtime.shutdown().await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), PiApplicationError> {
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
    ) -> Result<PiSession, PiApplicationError> {
        let _operation = self.inner.operation_gate.lock().await;
        self.inner.ensure_open()?;
        let path = comparable_path(target.path());
        if let Some(active) = self.inner.session_at_path(&path) {
            return match existing {
                ExistingSessionPolicy::Reuse => Ok(active),
                ExistingSessionPolicy::Reject => {
                    Err(PiApplicationError::SessionAlreadyActive(path))
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
            application: Arc::downgrade(&self.inner),
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
    ) -> Result<AgentSessionReplacement, PiApplicationError> {
        let cwd = cwd.into();
        let path = path.into();
        let application = self.application()?;
        let _operation = application.operation_gate.lock().await;
        application.ensure_open()?;
        application.ensure_path_available(self, &path)?;
        Ok(self.runtime.new_session(cwd, path).await?)
    }

    pub async fn resume_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, PiApplicationError> {
        let path = path.into();
        let application = self.application()?;
        let _operation = application.operation_gate.lock().await;
        application.ensure_open()?;
        application.ensure_path_available(self, &path)?;
        Ok(self.runtime.switch_session(path).await?)
    }

    pub async fn fork_session(
        &self,
        entry_id: impl Into<String>,
        position: ForkPosition,
    ) -> Result<AgentSessionReplacement, PiApplicationError> {
        let application = self.application()?;
        let _operation = application.operation_gate.lock().await;
        application.ensure_open()?;
        Ok(self.runtime.fork_session(entry_id, position).await?)
    }

    pub async fn reload(&self) -> Result<(), PiApplicationError> {
        let application = self.application()?;
        let _operation = application.operation_gate.lock().await;
        application.ensure_open()?;
        self.runtime.reload().await?;
        Ok(())
    }

    pub fn abort(&self) {
        self.runtime.abort();
    }

    fn application(&self) -> Result<Arc<ApplicationInner>, PiApplicationError> {
        self.application.upgrade().ok_or(PiApplicationError::Closed)
    }
}

impl ApplicationInner {
    fn ensure_open(&self) -> Result<(), PiApplicationError> {
        if self.closed.load(Ordering::Acquire) {
            Err(PiApplicationError::Closed)
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
    ) -> Result<(), PiApplicationError> {
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
            Err(PiApplicationError::SessionAlreadyActive(path))
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

    fn test_application() -> PiApplication {
        PiApplication::new(|request: AgentSessionRuntimeRequest| async move {
            let (cwd, path, create, reused_log) = match request.target {
                AgentSessionRuntimeTarget::Create { cwd, path } => (cwd, path, true, None),
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
    async fn application_owns_multiple_sessions_and_closes_them() {
        let directory = tempfile::tempdir().unwrap();
        let application = test_application();
        let first = application
            .create_session(directory.path(), directory.path().join("first.jsonl"))
            .await
            .unwrap();
        let second = application
            .create_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap();

        assert_ne!(first.id(), second.id());
        assert_eq!(application.sessions().len(), 2);

        application.close_session(&first).await.unwrap();
        assert!(first.current().is_closed());
        assert!(!second.current().is_closed());
        assert_eq!(application.sessions().len(), 1);

        application.shutdown().await.unwrap();
        assert!(second.current().is_closed());
        assert!(application.sessions().is_empty());
        assert!(matches!(
            application
                .create_session(directory.path(), directory.path().join("third.jsonl"))
                .await,
            Err(PiApplicationError::Closed)
        ));
    }

    #[tokio::test]
    async fn opening_an_active_path_reuses_its_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let application = test_application();
        let created = application
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        created.current().log().materialize().unwrap();

        let opened = application.open_session(&path).await.unwrap();

        assert_eq!(created.registration_id, opened.registration_id);
        assert_eq!(application.sessions().len(), 1);
        application.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn transition_rejects_a_path_owned_by_another_handle() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let second_path = directory.path().join("second.jsonl");
        let application = test_application();
        let first = application
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let _second = application
            .create_session(directory.path(), &second_path)
            .await
            .unwrap();

        let error = first.resume_session(&second_path).await.unwrap_err();

        assert!(matches!(error, PiApplicationError::SessionAlreadyActive(_)));
        assert_eq!(first.path(), first_path);
        application.shutdown().await.unwrap();
    }
}
