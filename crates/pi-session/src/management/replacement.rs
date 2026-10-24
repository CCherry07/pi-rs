use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
use pi_core::ModelSelection;
use pi_plugin::Plugin;
use pi_runtime::{PiRuntime, PiRuntimeBuilder};
use tokio::sync::watch;

use super::paths::comparable_path;
use crate::runtime::{RestoredSessionConfiguration, SessionRecoveryPlan};
use crate::{
    AgentSession, AgentSessionInitialState, AgentSessionOptions, ForkOptions, ForkPosition,
    PiSession, PreparedAgentSession, SessionBeforeForkEvent, SessionBeforeSwitchEvent,
    SessionError, SessionHeader, SessionShutdownEvent, SessionShutdownReason, SessionStartEvent,
    SessionStartReason, SessionStore, SessionSwitchReason,
};

#[derive(Debug, Clone)]
pub(crate) enum SessionTarget {
    Create {
        workspace: pi_core::WorkspaceSnapshot,
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
        path: PathBuf,
        parent_session_id: Option<String>,
        session_id: Option<String>,
    },
    Open {
        path: PathBuf,
    },
    Reuse {
        store: SessionStore,
    },
}

impl SessionTarget {
    pub(crate) fn create(cwd: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self::Create {
            workspace: pi_core::WorkspaceSpec::from_cwd(cwd).snapshot(),
            metadata: None,
            path: path.into(),
            parent_session_id: None,
            session_id: None,
        }
    }

    fn create_with_parent(
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        parent_session_id: String,
    ) -> Self {
        Self::Create {
            workspace: pi_core::WorkspaceSpec::from_cwd(cwd).snapshot(),
            metadata: None,
            path: path.into(),
            parent_session_id: Some(parent_session_id),
            session_id: None,
        }
    }

    pub(crate) fn with_workspace(
        mut self,
        workspace: pi_core::WorkspaceSpec,
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Self {
        if let Self::Create {
            workspace: saved,
            metadata: extra,
            ..
        } = &mut self
        {
            *saved = workspace.snapshot();
            *extra = metadata;
        }
        self
    }

    pub(crate) fn open(path: impl Into<PathBuf>) -> Self {
        Self::Open { path: path.into() }
    }

    fn reuse_store(store: SessionStore) -> Self {
        Self::Reuse { store }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        match self {
            Self::Create { path, .. } | Self::Open { path } => path,
            Self::Reuse { store } => store.path(),
        }
    }
}

type OverlayPluginFactory = Arc<dyn Fn() -> Arc<dyn Plugin> + Send + Sync>;

/// Session-local additions layered onto every product runtime generation.
///
/// The overlay is deliberately transient: it survives new/resume/fork/reload
/// replacements on the live handle, but is never serialized into the v4 log.
#[derive(Clone, Default)]
pub struct SessionGenerationOverlay {
    agent_plugins: Vec<OverlayPluginFactory>,
    execution_origin: pi_plugin::SessionExecutionOrigin,
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
    pub fn with_execution_origin(mut self, origin: pi_plugin::SessionExecutionOrigin) -> Self {
        self.execution_origin = origin;
        self
    }

    /// Adds one session-local plugin that is rebuilt for every runtime generation.
    pub fn with_plugin<F>(mut self, plugin: F) -> Self
    where
        F: Fn() -> Arc<dyn Plugin> + Send + Sync + 'static,
    {
        self.agent_plugins.push(Arc::new(plugin));
        self
    }

    /// Applies the opaque session-local additions to a product runtime builder.
    pub fn apply_to(&self, mut builder: PiRuntimeBuilder) -> PiRuntimeBuilder {
        builder = builder.execution_origin(self.execution_origin);
        for plugin in &self.agent_plugins {
            let plugin = Arc::clone(plugin);
            builder = builder.try_plugin_arc_factory(move || {
                Ok::<Arc<dyn Plugin>, std::convert::Infallible>(plugin())
            });
        }
        builder
    }
}

#[derive(Debug, Clone)]
pub struct SessionGenerationRequest {
    pub workspace: pi_core::WorkspaceSnapshot,
    /// Compatibility projection of workspace.cwd(); factories should use the full workspace.
    pub cwd: PathBuf,
    pub session_path: PathBuf,
    pub reason: SessionStartReason,
    pub generation_overlay: SessionGenerationOverlay,
    /// Complete initial state for a fresh runtime. Product factories must
    /// validate any product-specific policy; `SessionSlot` applies it
    /// before constructing the session so persistence and provider state agree.
    pub initial_state: Option<AgentSessionInitialState>,
    /// Settled selection restored from the current conversation when the
    /// generation is being rebuilt for reload. Product policy may use this to
    /// avoid reapplying startup-only model arguments.
    pub reload_model: Option<ModelSelection>,
    /// Tool names registered in the live generation being reloaded, including
    /// inactive tools. Product policy may enable newly available tools without
    /// re-enabling tools the user deselected. Absent for other transitions.
    pub reload_registered_tools: Option<Vec<String>>,
    /// Read-only projection of an existing journal after accepted recovery writes.
    /// Present for every existing journal, including those without configuration entries.
    pub restored_configuration: Option<RestoredSessionConfiguration>,
}

/// Product state that commits atomically with a prepared session generation.
///
/// Implementations should own rollback-on-drop guards as the cancellation
/// fallback. `rollback` is called when session construction returns an error
/// so adapters can preserve any more specific rollback diagnostics.
pub trait SessionGenerationActivation: Send {
    fn commit(self: Box<Self>);

    fn rollback(self: Box<Self>, error: SessionError) -> SessionError {
        error
    }
}

type SessionGenerationBinding = Box<dyn FnOnce(Arc<AgentSession>) + Send>;

/// A complete runtime and session-plugin generation that has not yet been
/// bound to storage or activated.
#[must_use = "prepared generations must be bound to a session or dropped to roll back staged state"]
pub struct PreparedSessionGeneration {
    runtime: PiRuntime,
    options: AgentSessionOptions,
    session_bindings: Vec<SessionGenerationBinding>,
    activations: Vec<Box<dyn SessionGenerationActivation>>,
}

impl PreparedSessionGeneration {
    pub fn new(runtime: PiRuntime, options: AgentSessionOptions) -> Self {
        Self {
            runtime,
            options,
            session_bindings: Vec::new(),
            activations: Vec::new(),
        }
    }

    /// Binds generation-scoped capabilities after the session is constructed
    /// and before its `session_start` hooks run.
    pub fn bind_session(
        mut self,
        binding: impl FnOnce(Arc<AgentSession>) + Send + 'static,
    ) -> Self {
        self.session_bindings.push(Box::new(binding));
        self
    }

    /// Stages generation-external state for the same activation transaction.
    pub fn with_activation(
        mut self,
        activation: impl SessionGenerationActivation + 'static,
    ) -> Self {
        self.activations.push(Box::new(activation));
        self
    }

    fn rollback(
        mut activations: Vec<Box<dyn SessionGenerationActivation>>,
        mut error: SessionError,
    ) -> SessionError {
        while let Some(activation) = activations.pop() {
            error = activation.rollback(error);
        }
        error
    }
}

#[async_trait]
pub trait SessionGenerationFactory: Send + Sync {
    /// Prepare a complete product generation without opening or mutating the
    /// session journal and without emitting `session_start`.
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError>;

    /// Observe a stable frontend handle after the multi-session manager has
    /// registered it and after each replacement has been published. Product
    /// adapters use this lifecycle seam to rebind outer capabilities without
    /// requiring callers to perform a second setup step.
    fn session_registered(&self, _session: &PiSession) {}
}

#[async_trait]
impl<F, Fut> SessionGenerationFactory for F
where
    F: Fn(SessionGenerationRequest) -> Fut + Send + Sync,
    Fut: Future<Output = Result<PreparedSessionGeneration, SessionError>> + Send,
{
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        self(request).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionReplacement {
    Replaced,
    Cancelled,
}

pub(crate) enum ResolvedSessionTransition {
    New {
        cwd: PathBuf,
        path: PathBuf,
        parent_session_id: Option<String>,
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

/// Owns the replaceable current `AgentSession`.
///
/// Managed replacement is serialized by `MultiSessionManager`. The current
/// agent is first settled, then the factory prepares the next product
/// generation and this module binds it to the target journal. A preparation
/// failure leaves the current session active. A successful transition emits
/// old `session_shutdown`, then new `session_start`, then publishes the new
/// handle to subscribers.
#[derive(Clone)]
pub(crate) struct SessionSlot {
    current: watch::Sender<Arc<AgentSession>>,
    state: Arc<SessionSlotState>,
    factory: Arc<dyn SessionGenerationFactory>,
    generation_overlay: SessionGenerationOverlay,
}

#[derive(Default)]
pub(super) struct SessionSlotState {
    pub(super) epoch: AtomicU64,
    pub(super) closed: AtomicBool,
}

struct CloseObservation<'a>(&'a SessionSlot);

impl Drop for CloseObservation<'_> {
    fn drop(&mut self) {
        // Wake observers even when shutdown is cancelled while awaiting a hook.
        self.0.current.send_modify(|_| {
            self.0.state.closed.store(true, Ordering::Release);
        });
    }
}

enum ResolvedSessionTarget {
    Create {
        path: PathBuf,
        parent_session_id: Option<String>,
        session_id: Option<String>,
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    },
    Existing {
        store: SessionStore,
        recovery: SessionRecoveryPlan,
    },
}

impl ResolvedSessionTarget {
    fn resolve(
        target: SessionTarget,
        reason: SessionStartReason,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
    ) -> Result<(Self, SessionGenerationRequest), SessionError> {
        if initial_state.is_some() && !matches!(&target, SessionTarget::Create { .. }) {
            return Err(SessionError::Runtime(
                "initial runtime state is valid only for a fresh session".to_string(),
            ));
        }
        match target {
            SessionTarget::Create {
                workspace,
                metadata,
                path,
                parent_session_id,
                session_id,
            } => {
                let request = SessionGenerationRequest {
                    cwd: workspace.cwd().to_path_buf(),
                    workspace,
                    session_path: path.clone(),
                    reason,
                    generation_overlay,
                    initial_state,
                    reload_model: None,
                    reload_registered_tools: None,
                    restored_configuration: None,
                };
                Ok((
                    Self::Create {
                        path,
                        parent_session_id,
                        session_id,
                        metadata,
                    },
                    request,
                ))
            }
            SessionTarget::Open { path } => {
                let store = SessionStore::open_handle(&path)?;
                Self::resolve_existing(store, reason, generation_overlay)
            }
            SessionTarget::Reuse { store } => {
                Self::resolve_existing(store, reason, generation_overlay)
            }
        }
    }

    fn resolve_existing(
        store: SessionStore,
        reason: SessionStartReason,
        generation_overlay: SessionGenerationOverlay,
    ) -> Result<(Self, SessionGenerationRequest), SessionError> {
        let reload_model = reload_model(&store, reason)?;
        let recovery = SessionRecoveryPlan::prepare(&store)?;
        let restored_configuration = Some(recovery.configuration(&store)?);
        let request = SessionGenerationRequest {
            workspace: store.header().workspace()?.snapshot(),
            cwd: store.header().cwd.clone(),
            session_path: store.path().to_path_buf(),
            reason,
            generation_overlay,
            initial_state: None,
            reload_model,
            reload_registered_tools: None,
            restored_configuration,
        };
        Ok((Self::Existing { store, recovery }, request))
    }
}

fn reload_model(
    store: &SessionStore,
    reason: SessionStartReason,
) -> Result<Option<ModelSelection>, SessionError> {
    if reason != SessionStartReason::Reload {
        return Ok(None);
    }
    Ok(store
        .context()?
        .model
        .map(|model| ModelSelection::new(model.provider, model.model_id)))
}

impl SessionSlot {
    pub(crate) async fn create_with_overlay_and_initial_state(
        factory: Arc<dyn SessionGenerationFactory>,
        initial_target: SessionTarget,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
        initial_context: Option<crate::history::IsolatedContextSeed>,
    ) -> Result<Self, SessionError> {
        let start_event = SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        };
        let prepared = Self::prepare_session(
            factory.as_ref(),
            initial_target,
            start_event.reason,
            generation_overlay.clone(),
            initial_state,
            None,
        )
        .await?;
        if let Some(seed) = initial_context {
            prepared.session().initialize_isolated_context(seed)?;
        }
        let session = prepared.activate(start_event).await;
        Ok(Self::from_parts(session, factory, generation_overlay))
    }

    fn from_parts(
        session: Arc<AgentSession>,
        factory: Arc<dyn SessionGenerationFactory>,
        generation_overlay: SessionGenerationOverlay,
    ) -> Self {
        let (current, _) = watch::channel(session);
        Self {
            current,
            state: Arc::default(),
            factory,
            generation_overlay,
        }
    }

    pub(crate) fn session(&self) -> Arc<AgentSession> {
        Arc::clone(&self.current.borrow())
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<AgentSession>> {
        self.current.subscribe()
    }

    pub(super) fn observe(&self) -> super::SessionObservation {
        super::SessionObservation::new(self.subscribe(), Arc::clone(&self.state))
    }

    fn publish(&self, next: Arc<AgentSession>) {
        self.current.send_modify(|current| {
            *current = next;
            self.state.epoch.fetch_add(1, Ordering::Release);
        });
    }

    pub(crate) async fn transition(
        &self,
        transition: ResolvedSessionTransition,
    ) -> Result<AgentSessionReplacement, SessionError> {
        // MultiSessionManager holds its lifecycle write guard across this
        // entire call. AgentSession::begin_replacement remains the inner gate
        // against live mutations of the captured session.
        let current = self.session();
        if current.is_closed() {
            return Err(SessionError::Closed);
        }
        match transition {
            transition @ (ResolvedSessionTransition::New { .. }
            | ResolvedSessionTransition::Resume { .. }) => {
                self.replace_with_switch(current, transition).await
            }
            ResolvedSessionTransition::Fork { entry_id, position } => {
                self.replace_with_fork(current, entry_id, position).await
            }
            ResolvedSessionTransition::Reload => self.replace_with_reload(current).await,
        }
    }

    /// Executes the shared Pi switch transaction for new and resume.
    async fn replace_with_switch(
        &self,
        current: Arc<AgentSession>,
        transition: ResolvedSessionTransition,
    ) -> Result<AgentSessionReplacement, SessionError> {
        let before_event = match &transition {
            ResolvedSessionTransition::New { .. } => SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::New,
                target_session_file: None,
            },
            ResolvedSessionTransition::Resume { path } => SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::Resume,
                target_session_file: Some(path.clone()),
            },
            ResolvedSessionTransition::Fork { .. } | ResolvedSessionTransition::Reload => {
                unreachable!("fork and reload have dedicated replacement transactions")
            }
        };
        let before = current
            .plugin_driver()
            .session_before_switch(current.session_dispatch_context(), &before_event)
            .await;
        if before.is_some_and(|result| result.cancel) {
            return Ok(AgentSessionReplacement::Cancelled);
        }

        let previous_session_file = current.store().path().to_path_buf();
        let (target, start_reason, shutdown_reason, target_session_file) = match transition {
            ResolvedSessionTransition::New {
                cwd,
                path,
                parent_session_id,
            } => {
                let inherit_workspace = cwd == current.runtime().cwd();
                let target = match parent_session_id {
                    Some(parent) => SessionTarget::create_with_parent(cwd, &path, parent),
                    None => SessionTarget::create(cwd, &path),
                };
                let target = if inherit_workspace {
                    target.with_workspace(
                        current.runtime().workspace().spec().clone(),
                        current.store().header().metadata,
                    )
                } else {
                    target
                };
                (
                    target,
                    SessionStartReason::New,
                    SessionShutdownReason::New,
                    Some(path),
                )
            }
            ResolvedSessionTransition::Resume { path } => {
                let target = if comparable_path(&previous_session_file) == comparable_path(&path) {
                    SessionTarget::reuse_store(current.store().clone())
                } else {
                    SessionTarget::open(&path)
                };
                (
                    target,
                    SessionStartReason::Resume,
                    SessionShutdownReason::Resume,
                    Some(path),
                )
            }
            ResolvedSessionTransition::Fork { .. } | ResolvedSessionTransition::Reload => {
                unreachable!("fork and reload have dedicated replacement transactions")
            }
        };
        self.replace_current(
            current,
            target,
            SessionStartEvent {
                reason: start_reason,
                previous_session_file: Some(previous_session_file),
            },
            None,
            SessionShutdownEvent {
                reason: shutdown_reason,
                target_session_file,
            },
        )
        .await?;
        Ok(AgentSessionReplacement::Replaced)
    }

    /// Forks the current session at a message and atomically switches to the fork.
    /// `Before` is Pi's `/fork` behavior; `At` is `/clone`.
    async fn replace_with_fork(
        &self,
        current: Arc<AgentSession>,
        entry_id: String,
        position: ForkPosition,
    ) -> Result<AgentSessionReplacement, SessionError> {
        let before = current
            .plugin_driver()
            .session_before_fork(
                current.session_dispatch_context(),
                &SessionBeforeForkEvent {
                    entry_id: entry_id.clone(),
                    position,
                },
            )
            .await;
        if before.is_some_and(|result| result.cancel) {
            return Ok(AgentSessionReplacement::Cancelled);
        }

        let _session_transition = current.begin_replacement().await?;
        let source = current.store().header();
        let id = uuid::Uuid::now_v7().to_string();
        let path = current
            .store()
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(format!("{id}.jsonl"));
        let mut header = SessionHeader::new(id, source.cwd);
        header.metadata = source.metadata;
        header.parent_session_id = Some(source.id);
        let fork = current.store().fork(
            &path,
            header,
            &ForkOptions::Branch {
                entry_id: Some(entry_id),
                position: Some(position),
            },
        )?;
        let previous_session_file = current.store().path().to_path_buf();
        let start_event = SessionStartEvent {
            reason: SessionStartReason::Fork,
            previous_session_file: Some(previous_session_file),
        };
        let prepared = match Self::prepare_session(
            self.factory.as_ref(),
            SessionTarget::reuse_store(fork),
            start_event.reason,
            self.generation_overlay.clone(),
            None,
            None,
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
        };
        current
            .shutdown_with(SessionShutdownEvent {
                reason: SessionShutdownReason::Fork,
                target_session_file: Some(path),
            })
            .await;
        let next = prepared.activate(start_event).await;
        self.publish(next);
        Ok(AgentSessionReplacement::Replaced)
    }

    /// Rebuilds the entire current session through the factory. This reloads
    /// runtime, provider, feature, resource, and session plugin generations as
    /// one product-level transition.
    async fn replace_with_reload(
        &self,
        current: Arc<AgentSession>,
    ) -> Result<AgentSessionReplacement, SessionError> {
        // Both generations refer to the same conversation. Sharing its journal
        // includes final shutdown-hook writes and preserves one mutation sequence,
        // including when the file has already materialized.
        let target = SessionTarget::reuse_store(current.store().clone());
        self.replace_current(
            current,
            target,
            SessionStartEvent {
                reason: SessionStartReason::Reload,
                previous_session_file: None,
            },
            None,
            SessionShutdownEvent {
                reason: SessionShutdownReason::Reload,
                target_session_file: None,
            },
        )
        .await?;
        Ok(AgentSessionReplacement::Replaced)
    }

    pub(crate) fn abort(&self) {
        let session = self.session();
        session.abort();
        session.abort_compaction();
        session.abort_shell();
    }

    pub(crate) async fn shutdown(&self) -> Result<(), SessionError> {
        // Close and manager shutdown hold the same lifecycle write guard used
        // by transition, so no separate runtime mutex is needed here.
        let current = self.session();
        let _close = CloseObservation(self);
        if current.is_closed() {
            return Ok(());
        }
        let _transition = current.begin_replacement().await?;
        current.shutdown().await;
        Ok(())
    }

    async fn replace_current(
        &self,
        current: Arc<AgentSession>,
        target: SessionTarget,
        start_event: SessionStartEvent,
        initial_state: Option<AgentSessionInitialState>,
        shutdown_event: SessionShutdownEvent,
    ) -> Result<(), SessionError> {
        let _session_transition = current.begin_replacement().await?;
        let reload_registered_tools =
            (start_event.reason == SessionStartReason::Reload).then(|| {
                current
                    .runtime()
                    .tool_specs()
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect()
            });
        let prepared = Self::prepare_session(
            self.factory.as_ref(),
            target,
            start_event.reason,
            self.generation_overlay.clone(),
            initial_state,
            reload_registered_tools,
        )
        .await?;
        current.shutdown_with(shutdown_event).await;
        let next = prepared.activate(start_event).await;
        self.publish(next);
        Ok(())
    }

    async fn prepare_session(
        factory: &dyn SessionGenerationFactory,
        target: SessionTarget,
        reason: SessionStartReason,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
        reload_registered_tools: Option<Vec<String>>,
    ) -> Result<PreparedAgentSession, SessionError> {
        let (target, mut request) = ResolvedSessionTarget::resolve(
            target,
            reason,
            generation_overlay,
            initial_state.clone(),
        )?;
        request.reload_registered_tools = reload_registered_tools;
        let expected_workspace = request.workspace.clone();
        let generation = factory.prepare_generation(request).await?;
        let PreparedSessionGeneration {
            runtime,
            mut options,
            session_bindings,
            activations,
        } = generation;

        if runtime.workspace() != &expected_workspace {
            return Err(PreparedSessionGeneration::rollback(
                activations,
                SessionError::Runtime(
                    "generation factory did not preserve requested workspace".into(),
                ),
            ));
        }

        if let Some(initial_state) = initial_state
            && let Err(error) = initial_state.apply_to(&runtime)
        {
            return Err(PreparedSessionGeneration::rollback(activations, error));
        }

        let prepared = match target {
            ResolvedSessionTarget::Create {
                path,
                parent_session_id,
                session_id,
                metadata,
            } => {
                options.parent_session_id = parent_session_id;
                options.session_id = session_id;
                options.header_metadata = metadata;
                AgentSession::prepare_create(runtime, path, options)
            }
            ResolvedSessionTarget::Existing { store, recovery } => {
                options.parent_session_id = None;
                options.session_id = None;
                AgentSession::prepare_reuse_with_recovery(runtime, store, options, recovery)
            }
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(PreparedSessionGeneration::rollback(activations, error));
            }
        };
        let session = prepared.session();
        for binding in session_bindings {
            binding(Arc::clone(&session));
        }
        if activations.is_empty() {
            Ok(prepared)
        } else {
            Ok(prepared.with_activation_commit(move || {
                for activation in activations {
                    activation.commit();
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests;
