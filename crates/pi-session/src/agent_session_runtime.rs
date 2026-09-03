use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use pi_core::{AgentPlugin, ModelSelection, ThinkingLevel};
use pi_runtime::{PiRuntime, PiRuntimeBuilder};
use tokio::sync::{Mutex, watch};

use crate::{
    AgentSession, ForkOptions, ForkPosition, PiSession, PreparedAgentSession,
    SessionBeforeForkEvent, SessionBeforeSwitchEvent, SessionError, SessionHeader, SessionLog,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason,
    SessionSwitchReason, import_session_file,
};

#[derive(Debug, Clone)]
pub enum AgentSessionRuntimeTarget {
    Create {
        cwd: PathBuf,
        path: PathBuf,
        parent_session: Option<PathBuf>,
    },
    Open {
        path: PathBuf,
    },
    Reuse {
        log: SessionLog,
    },
}

impl AgentSessionRuntimeTarget {
    pub fn create(cwd: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self::Create {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: None,
        }
    }

    pub fn create_with_parent(
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        parent_session: impl Into<PathBuf>,
    ) -> Self {
        Self::Create {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: Some(parent_session.into()),
        }
    }

    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self::Open { path: path.into() }
    }

    pub fn reuse_log(log: SessionLog) -> Self {
        Self::Reuse { log }
    }

    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::Create { path, .. } | Self::Open { path } => path,
            Self::Reuse { log } => log.path(),
        }
    }
}

type OverlayAgentPlugin = Arc<dyn Fn() -> Arc<dyn AgentPlugin> + Send + Sync>;

/// Session-local additions layered onto every product runtime generation.
///
/// The overlay is deliberately transient: it survives new/resume/fork/reload
/// replacements on the live handle, but is never serialized into the v4 log.
#[derive(Clone, Default)]
pub struct SessionGenerationOverlay {
    agent_plugins: Vec<OverlayAgentPlugin>,
    execution_origin: pi_core::SessionExecutionOrigin,
}

impl std::fmt::Debug for SessionGenerationOverlay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionGenerationOverlay")
            .field("agent_plugins", &self.agent_plugins.len())
            .field("execution_origin", &self.execution_origin)
            .finish()
    }
}

impl SessionGenerationOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Provenance follows the live handle across replacement and reload, not
    /// the persisted parent-session pointer (a user fork is still user work).
    pub fn with_execution_origin(mut self, origin: pi_core::SessionExecutionOrigin) -> Self {
        self.execution_origin = origin;
        self
    }

    /// Adds one session-local agent plugin that is rebuilt for every runtime generation.
    pub fn with_agent_plugin<F>(mut self, plugin: F) -> Self
    where
        F: Fn() -> Arc<dyn AgentPlugin> + Send + Sync + 'static,
    {
        self.agent_plugins.push(Arc::new(plugin));
        self
    }

    /// Applies the opaque session-local additions to a product runtime builder.
    pub fn apply_to(&self, mut builder: PiRuntimeBuilder) -> PiRuntimeBuilder {
        builder = builder.execution_origin(self.execution_origin);
        for plugin in &self.agent_plugins {
            let plugin = Arc::clone(plugin);
            builder = builder.try_agent_plugin_arc_factory(move || {
                Ok::<Arc<dyn AgentPlugin>, std::convert::Infallible>(plugin())
            });
        }
        builder
    }
}

#[derive(Debug, Clone)]
pub struct AgentSessionRuntimeRequest {
    pub target: AgentSessionRuntimeTarget,
    pub start_event: SessionStartEvent,
    pub generation_overlay: SessionGenerationOverlay,
    /// Complete initial state for a fresh runtime. Product factories must
    /// apply it before preparing the new session so its first persisted
    /// configuration and provider request agree.
    pub initial_state: Option<AgentSessionInitialState>,
}

/// Fully resolved initial runtime state for a fresh agent session.
///
/// This type is product-policy agnostic: isolated-session inheritance and
/// capability ceilings are resolved by the owning multi-session host before
/// the runtime factory receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionInitialState {
    pub model: ModelSelection,
    /// Allows product policy to distinguish an explicit child selection from
    /// faithful inheritance of a model already accepted by the parent.
    pub model_source: AgentSessionInitialModelSource,
    pub thinking_level: ThinkingLevel,
    pub active_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionInitialModelSource {
    Inherited,
    Requested,
}

impl AgentSessionInitialState {
    /// Applies the complete state to a built runtime before session creation.
    pub fn apply_to(&self, runtime: &PiRuntime) -> Result<(), SessionError> {
        if let Some(model) = runtime.model(&self.model.provider, &self.model.model_id)
            && !model.supports_thinking_level(self.thinking_level)
        {
            return Err(SessionError::Runtime(format!(
                "thinking level {} is unsupported by {}/{}",
                self.thinking_level.as_str(),
                self.model.provider,
                self.model.model_id
            )));
        }
        let current = runtime.agent().state();
        if current.provider_id != self.model.provider || current.model_id != self.model.model_id {
            runtime.set_model(self.model.provider.clone(), self.model.model_id.clone())?;
        }
        if current.thinking_level != self.thinking_level {
            runtime.set_thinking_level(self.thinking_level)?;
        }
        if current.active_tools != self.active_tools {
            runtime.set_active_tools(self.active_tools.clone())?;
        }
        Ok(())
    }
}

#[async_trait]
pub trait AgentSessionRuntimeFactory: Send + Sync {
    /// Build the complete next session without emitting `session_start`.
    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError>;

    /// Observe a stable frontend handle after the multi-session manager has
    /// registered it. Product adapters use this lifecycle seam to bind outer
    /// capabilities without requiring callers to perform a second setup step.
    fn session_registered(&self, _session: &PiSession) {}
}

#[async_trait]
impl<F, Fut> AgentSessionRuntimeFactory for F
where
    F: Fn(AgentSessionRuntimeRequest) -> Fut + Send + Sync,
    Fut: Future<Output = Result<PreparedAgentSession, SessionError>> + Send,
{
    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError> {
        self(request).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionReplacement {
    Replaced,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentSessionRuntimeError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("agent session runtime is closed")]
    Closed,
}

/// Owns the replaceable current `AgentSession`.
///
/// Replacement is serialized. The current agent is first settled, then the
/// factory prepares the complete next session. A preparation failure leaves
/// the current session active. A successful transition emits old
/// `session_shutdown`, then new `session_start`, then publishes the new handle
/// to subscribers.
#[derive(Clone)]
pub struct AgentSessionRuntime {
    current: watch::Sender<Arc<AgentSession>>,
    factory: Arc<dyn AgentSessionRuntimeFactory>,
    generation_overlay: SessionGenerationOverlay,
    transition_gate: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
}

struct ImportedFileTransaction {
    destination: PathBuf,
    backup: Option<PathBuf>,
    owns_destination: bool,
    committed: bool,
}

impl ImportedFileTransaction {
    fn stage(source: &Path, destination: &Path) -> Result<Self, SessionError> {
        if comparable_path(source) == comparable_path(destination) {
            SessionLog::open(source)?;
            return Ok(Self {
                destination: destination.to_path_buf(),
                backup: None,
                owns_destination: false,
                committed: false,
            });
        }
        if destination.is_dir() {
            return Err(SessionError::Storage(format!(
                "import destination is a directory: {}",
                destination.display()
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let temporary = sibling_transaction_path(destination, "import");
        let backup = destination
            .exists()
            .then(|| sibling_transaction_path(destination, "backup"));
        let staged = (|| {
            // Copy current v4 files or migrate coding-agent v1-v3 into the
            // staging path. Validation and torn-tail repair apply only to the
            // staged file; the user's source is never mutated by import.
            import_session_file(source, &temporary)?;
            if let Some(backup) = &backup {
                std::fs::rename(destination, backup)?;
            }
            if let Err(error) = std::fs::rename(&temporary, destination) {
                if let Some(backup) = &backup {
                    let _ = std::fs::rename(backup, destination);
                }
                return Err(error.into());
            }
            Ok::<(), SessionError>(())
        })();
        if staged.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        staged?;
        Ok(Self {
            destination: destination.to_path_buf(),
            backup,
            owns_destination: true,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
        if let Some(backup) = &self.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
}

impl Drop for ImportedFileTransaction {
    fn drop(&mut self) {
        if self.committed || !self.owns_destination {
            return;
        }
        let _ = std::fs::remove_file(&self.destination);
        if let Some(backup) = &self.backup {
            let _ = std::fs::rename(backup, &self.destination);
        }
    }
}

impl AgentSessionRuntime {
    pub async fn create<F>(
        factory: F,
        initial_target: AgentSessionRuntimeTarget,
    ) -> Result<Self, AgentSessionRuntimeError>
    where
        F: AgentSessionRuntimeFactory + 'static,
    {
        Self::create_with_overlay(factory, initial_target, SessionGenerationOverlay::default())
            .await
    }

    pub async fn create_with_overlay<F>(
        factory: F,
        initial_target: AgentSessionRuntimeTarget,
        generation_overlay: SessionGenerationOverlay,
    ) -> Result<Self, AgentSessionRuntimeError>
    where
        F: AgentSessionRuntimeFactory + 'static,
    {
        Self::create_with_overlay_and_initial_state(
            factory,
            initial_target,
            generation_overlay,
            None,
        )
        .await
    }

    pub(crate) async fn create_with_overlay_and_initial_state<F>(
        factory: F,
        initial_target: AgentSessionRuntimeTarget,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
    ) -> Result<Self, AgentSessionRuntimeError>
    where
        F: AgentSessionRuntimeFactory + 'static,
    {
        let factory: Arc<dyn AgentSessionRuntimeFactory> = Arc::new(factory);
        let start_event = SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        };
        let prepared = factory
            .prepare(AgentSessionRuntimeRequest {
                target: initial_target,
                start_event: start_event.clone(),
                generation_overlay: generation_overlay.clone(),
                initial_state,
            })
            .await?;
        let session = prepared.activate(start_event).await;
        Ok(Self::from_parts(session, factory, generation_overlay))
    }

    pub fn from_session<F>(session: AgentSession, factory: F) -> Self
    where
        F: AgentSessionRuntimeFactory + 'static,
    {
        Self::from_parts(
            Arc::new(session),
            Arc::new(factory),
            SessionGenerationOverlay::default(),
        )
    }

    fn from_parts(
        session: Arc<AgentSession>,
        factory: Arc<dyn AgentSessionRuntimeFactory>,
        generation_overlay: SessionGenerationOverlay,
    ) -> Self {
        let (current, _) = watch::channel(session);
        Self {
            current,
            factory,
            generation_overlay,
            transition_gate: Arc::new(Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn session(&self) -> Arc<AgentSession> {
        Arc::clone(&self.current.borrow())
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<AgentSession>> {
        self.current.subscribe()
    }

    pub async fn new_session(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, AgentSessionRuntimeError> {
        self.new_session_with_parent(cwd, path, None).await
    }

    pub async fn new_session_with_parent(
        &self,
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        parent_session: Option<PathBuf>,
    ) -> Result<AgentSessionReplacement, AgentSessionRuntimeError> {
        let _transition = self.transition_gate.lock().await;
        self.ensure_open()?;
        let current = self.session();
        let path = path.into();
        let before = current
            .session_plugin_driver()
            .session_before_switch(&SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::New,
                target_session_file: None,
            })
            .await;
        if before.is_some_and(|result| result.cancel) {
            return Ok(AgentSessionReplacement::Cancelled);
        }

        let previous_session_file = current.log().path().to_path_buf();
        self.replace_current(
            current,
            AgentSessionRuntimeRequest {
                target: match parent_session {
                    Some(parent) => {
                        AgentSessionRuntimeTarget::create_with_parent(cwd, &path, parent)
                    }
                    None => AgentSessionRuntimeTarget::create(cwd, &path),
                },
                start_event: SessionStartEvent {
                    reason: SessionStartReason::New,
                    previous_session_file: Some(previous_session_file),
                },
                generation_overlay: self.generation_overlay.clone(),
                initial_state: None,
            },
            SessionShutdownEvent {
                reason: SessionShutdownReason::New,
                target_session_file: Some(path),
            },
        )
        .await?;
        Ok(AgentSessionReplacement::Replaced)
    }

    pub async fn switch_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, AgentSessionRuntimeError> {
        let _transition = self.transition_gate.lock().await;
        self.ensure_open()?;
        let current = self.session();
        let path = path.into();
        let before = current
            .session_plugin_driver()
            .session_before_switch(&SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::Resume,
                target_session_file: Some(path.clone()),
            })
            .await;
        if before.is_some_and(|result| result.cancel) {
            return Ok(AgentSessionReplacement::Cancelled);
        }

        let previous_session_file = current.log().path().to_path_buf();
        self.replace_current(
            current,
            AgentSessionRuntimeRequest {
                target: AgentSessionRuntimeTarget::open(&path),
                start_event: SessionStartEvent {
                    reason: SessionStartReason::Resume,
                    previous_session_file: Some(previous_session_file),
                },
                generation_overlay: self.generation_overlay.clone(),
                initial_state: None,
            },
            SessionShutdownEvent {
                reason: SessionShutdownReason::Resume,
                target_session_file: Some(path),
            },
        )
        .await?;
        Ok(AgentSessionReplacement::Replaced)
    }

    /// Copies a validated v4 JSONL session, or migrates coding-agent v1-v3,
    /// into product storage and switches through the resume transaction.
    pub async fn import_session(
        &self,
        source: impl Into<PathBuf>,
        destination: impl Into<PathBuf>,
    ) -> Result<AgentSessionReplacement, AgentSessionRuntimeError> {
        let _transition = self.transition_gate.lock().await;
        self.ensure_open()?;
        let current = self.session();
        let source = source.into();
        let destination = destination.into();
        let before = current
            .session_plugin_driver()
            .session_before_switch(&SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::Resume,
                target_session_file: Some(destination.clone()),
            })
            .await;
        if before.is_some_and(|result| result.cancel) {
            return Ok(AgentSessionReplacement::Cancelled);
        }

        let imported = ImportedFileTransaction::stage(&source, &destination)?;
        let previous_session_file = current.log().path().to_path_buf();
        self.replace_current(
            current,
            AgentSessionRuntimeRequest {
                target: AgentSessionRuntimeTarget::open(&destination),
                start_event: SessionStartEvent {
                    reason: SessionStartReason::Resume,
                    previous_session_file: Some(previous_session_file),
                },
                generation_overlay: self.generation_overlay.clone(),
                initial_state: None,
            },
            SessionShutdownEvent {
                reason: SessionShutdownReason::Resume,
                target_session_file: Some(destination),
            },
        )
        .await?;
        imported.commit();
        Ok(AgentSessionReplacement::Replaced)
    }

    /// Forks the current session at a message and atomically switches to the fork.
    /// `Before` is Pi's `/fork` behavior; `At` is `/clone`.
    pub async fn fork_session(
        &self,
        entry_id: impl Into<String>,
        position: ForkPosition,
    ) -> Result<AgentSessionReplacement, AgentSessionRuntimeError> {
        let _transition = self.transition_gate.lock().await;
        self.ensure_open()?;
        let current = self.session();
        let entry_id = entry_id.into();
        let before = current
            .session_plugin_driver()
            .session_before_fork(&SessionBeforeForkEvent {
                entry_id: entry_id.clone(),
                position,
            })
            .await;
        if before.is_some_and(|result| result.cancel) {
            return Ok(AgentSessionReplacement::Cancelled);
        }

        let _session_transition = current.begin_replacement().await?;
        let source = current.log().header();
        let id = uuid::Uuid::now_v7().to_string();
        let path = current
            .log()
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(format!("{id}.jsonl"));
        let mut header = SessionHeader::new(id, source.cwd);
        header.parent_session_id = Some(source.id);
        let fork = current.log().fork(
            &path,
            header,
            &ForkOptions::Branch {
                entry_id: Some(entry_id),
                position: Some(position),
            },
        )?;
        let previous_session_file = current.log().path().to_path_buf();
        let start_event = SessionStartEvent {
            reason: SessionStartReason::Fork,
            previous_session_file: Some(previous_session_file),
        };
        let prepared = match self
            .factory
            .prepare(AgentSessionRuntimeRequest {
                target: AgentSessionRuntimeTarget::reuse_log(fork),
                start_event: start_event.clone(),
                generation_overlay: self.generation_overlay.clone(),
                initial_state: None,
            })
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error.into());
            }
        };
        current
            .shutdown_with(SessionShutdownEvent {
                reason: SessionShutdownReason::Fork,
                target_session_file: Some(path),
            })
            .await;
        let next = prepared.activate(start_event).await;
        self.current.send_replace(next);
        Ok(AgentSessionReplacement::Replaced)
    }

    /// Rebuilds the entire current session through the factory. This reloads
    /// runtime, provider, feature, resource, and session plugin generations as
    /// one product-level transition.
    pub async fn reload(&self) -> Result<(), AgentSessionRuntimeError> {
        let _transition = self.transition_gate.lock().await;
        self.ensure_open()?;
        let current = self.session();
        let path = current.log().path().to_path_buf();
        let target = if current.log().is_materialized() {
            AgentSessionRuntimeTarget::open(&path)
        } else {
            AgentSessionRuntimeTarget::reuse_log(current.log().clone())
        };
        self.replace_current(
            current,
            AgentSessionRuntimeRequest {
                target,
                start_event: SessionStartEvent {
                    reason: SessionStartReason::Reload,
                    previous_session_file: None,
                },
                generation_overlay: self.generation_overlay.clone(),
                initial_state: None,
            },
            SessionShutdownEvent {
                reason: SessionShutdownReason::Reload,
                target_session_file: None,
            },
        )
        .await
    }

    pub fn abort(&self) {
        let session = self.session();
        session.abort();
        session.abort_compaction();
        session.abort_shell();
    }

    pub async fn shutdown(&self) -> Result<(), AgentSessionRuntimeError> {
        let _transition = self.transition_gate.lock().await;
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let current = self.session();
        let _transition = current.begin_replacement().await?;
        current.shutdown().await;
        Ok(())
    }

    async fn replace_current(
        &self,
        current: Arc<AgentSession>,
        request: AgentSessionRuntimeRequest,
        shutdown_event: SessionShutdownEvent,
    ) -> Result<(), AgentSessionRuntimeError> {
        let _session_transition = current.begin_replacement().await?;
        let start_event = request.start_event.clone();
        let prepared = self.factory.prepare(request).await?;
        current.shutdown_with(shutdown_event).await;
        let next = prepared.activate(start_event).await;
        self.current.send_replace(next);
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), AgentSessionRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            Err(AgentSessionRuntimeError::Closed)
        } else {
            Ok(())
        }
    }
}

fn sibling_transaction_path(path: &Path, purpose: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "session".into(), |name| name.to_os_string());
    name.push(format!(".{purpose}-{}.tmp", uuid::Uuid::now_v7()));
    path.with_file_name(name)
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
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;

    use pi_agent::AgentOptions;
    use pi_core::{
        AgentPlugin, Message, ModelId, PluginId, ProviderId, RegisterContext, UserMessage,
    };
    use pi_runtime::PiRuntime;
    use pi_test_support::ScriptedProviderPlugin;
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        AgentSessionOptions, SessionPlugin, SessionPluginContext, SessionPluginError,
        SessionPlugins,
    };

    #[derive(Clone)]
    struct TestFactory {
        events: Arc<StdMutex<Vec<String>>>,
        cancel_switch: Arc<AtomicBool>,
        fail_prepare: Arc<AtomicBool>,
        prepare_count: Arc<AtomicUsize>,
    }

    impl TestFactory {
        fn new() -> Self {
            Self {
                events: Arc::new(StdMutex::new(Vec::new())),
                cancel_switch: Arc::new(AtomicBool::new(false)),
                fail_prepare: Arc::new(AtomicBool::new(false)),
                prepare_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn events(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn record(&self, event: impl Into<String>) {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event.into());
        }
    }

    struct LifecyclePlugin {
        events: Arc<StdMutex<Vec<String>>>,
        cancel_switch: Arc<AtomicBool>,
    }

    struct OverlayPlugin;

    #[pi_core::agent_plugin]
    impl AgentPlugin for OverlayPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("session-overlay")
        }

        fn register(&self, _context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
            Ok(())
        }
    }

    impl LifecyclePlugin {
        fn record(&self, event: impl Into<String>) {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event.into());
        }
    }

    #[pi_session::session_plugin]
    impl SessionPlugin for LifecyclePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("runtime-lifecycle")
        }

        async fn session_start(
            &self,
            _context: &SessionPluginContext,
            event: &SessionStartEvent,
        ) -> Result<(), SessionPluginError> {
            self.record(format!("start:{:?}", event.reason));
            Ok(())
        }

        async fn session_before_switch(
            &self,
            _context: &SessionPluginContext,
            event: &SessionBeforeSwitchEvent,
        ) -> Result<Option<crate::SessionBeforeSwitchResult>, SessionPluginError> {
            self.record(format!("before:{:?}", event.reason));
            Ok(Some(crate::SessionBeforeSwitchResult {
                cancel: self.cancel_switch.load(Ordering::Acquire),
            }))
        }

        async fn session_shutdown(
            &self,
            _context: &SessionPluginContext,
            event: &SessionShutdownEvent,
        ) -> Result<(), SessionPluginError> {
            self.record(format!("shutdown:{:?}", event.reason));
            Ok(())
        }
    }

    #[async_trait]
    impl AgentSessionRuntimeFactory for TestFactory {
        async fn prepare(
            &self,
            request: AgentSessionRuntimeRequest,
        ) -> Result<PreparedAgentSession, SessionError> {
            self.prepare_count.fetch_add(1, Ordering::AcqRel);
            self.record(format!("prepare:{:?}", request.start_event.reason));
            if self.fail_prepare.load(Ordering::Acquire) {
                return Err(SessionError::Runtime(
                    "fixture preparation failed".to_string(),
                ));
            }

            let generation_overlay = request.generation_overlay;
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
            let mut builder = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd,
                    ..AgentOptions::default()
                });
            builder = generation_overlay.apply_to(builder);
            let runtime = builder.build()?;
            let options = AgentSessionOptions::default().plugins(SessionPlugins::new().plugin(
                LifecyclePlugin {
                    events: Arc::clone(&self.events),
                    cancel_switch: Arc::clone(&self.cancel_switch),
                },
            ));
            if create {
                AgentSession::prepare_create_with_options(runtime, path, options).await
            } else if let Some(log) = reused_log {
                AgentSession::prepare_reuse_with_options(runtime, log, options).await
            } else {
                AgentSession::prepare_open_with_options(runtime, path, options).await
            }
        }
    }

    #[tokio::test]
    async fn transient_generation_overlay_is_rebuilt_across_reload_and_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("first.jsonl");
        let loads = Arc::new(AtomicUsize::new(0));
        let factory_loads = Arc::clone(&loads);
        let overlay = SessionGenerationOverlay::new().with_agent_plugin(move || {
            factory_loads.fetch_add(1, Ordering::AcqRel);
            Arc::new(OverlayPlugin)
        });
        let runtime = AgentSessionRuntime::create_with_overlay(
            TestFactory::new(),
            AgentSessionRuntimeTarget::create(directory.path(), &path),
            overlay,
        )
        .await
        .unwrap();

        assert!(
            runtime
                .session()
                .runtime()
                .plugin_order()
                .contains(&PluginId::new("session-overlay"))
        );
        assert_eq!(loads.load(Ordering::Acquire), 1);

        runtime.reload().await.unwrap();
        assert!(
            runtime
                .session()
                .runtime()
                .plugin_order()
                .contains(&PluginId::new("session-overlay"))
        );
        assert_eq!(loads.load(Ordering::Acquire), 2);

        runtime
            .new_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap();
        assert!(
            runtime
                .session()
                .runtime()
                .plugin_order()
                .contains(&PluginId::new("session-overlay"))
        );
        assert_eq!(loads.load(Ordering::Acquire), 3);
    }

    #[derive(Clone)]
    struct BlockingFactory {
        inner: TestFactory,
        block: Arc<AtomicBool>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl AgentSessionRuntimeFactory for BlockingFactory {
        async fn prepare(
            &self,
            request: AgentSessionRuntimeRequest,
        ) -> Result<PreparedAgentSession, SessionError> {
            if self.block.load(Ordering::Acquire) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.prepare(request).await
        }
    }

    #[tokio::test]
    async fn new_and_resume_publish_only_after_ordered_lifecycle_transition() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let second_path = directory.path().join("second.jsonl");
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &first_path),
        )
        .await
        .unwrap();
        let mut changes = runtime.subscribe();
        let first = runtime.session();
        first.log().materialize().unwrap();

        let outcome = runtime
            .new_session(directory.path(), &second_path)
            .await
            .unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        changes.changed().await.unwrap();
        let second = runtime.session();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(first.is_closed());
        assert!(matches!(
            first.set_name(Some("stale".to_string())).await,
            Err(SessionError::Closed)
        ));
        assert_eq!(second.log().path(), second_path);
        assert_eq!(
            factory.events(),
            vec![
                "prepare:Startup",
                "start:Startup",
                "before:New",
                "prepare:New",
                "shutdown:New",
                "start:New",
            ]
        );

        let outcome = runtime.switch_session(&first_path).await.unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        assert_eq!(runtime.session().log().path(), first_path);
        assert_eq!(
            &factory.events()[6..],
            [
                "before:Resume",
                "prepare:Resume",
                "shutdown:Resume",
                "start:Resume",
            ]
        );
    }

    #[tokio::test]
    async fn import_copies_a_valid_v4_session_and_uses_resume_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let source_directory = tempfile::tempdir().unwrap();
        let current_path = directory.path().join("current.jsonl");
        let destination = directory.path().join("imported.jsonl");
        let source = source_directory.path().join("imported.jsonl");
        let imported_log =
            SessionLog::create(&source, SessionHeader::new("imported", directory.path())).unwrap();
        imported_log
            .append_message(Message::User(UserMessage::text("portable", 1)))
            .unwrap();
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &current_path),
        )
        .await
        .unwrap();
        let current = runtime.session();

        let outcome = runtime.import_session(&source, &destination).await.unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        assert!(current.is_closed());
        assert_eq!(runtime.session().log().path(), destination);
        assert_eq!(runtime.session().log().header().id, "imported");
        assert_eq!(runtime.session().log().load().unwrap().messages().len(), 1);
        assert!(source.exists());
        assert_eq!(
            factory.events(),
            vec![
                "prepare:Startup",
                "start:Startup",
                "before:Resume",
                "prepare:Resume",
                "shutdown:Resume",
                "start:Resume",
            ]
        );
    }

    #[tokio::test]
    async fn failed_import_leaves_the_source_and_current_session_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let current_path = directory.path().join("current.jsonl");
        let destination = directory.path().join("legacy.jsonl");
        let source = directory.path().join("outside-legacy.jsonl");
        let legacy = r#"{"type":"session","version":3,"id":"legacy"}
"#;
        std::fs::write(&source, legacy).unwrap();
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory,
            AgentSessionRuntimeTarget::create(directory.path(), &current_path),
        )
        .await
        .unwrap();
        let current = runtime.session();

        let error = runtime
            .import_session(&source, &destination)
            .await
            .unwrap_err();

        assert!(matches!(error, AgentSessionRuntimeError::Session(_)));
        assert!(Arc::ptr_eq(&current, &runtime.session()));
        assert!(!current.is_closed());
        assert!(!destination.exists());
        assert_eq!(std::fs::read_to_string(source).unwrap(), legacy);
    }

    #[tokio::test]
    async fn fork_copies_the_selected_branch_and_replaces_with_fork_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &first_path),
        )
        .await
        .unwrap();
        let first = runtime.session();
        let user = first
            .log()
            .append_message(Message::User(UserMessage::text("fork here", 1)))
            .unwrap();
        first.log().materialize().unwrap();

        let outcome = runtime
            .fork_session(&user, ForkPosition::Before)
            .await
            .unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        assert!(first.is_closed());
        let fork = runtime.session();
        assert_ne!(fork.log().path(), first_path);
        assert_eq!(
            fork.log().header().parent_session_id.as_deref(),
            Some(first.log().header().id.as_str())
        );
        assert!(fork.log().get_entry(&user).is_none());
        assert_eq!(
            &factory.events()[2..],
            ["prepare:Fork", "shutdown:Fork", "start:Fork"]
        );
    }

    #[tokio::test]
    async fn cancelled_switch_does_not_prepare_or_replace() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &first_path),
        )
        .await
        .unwrap();
        let first = runtime.session();
        factory.cancel_switch.store(true, Ordering::Release);

        let outcome = runtime
            .new_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Cancelled);
        assert!(Arc::ptr_eq(&first, &runtime.session()));
        assert_eq!(factory.prepare_count.load(Ordering::Acquire), 1);
        assert_eq!(
            factory.events(),
            vec!["prepare:Startup", "start:Startup", "before:New"]
        );
    }

    #[tokio::test]
    async fn preparation_failure_keeps_current_session_active() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &first_path),
        )
        .await
        .unwrap();
        let first = runtime.session();
        factory.fail_prepare.store(true, Ordering::Release);

        let error = runtime
            .new_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap_err();

        assert!(matches!(error, AgentSessionRuntimeError::Session(_)));
        assert!(Arc::ptr_eq(&first, &runtime.session()));
        assert!(!first.is_closed());
        first
            .set_name(Some("still active".to_string()))
            .await
            .unwrap();
        assert_eq!(
            factory.events(),
            vec![
                "prepare:Startup",
                "start:Startup",
                "before:New",
                "prepare:New",
            ]
        );
    }

    #[tokio::test]
    async fn cancelled_replacement_future_reopens_the_current_session() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let inner = TestFactory::new();
        let factory = BlockingFactory {
            inner,
            block: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &first_path),
        )
        .await
        .unwrap();
        let first = runtime.session();
        factory.block.store(true, Ordering::Release);

        let replacing = {
            let runtime = runtime.clone();
            let cwd = directory.path().to_path_buf();
            let second_path = directory.path().join("second.jsonl");
            tokio::spawn(async move { runtime.new_session(cwd, second_path).await })
        };
        factory.entered.notified().await;
        replacing.abort();
        assert!(replacing.await.unwrap_err().is_cancelled());

        assert!(Arc::ptr_eq(&first, &runtime.session()));
        assert!(!first.is_closed());
        first.set_name(Some("reopened".to_string())).await.unwrap();
    }

    #[tokio::test]
    async fn reload_rebuilds_the_whole_session_and_shutdown_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let factory = TestFactory::new();
        let runtime = AgentSessionRuntime::create(
            factory.clone(),
            AgentSessionRuntimeTarget::create(directory.path(), &path),
        )
        .await
        .unwrap();
        let first = runtime.session();
        assert!(!path.exists());

        runtime.reload().await.unwrap();

        assert!(!Arc::ptr_eq(&first, &runtime.session()));
        assert_eq!(runtime.session().log().path(), path);
        assert!(!path.exists());
        assert!(!runtime.session().log().is_materialized());
        assert_eq!(
            factory.events(),
            vec![
                "prepare:Startup",
                "start:Startup",
                "prepare:Reload",
                "shutdown:Reload",
                "start:Reload",
            ]
        );

        runtime.shutdown().await.unwrap();
        runtime.shutdown().await.unwrap();
        assert_eq!(
            factory.events().last().map(String::as_str),
            Some("shutdown:Quit")
        );
        assert!(matches!(
            runtime.reload().await.unwrap_err(),
            AgentSessionRuntimeError::Closed
        ));
    }
}
