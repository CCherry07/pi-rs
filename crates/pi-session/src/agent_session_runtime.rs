use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{ModelSelection, ThinkingLevel};
use pi_plugin::Plugin;
use pi_runtime::{PiRuntime, PiRuntimeBuilder};
use tokio::sync::watch;

use crate::journal::{comparable_path, sibling_transaction_path};
use crate::{
    AgentSession, AgentSessionOptions, ForkOptions, ForkPosition, PiSession, PreparedAgentSession,
    SessionBeforeForkEvent, SessionBeforeSwitchEvent, SessionError, SessionHeader, SessionLog,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason,
    SessionSwitchReason, import_session_file,
};

#[derive(Debug, Clone)]
pub(crate) enum AgentSessionRuntimeTarget {
    Create {
        cwd: PathBuf,
        path: PathBuf,
        parent_session: Option<PathBuf>,
        session_id: Option<String>,
    },
    Open {
        path: PathBuf,
    },
    Reuse {
        log: SessionLog,
    },
}

impl AgentSessionRuntimeTarget {
    pub(crate) fn create(cwd: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self::Create {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: None,
            session_id: None,
        }
    }

    pub(crate) fn create_with_id(
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Self {
        Self::Create {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: None,
            session_id: Some(session_id.into()),
        }
    }

    fn create_with_parent(
        cwd: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        parent_session: impl Into<PathBuf>,
    ) -> Self {
        Self::Create {
            cwd: cwd.into(),
            path: path.into(),
            parent_session: Some(parent_session.into()),
            session_id: None,
        }
    }

    pub(crate) fn open(path: impl Into<PathBuf>) -> Self {
        Self::Open { path: path.into() }
    }

    fn reuse_log(log: SessionLog) -> Self {
        Self::Reuse { log }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        match self {
            Self::Create { path, .. } | Self::Open { path } => path,
            Self::Reuse { log } => log.path(),
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
    pub cwd: PathBuf,
    pub session_path: PathBuf,
    pub reason: SessionStartReason,
    pub generation_overlay: SessionGenerationOverlay,
    /// Complete initial state for a fresh runtime. Product factories must
    /// validate any product-specific policy; `AgentSessionRuntime` applies it
    /// before constructing the session so persistence and provider state agree.
    pub initial_state: Option<AgentSessionInitialState>,
    /// Settled selection restored from the current conversation when the
    /// generation is being rebuilt for reload. Product policy may use this to
    /// avoid reapplying startup-only model arguments.
    pub reload_model: Option<ModelSelection>,
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
    fn apply_to(&self, runtime: &PiRuntime) -> Result<(), SessionError> {
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
        parent_session: Option<PathBuf>,
    },
    Resume {
        path: PathBuf,
    },
    Import {
        source: PathBuf,
        destination: PathBuf,
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
pub(crate) struct AgentSessionRuntime {
    current: watch::Sender<Arc<AgentSession>>,
    factory: Arc<dyn SessionGenerationFactory>,
    generation_overlay: SessionGenerationOverlay,
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

enum ResolvedSessionTarget {
    Create {
        path: PathBuf,
        parent_session: Option<PathBuf>,
        session_id: Option<String>,
    },
    Existing {
        log: SessionLog,
    },
}

impl ResolvedSessionTarget {
    fn resolve(
        target: AgentSessionRuntimeTarget,
        reason: SessionStartReason,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
    ) -> Result<(Self, SessionGenerationRequest), SessionError> {
        if initial_state.is_some() && !matches!(&target, AgentSessionRuntimeTarget::Create { .. }) {
            return Err(SessionError::Runtime(
                "initial runtime state is valid only for a fresh session".to_string(),
            ));
        }
        match target {
            AgentSessionRuntimeTarget::Create {
                cwd,
                path,
                parent_session,
                session_id,
            } => {
                let request = SessionGenerationRequest {
                    cwd,
                    session_path: path.clone(),
                    reason,
                    generation_overlay,
                    initial_state,
                    reload_model: None,
                };
                Ok((
                    Self::Create {
                        path,
                        parent_session,
                        session_id,
                    },
                    request,
                ))
            }
            AgentSessionRuntimeTarget::Open { path } => {
                let log = SessionLog::open_handle(&path)?;
                Self::resolve_existing(log, reason, generation_overlay)
            }
            AgentSessionRuntimeTarget::Reuse { log } => {
                Self::resolve_existing(log, reason, generation_overlay)
            }
        }
    }

    fn resolve_existing(
        log: SessionLog,
        reason: SessionStartReason,
        generation_overlay: SessionGenerationOverlay,
    ) -> Result<(Self, SessionGenerationRequest), SessionError> {
        let reload_model = reload_model(&log, reason)?;
        let request = SessionGenerationRequest {
            cwd: log.header().cwd.clone(),
            session_path: log.path().to_path_buf(),
            reason,
            generation_overlay,
            initial_state: None,
            reload_model,
        };
        Ok((Self::Existing { log }, request))
    }
}

fn reload_model(
    log: &SessionLog,
    reason: SessionStartReason,
) -> Result<Option<ModelSelection>, SessionError> {
    if reason != SessionStartReason::Reload {
        return Ok(None);
    }
    Ok(log
        .context()?
        .model
        .map(|model| ModelSelection::new(model.provider, model.model_id)))
}

impl AgentSessionRuntime {
    pub(crate) async fn create_with_overlay_and_initial_state(
        factory: Arc<dyn SessionGenerationFactory>,
        initial_target: AgentSessionRuntimeTarget,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
        initial_context: Option<crate::isolated_context::IsolatedContextSeed>,
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
            | ResolvedSessionTransition::Resume { .. }
            | ResolvedSessionTransition::Import { .. }) => {
                self.replace_with_switch(current, transition).await
            }
            ResolvedSessionTransition::Fork { entry_id, position } => {
                self.replace_with_fork(current, entry_id, position).await
            }
            ResolvedSessionTransition::Reload => self.replace_with_reload(current).await,
        }
    }

    /// Executes the shared Pi switch transaction for new, resume, and import.
    /// Import keeps its staged-file guard until the replacement is published.
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
            ResolvedSessionTransition::Import { destination, .. } => SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::Resume,
                target_session_file: Some(destination.clone()),
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

        let previous_session_file = current.log().path().to_path_buf();
        let mut imported = None;
        let (target, start_reason, shutdown_reason, target_session_file) = match transition {
            ResolvedSessionTransition::New {
                cwd,
                path,
                parent_session,
            } => {
                let target = match parent_session {
                    Some(parent) => {
                        AgentSessionRuntimeTarget::create_with_parent(cwd, &path, parent)
                    }
                    None => AgentSessionRuntimeTarget::create(cwd, &path),
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
                    AgentSessionRuntimeTarget::reuse_log(current.log().clone())
                } else {
                    AgentSessionRuntimeTarget::open(&path)
                };
                (
                    target,
                    SessionStartReason::Resume,
                    SessionShutdownReason::Resume,
                    Some(path),
                )
            }
            ResolvedSessionTransition::Import {
                source,
                destination,
            } => {
                imported = Some(ImportedFileTransaction::stage(&source, &destination)?);
                (
                    AgentSessionRuntimeTarget::open(&destination),
                    SessionStartReason::Resume,
                    SessionShutdownReason::Resume,
                    Some(destination),
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
        if let Some(imported) = imported {
            imported.commit();
        }
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
        let prepared = match Self::prepare_session(
            self.factory.as_ref(),
            AgentSessionRuntimeTarget::reuse_log(fork),
            start_event.reason,
            self.generation_overlay.clone(),
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
        self.current.send_replace(next);
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
        let target = AgentSessionRuntimeTarget::reuse_log(current.log().clone());
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
        target: AgentSessionRuntimeTarget,
        start_event: SessionStartEvent,
        initial_state: Option<AgentSessionInitialState>,
        shutdown_event: SessionShutdownEvent,
    ) -> Result<(), SessionError> {
        let _session_transition = current.begin_replacement().await?;
        let prepared = Self::prepare_session(
            self.factory.as_ref(),
            target,
            start_event.reason,
            self.generation_overlay.clone(),
            initial_state,
        )
        .await?;
        current.shutdown_with(shutdown_event).await;
        let next = prepared.activate(start_event).await;
        self.current.send_replace(next);
        Ok(())
    }

    async fn prepare_session(
        factory: &dyn SessionGenerationFactory,
        target: AgentSessionRuntimeTarget,
        reason: SessionStartReason,
        generation_overlay: SessionGenerationOverlay,
        initial_state: Option<AgentSessionInitialState>,
    ) -> Result<PreparedAgentSession, SessionError> {
        let (target, request) = ResolvedSessionTarget::resolve(
            target,
            reason,
            generation_overlay,
            initial_state.clone(),
        )?;
        let generation = factory.prepare_generation(request).await?;
        let PreparedSessionGeneration {
            runtime,
            mut options,
            session_bindings,
            activations,
        } = generation;

        if let Some(initial_state) = initial_state
            && let Err(error) = initial_state.apply_to(&runtime)
        {
            return Err(PreparedSessionGeneration::rollback(activations, error));
        }

        let prepared = match target {
            ResolvedSessionTarget::Create {
                path,
                parent_session,
                session_id,
            } => {
                options.parent_session_path = parent_session;
                options.session_id = session_id;
                AgentSession::prepare_create_with_options(runtime, path, options).await
            }
            ResolvedSessionTarget::Existing { log } => {
                options.parent_session_path = None;
                options.session_id = None;
                AgentSession::prepare_reuse_with_options(runtime, log, options).await
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
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use pi_agent::AgentOptions;
    use pi_core::{Message, ModelId, PluginId, ProviderId, UserMessage};
    use pi_plugin::{Plugin, RegisterContext};
    use pi_runtime::PiRuntime;
    use pi_test_support::ScriptedProviderPlugin;

    use super::*;
    use crate::{
        AgentSessionOptions, MultiSessionManager, MultiSessionManagerError, PluginError,
        SessionPluginContext,
    };

    #[derive(Clone)]
    struct TestFactory {
        events: Arc<StdMutex<Vec<String>>>,
        requests: Arc<StdMutex<Vec<GenerationRequestSnapshot>>>,
        cancel_switch: Arc<AtomicBool>,
        fail_prepare: Arc<AtomicBool>,
        fail_session_plugin_load: Arc<AtomicBool>,
        prepare_count: Arc<AtomicUsize>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct GenerationRequestSnapshot {
        cwd: PathBuf,
        session_path: PathBuf,
        reason: SessionStartReason,
        reload_model: Option<ModelSelection>,
        has_initial_state: bool,
    }

    impl TestFactory {
        fn new() -> Self {
            Self {
                events: Arc::new(StdMutex::new(Vec::new())),
                requests: Arc::new(StdMutex::new(Vec::new())),
                cancel_switch: Arc::new(AtomicBool::new(false)),
                fail_prepare: Arc::new(AtomicBool::new(false)),
                fail_session_plugin_load: Arc::new(AtomicBool::new(false)),
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

        fn requests(&self) -> Vec<GenerationRequestSnapshot> {
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    struct LifecyclePlugin {
        events: Arc<StdMutex<Vec<String>>>,
        cancel_switch: Arc<AtomicBool>,
    }

    struct OverlayPlugin;

    struct StagedActivation {
        committed: Arc<AtomicBool>,
        rolled_back: Arc<AtomicBool>,
        armed: bool,
    }

    impl StagedActivation {
        fn new(committed: Arc<AtomicBool>, rolled_back: Arc<AtomicBool>) -> Self {
            Self {
                committed,
                rolled_back,
                armed: true,
            }
        }

        fn commit(mut self) {
            self.committed.store(true, Ordering::Release);
            self.armed = false;
        }
    }

    impl Drop for StagedActivation {
        fn drop(&mut self) {
            if self.armed {
                self.rolled_back.store(true, Ordering::Release);
            }
        }
    }

    struct RecordedActivation {
        factory: TestFactory,
        label: &'static str,
        staged: Option<StagedActivation>,
    }

    impl SessionGenerationActivation for RecordedActivation {
        fn commit(mut self: Box<Self>) {
            if let Some(staged) = self.staged.take() {
                staged.commit();
            }
            self.factory.record(self.label);
        }
    }

    #[derive(Clone)]
    struct ActivationFactory {
        inner: TestFactory,
        committed: Arc<AtomicBool>,
        rolled_back: Arc<AtomicBool>,
    }

    #[async_trait]
    impl SessionGenerationFactory for ActivationFactory {
        async fn prepare_generation(
            &self,
            request: SessionGenerationRequest,
        ) -> Result<PreparedSessionGeneration, SessionError> {
            let generation = self.inner.prepare_generation(request).await?;
            Ok(generation
                .with_activation(RecordedActivation {
                    factory: self.inner.clone(),
                    label: "commit:first",
                    staged: None,
                })
                .with_activation(RecordedActivation {
                    factory: self.inner.clone(),
                    label: "commit:second",
                    staged: Some(StagedActivation::new(
                        Arc::clone(&self.committed),
                        Arc::clone(&self.rolled_back),
                    )),
                }))
        }
    }

    #[pi_plugin::plugin]
    impl Plugin for OverlayPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("session-overlay")
        }

        fn register(&self, _context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
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

    #[pi_plugin::plugin]
    impl Plugin for LifecyclePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("runtime-lifecycle")
        }

        async fn session_start(
            &self,
            _context: &SessionPluginContext,
            event: &SessionStartEvent,
        ) -> Result<(), PluginError> {
            self.record(format!("start:{:?}", event.reason));
            Ok(())
        }

        async fn session_before_switch(
            &self,
            _context: &SessionPluginContext,
            event: &SessionBeforeSwitchEvent,
        ) -> Result<Option<crate::SessionBeforeSwitchResult>, PluginError> {
            self.record(format!("before:{:?}", event.reason));
            Ok(Some(crate::SessionBeforeSwitchResult {
                cancel: self.cancel_switch.load(Ordering::Acquire),
            }))
        }

        async fn session_shutdown(
            &self,
            _context: &SessionPluginContext,
            event: &SessionShutdownEvent,
        ) -> Result<(), PluginError> {
            self.record(format!("shutdown:{:?}", event.reason));
            Ok(())
        }
    }

    #[async_trait]
    impl SessionGenerationFactory for TestFactory {
        async fn prepare_generation(
            &self,
            request: SessionGenerationRequest,
        ) -> Result<PreparedSessionGeneration, SessionError> {
            self.prepare_count.fetch_add(1, Ordering::AcqRel);
            self.record(format!("prepare:{:?}", request.reason));
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(GenerationRequestSnapshot {
                    cwd: request.cwd.clone(),
                    session_path: request.session_path.clone(),
                    reason: request.reason,
                    reload_model: request.reload_model.clone(),
                    has_initial_state: request.initial_state.is_some(),
                });
            if self.fail_prepare.load(Ordering::Acquire) {
                return Err(SessionError::Runtime(
                    "fixture preparation failed".to_string(),
                ));
            }

            let generation_overlay = request.generation_overlay;
            let mut builder = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd: request.cwd,
                    ..AgentOptions::default()
                });
            builder = generation_overlay.apply_to(builder);
            let plugin_events = Arc::clone(&self.events);
            let cancel_switch = Arc::clone(&self.cancel_switch);
            let fail_session_plugin_load = Arc::clone(&self.fail_session_plugin_load);
            let runtime = builder
                .try_plugin_arc_factory(move || {
                    if fail_session_plugin_load.load(Ordering::Acquire) {
                        return Err("fixture session plugin load failed");
                    }
                    Ok(Arc::new(LifecyclePlugin {
                        events: Arc::clone(&plugin_events),
                        cancel_switch: Arc::clone(&cancel_switch),
                    }) as Arc<dyn Plugin>)
                })
                .build()?;
            Ok(PreparedSessionGeneration::new(
                runtime,
                AgentSessionOptions::default(),
            ))
        }
    }

    #[tokio::test]
    async fn factory_receives_resolved_generation_context_without_owning_session_storage() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        let path = directory.path().join("session.jsonl");
        std::fs::create_dir_all(&cwd).unwrap();
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager.create_session(&cwd, &path).await.unwrap();

        assert_eq!(session.path(), path);
        assert_eq!(
            factory.requests(),
            [GenerationRequestSnapshot {
                cwd: cwd.clone(),
                session_path: path.clone(),
                reason: SessionStartReason::Startup,
                reload_model: None,
                has_initial_state: false,
            }]
        );

        session.reload().await.unwrap();

        assert_eq!(session.path(), path);
        assert_eq!(
            factory.requests()[1],
            GenerationRequestSnapshot {
                cwd,
                session_path: path,
                reason: SessionStartReason::Reload,
                reload_model: Some(ModelSelection::new("scripted", "test")),
                has_initial_state: false,
            }
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn prepared_session_defers_external_state_until_activation() {
        let directory = tempfile::tempdir().unwrap();
        let inner = TestFactory::new();
        let committed = Arc::new(AtomicBool::new(false));
        let rolled_back = Arc::new(AtomicBool::new(false));
        let factory = ActivationFactory {
            inner: inner.clone(),
            committed: Arc::clone(&committed),
            rolled_back: Arc::clone(&rolled_back),
        };
        let start_event = SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        };
        let prepared = AgentSessionRuntime::prepare_session(
            &factory,
            AgentSessionRuntimeTarget::create(
                directory.path(),
                directory.path().join("session.jsonl"),
            ),
            start_event.reason,
            SessionGenerationOverlay::default(),
            None,
        )
        .await
        .unwrap();

        assert!(!committed.load(Ordering::Acquire));
        assert!(!rolled_back.load(Ordering::Acquire));
        assert_eq!(inner.events(), ["prepare:Startup"]);

        let session = prepared.activate(start_event).await;

        assert!(committed.load(Ordering::Acquire));
        assert!(!rolled_back.load(Ordering::Acquire));
        assert_eq!(
            inner.events(),
            [
                "prepare:Startup",
                "commit:first",
                "commit:second",
                "start:Startup",
            ]
        );
        session.shutdown().await;
    }

    #[tokio::test]
    async fn dropping_a_prepared_session_rolls_back_staged_external_state() {
        let directory = tempfile::tempdir().unwrap();
        let inner = TestFactory::new();
        let committed = Arc::new(AtomicBool::new(false));
        let rolled_back = Arc::new(AtomicBool::new(false));
        let factory = ActivationFactory {
            inner: inner.clone(),
            committed: Arc::clone(&committed),
            rolled_back: Arc::clone(&rolled_back),
        };
        let prepared = AgentSessionRuntime::prepare_session(
            &factory,
            AgentSessionRuntimeTarget::create(
                directory.path(),
                directory.path().join("session.jsonl"),
            ),
            SessionStartReason::Startup,
            SessionGenerationOverlay::default(),
            None,
        )
        .await
        .unwrap();

        drop(prepared);

        assert!(!committed.load(Ordering::Acquire));
        assert!(rolled_back.load(Ordering::Acquire));
        assert_eq!(inner.events(), ["prepare:Startup"]);
    }

    #[tokio::test]
    async fn transient_generation_overlay_is_rebuilt_across_reload_and_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("first.jsonl");
        let loads = Arc::new(AtomicUsize::new(0));
        let factory_loads = Arc::clone(&loads);
        let overlay = SessionGenerationOverlay::new().with_plugin(move || {
            factory_loads.fetch_add(1, Ordering::AcqRel);
            Arc::new(OverlayPlugin)
        });
        let manager = MultiSessionManager::new(TestFactory::new());
        let session = manager
            .create_session_with_overlay(directory.path(), &path, overlay)
            .await
            .unwrap();

        assert!(
            session
                .current()
                .runtime()
                .plugin_order()
                .contains(&PluginId::new("session-overlay"))
        );
        assert_eq!(loads.load(Ordering::Acquire), 1);

        session.reload().await.unwrap();
        assert!(
            session
                .current()
                .runtime()
                .plugin_order()
                .contains(&PluginId::new("session-overlay"))
        );
        assert_eq!(loads.load(Ordering::Acquire), 2);

        session
            .new_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap();
        assert!(
            session
                .current()
                .runtime()
                .plugin_order()
                .contains(&PluginId::new("session-overlay"))
        );
        assert_eq!(loads.load(Ordering::Acquire), 3);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn new_and_resume_publish_only_after_ordered_lifecycle_transition() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let second_path = directory.path().join("second.jsonl");
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let mut changes = session.subscribe();
        let first = session.current();
        first.log().materialize().unwrap();

        let outcome = session
            .new_session(directory.path(), &second_path)
            .await
            .unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        changes.changed().await.unwrap();
        let second = session.current();
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

        let outcome = session.resume_session(&first_path).await.unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        assert_eq!(session.path(), first_path);
        assert_eq!(
            &factory.events()[6..],
            [
                "before:Resume",
                "prepare:Resume",
                "shutdown:Resume",
                "start:Resume",
            ]
        );
        manager.shutdown().await.unwrap();
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
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &current_path)
            .await
            .unwrap();
        let current = session.current();

        let outcome = session.import_session(&source).await.unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        assert!(current.is_closed());
        assert_eq!(session.path(), destination);
        assert_eq!(session.current().log().header().id, "imported");
        assert_eq!(session.current().log().load().unwrap().messages().len(), 1);
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
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_import_leaves_the_source_and_current_session_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let source_directory = tempfile::tempdir().unwrap();
        let current_path = directory.path().join("current.jsonl");
        let destination = directory.path().join("legacy.jsonl");
        let source = source_directory.path().join("legacy.jsonl");
        let legacy = r#"{"type":"session","version":3,"id":"legacy"}
"#;
        std::fs::write(&source, legacy).unwrap();
        let manager = MultiSessionManager::new(TestFactory::new());
        let session = manager
            .create_session(directory.path(), &current_path)
            .await
            .unwrap();
        let current = session.current();

        let error = session.import_session(&source).await.unwrap_err();

        assert!(matches!(error, MultiSessionManagerError::Session(_)));
        assert!(Arc::ptr_eq(&current, &session.current()));
        assert!(!current.is_closed());
        assert!(!destination.exists());
        assert_eq!(std::fs::read_to_string(source).unwrap(), legacy);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fork_copies_the_selected_branch_and_replaces_with_fork_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let first = session.current();
        let user = first
            .log()
            .append_message(Message::User(UserMessage::text("fork here", 1)))
            .unwrap();
        first.log().materialize().unwrap();

        let outcome = session
            .fork_session(&user, ForkPosition::Before)
            .await
            .unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Replaced);
        assert!(first.is_closed());
        let fork = session.current();
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
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_switch_does_not_prepare_or_replace() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let first = session.current();
        factory.cancel_switch.store(true, Ordering::Release);

        let outcome = session
            .new_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap();

        assert_eq!(outcome, AgentSessionReplacement::Cancelled);
        assert!(Arc::ptr_eq(&first, &session.current()));
        assert_eq!(factory.prepare_count.load(Ordering::Acquire), 1);
        assert_eq!(
            factory.events(),
            vec!["prepare:Startup", "start:Startup", "before:New"]
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn preparation_failure_keeps_current_session_active() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let first = session.current();
        factory.fail_prepare.store(true, Ordering::Release);

        let error = session
            .new_session(directory.path(), directory.path().join("second.jsonl"))
            .await
            .unwrap_err();

        assert!(matches!(error, MultiSessionManagerError::Session(_)));
        assert!(Arc::ptr_eq(&first, &session.current()));
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
        factory.fail_prepare.store(false, Ordering::Release);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn session_plugin_load_failure_keeps_whole_generation_active() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        let current = session.current();
        factory
            .fail_session_plugin_load
            .store(true, Ordering::Release);

        let error = session.reload().await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("fixture session plugin load failed")
        );
        assert!(Arc::ptr_eq(&current, &session.current()));
        assert!(!current.is_closed());
        assert_eq!(
            factory.events(),
            ["prepare:Startup", "start:Startup", "prepare:Reload"]
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reload_rebuilds_the_whole_session_and_manager_shutdown_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let factory = TestFactory::new();
        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(directory.path(), &path)
            .await
            .unwrap();
        let first = session.current();
        assert!(!path.exists());

        session.reload().await.unwrap();

        assert!(!Arc::ptr_eq(&first, &session.current()));
        assert_eq!(session.path(), path);
        assert!(!path.exists());
        assert!(!session.current().log().is_materialized());
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

        manager.shutdown().await.unwrap();
        manager.shutdown().await.unwrap();
        assert_eq!(
            factory.events().last().map(String::as_str),
            Some("shutdown:Quit")
        );
        assert!(matches!(
            session.reload().await.unwrap_err(),
            MultiSessionManagerError::Closed
        ));
    }
}
