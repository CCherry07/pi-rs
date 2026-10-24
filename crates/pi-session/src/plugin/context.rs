use crate::{AgentSession, AgentSessionReplacement, PiSession, SessionDocument, WeakPiSession};
use async_trait::async_trait;
use pi_plugin::{
    NoticeLevel, PluginContextError, PluginContextReplacement, PluginContextScope, PresentationMode,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq)]
pub enum PluginProviderMutation {
    Register { name: String, config: Value },
    Unregister { name: String },
}

/// Generation-external staging seam for provider mutations requested by a plugin.
/// The product adapter commits them through the next whole-session generation transaction.
pub trait PluginProviderMutationAccess: Send + Sync {
    fn stage(&self, mutation: PluginProviderMutation) -> Result<(), String>;
    fn has_pending(&self) -> bool;
}

/// Presentation bridge supplied by the product frontend.
///
/// `pi-session` owns the plugin-context adapter, while concrete terminal or
/// graphical interaction remains outside this crate.
#[async_trait]
pub trait PluginUiBridge: Send + Sync {
    fn surface_snapshot(&self) -> Result<pi_plugin::UiSurfaceSnapshot, String> {
        Err("extension surfaces unavailable".into())
    }
    fn ui_command(&self, _owner: String, _command: pi_plugin::UiCommand) -> Result<(), String> {
        Err("extension surfaces unavailable".into())
    }
    async fn surface_exchange(
        &self,
        _frame: pi_plugin::UiSurfaceFrame,
    ) -> Result<pi_plugin::UiSurfaceEvent, String> {
        Err("extension surfaces unavailable".into())
    }
}

#[derive(Default)]
struct PluginContextBindingState {
    sessions: Vec<WeakPiSession>,
    project_trust_by_session_id: HashMap<String, bool>,
}

/// Stable outer-session capability shared by native and JavaScript generations.
#[derive(Clone)]
pub struct PluginContextBinding {
    state: Arc<Mutex<PluginContextBindingState>>,
    shutdown: watch::Sender<bool>,
}

impl Default for PluginContextBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginContextBinding {
    pub fn new() -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            state: Arc::new(Mutex::new(PluginContextBindingState::default())),
            shutdown,
        }
    }

    pub fn bind(&self, session: PiSession) {
        let session = session.downgrade();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .sessions
            .retain(|candidate| candidate.upgrade().is_some());
        if !state
            .sessions
            .iter()
            .any(|candidate| candidate.registration_id() == session.registration_id())
        {
            state.sessions.push(session);
        }
    }

    pub async fn wait_for_shutdown(&self) {
        let mut receiver = self.shutdown.subscribe();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }

    pub(super) fn session(&self, generation_session_id: Option<&str>) -> Option<PiSession> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut live = Vec::with_capacity(state.sessions.len());
        let mut matched = None;
        state.sessions.retain(|candidate| {
            let Some(session) = candidate.upgrade() else {
                return false;
            };
            if generation_session_id.is_some_and(|id| session.id() == id) {
                matched = Some(session.clone());
            }
            live.push(session);
            true
        });
        matched.or_else(|| (live.len() == 1).then(|| live.remove(0)))
    }

    pub(super) fn register_project_trust(&self, session_id: String, trusted: bool) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project_trust_by_session_id
            .insert(session_id, trusted);
    }

    pub(super) fn project_trust(&self, session_id: &str) -> Option<bool> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project_trust_by_session_id
            .get(session_id)
            .copied()
    }

    pub(super) fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

/// `pi-session` implementation of the core plugin-context contract.
///
/// This handle is generation-bound by `pi-core::PluginContextEpoch`, but
/// its behavior is implemented against the real `AgentSession`, `PiSession`,
/// and each session's `PiRuntime`. It is not a JavaScript protocol adapter.
pub struct PiPluginContext {
    pub(super) mode: PresentationMode,
    pub(super) project_trusted: bool,
    generation_session: RwLock<Option<Weak<AgentSession>>>,
    pub(super) binding: PluginContextBinding,
    pub(super) runtime: tokio::runtime::Handle,
    provider_mutations: Option<Arc<dyn PluginProviderMutationAccess>>,
    pub(super) ui_bridge: Option<Arc<dyn PluginUiBridge>>,
    pub(super) ui_owners: Mutex<std::collections::HashSet<String>>,
    pub(super) provider_reload_gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) model_scope_patterns: Vec<String>,
}

impl PiPluginContext {
    pub fn new(
        mode: PresentationMode,
        project_trusted: bool,
        binding: PluginContextBinding,
    ) -> Self {
        Self {
            mode,
            project_trusted,
            generation_session: RwLock::new(None),
            binding,
            runtime: tokio::runtime::Handle::current(),
            provider_mutations: None,
            ui_bridge: None,
            ui_owners: Mutex::default(),
            provider_reload_gate: Arc::new(tokio::sync::Mutex::new(())),
            model_scope_patterns: Vec::new(),
        }
    }

    pub fn with_provider_mutations(
        mut self,
        access: Arc<dyn PluginProviderMutationAccess>,
    ) -> Self {
        self.provider_mutations = Some(access);
        self
    }

    pub fn with_ui_bridge(mut self, bridge: Arc<dyn PluginUiBridge>) -> Self {
        self.ui_bridge = Some(bridge);
        self
    }

    pub fn with_model_scope(mut self, patterns: Vec<String>) -> Self {
        self.model_scope_patterns = patterns;
        self
    }

    pub fn bind_generation_session(&self, session: Arc<AgentSession>) {
        self.binding
            .register_project_trust(session.store().id().to_owned(), self.project_trusted);
        *self
            .generation_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(&session));
    }

    pub(super) fn session(&self) -> Result<Arc<AgentSession>, PluginContextError> {
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
            .session(
                generation
                    .as_ref()
                    .map(|session| session.store().id().to_owned())
                    .as_deref(),
            )
            .map(|session| session.current())
            .or(generation)
            .ok_or(PluginContextError::Unbound)
    }

    pub(super) fn pi_session(&self) -> Result<PiSession, PluginContextError> {
        let generation_session_id = self
            .generation_session
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|session| session.store().id().to_owned());
        self.binding
            .session(generation_session_id.as_deref())
            .ok_or(PluginContextError::Unbound)
    }

    pub(super) fn document(
        &self,
    ) -> Result<(Arc<AgentSession>, Arc<SessionDocument>), PluginContextError> {
        let session = self.session()?;
        let document = session
            .store()
            .shared_document()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        Ok((session, document))
    }
}

impl PiPluginContext {
    pub(super) fn replacement(
        pi_session: PiSession,
        scope: PluginContextScope,
        replacement: AgentSessionReplacement,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        let cancelled = replacement == AgentSessionReplacement::Cancelled;
        let context = if cancelled {
            None
        } else {
            Some(pi_session.current().runtime().plugin_context_handle(scope))
        };
        Ok(PluginContextReplacement { cancelled, context })
    }

    pub(super) fn stage_provider_mutation(
        &self,
        mutation: PluginProviderMutation,
    ) -> Result<(), PluginContextError> {
        let access = self.provider_mutations.as_ref().ok_or_else(|| {
            PluginContextError::Unavailable(
                "dynamic provider registration is not configured".to_string(),
            )
        })?;
        access
            .stage(mutation)
            .map_err(PluginContextError::Invalid)?;
        let session = self.pi_session()?;
        let access = Arc::clone(access);
        let gate = Arc::clone(&self.provider_reload_gate);
        self.runtime.spawn(async move {
            let _reload = gate.lock().await;
            if !access.has_pending() {
                return;
            }
            if let Err(error) = session.reload().await {
                session.current().notify_plugin(
                    format!("plugin provider update failed: {error}"),
                    NoticeLevel::Error,
                );
            }
        });
        Ok(())
    }

    pub(super) async fn reload_pending_providers(&self) -> Result<(), PluginContextError> {
        if !self
            .provider_mutations
            .as_ref()
            .is_some_and(|mutations| mutations.has_pending())
        {
            return Ok(());
        }
        self.pi_session()?.reload().await.map_err(context_failed)
    }
}

pub(super) fn require_command(
    scope: PluginContextScope,
    operation: &'static str,
) -> Result<(), PluginContextError> {
    if scope == PluginContextScope::Command {
        Ok(())
    } else {
        Err(PluginContextError::CommandOnly(operation))
    }
}

pub(super) fn context_failed(error: impl std::fmt::Display) -> PluginContextError {
    PluginContextError::Failed(error.to_string())
}

pub(super) fn value<T: Serialize>(value: T) -> Result<Value, PluginContextError> {
    serde_json::to_value(value).map_err(context_failed)
}

pub(super) fn values<T: Serialize>(items: T) -> Result<Vec<Value>, PluginContextError> {
    match value(items)? {
        Value::Array(values) => Ok(values),
        _ => Err(PluginContextError::Failed(
            "context value did not serialize as an array".to_string(),
        )),
    }
}
