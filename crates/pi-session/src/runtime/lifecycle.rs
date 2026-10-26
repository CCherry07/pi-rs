//! Generation binding and activation of new or recovered sessions.
use super::activity::{ActivityGuard, ActivityPhase};
use super::{SESSION_CLOSED, SESSION_SHUTTING_DOWN, SESSION_TRANSITIONING};
use crate::{SessionShutdownEvent, SessionShutdownReason};
use pi_core::Message;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, RwLock};

use pi_plugin::SessionDispatchContext;
use pi_runtime::PiRuntime;

use super::activity::{RecoveredQueue, SessionActivity};
use super::configuration::clamp_thinking_for_model;
use super::context::{ContextUpdates, restore_runtime_context_with_request};
use super::recovery::{SessionRecoveryPlan, recover_interrupted_state};
use super::{AgentSession, SESSION_OPEN, bridge};
use crate::runtime::events::AgentSessionEventHub;
use crate::{
    ActiveToolsEntry, AgentSessionOptions, GenericCompactionPolicy, ModelChangeEntry, SessionEntry,
    SessionError, SessionHeader, SessionIdentity, SessionModel, SessionStartEvent,
    SessionStartReason, SessionStore, ThinkingLevelEntry, next_unique_id,
};

/// A fully constructed session whose `session_start` lifecycle event has not
/// been emitted yet.
///
/// Multi-session hosts use this two-phase form to prepare a replacement while
/// the current session is still valid, then order the lifecycle transition as
/// old `session_shutdown` followed by new `session_start`.
pub(crate) struct PreparedAgentSession {
    session: Arc<AgentSession>,
    activation_commit: Option<Box<dyn FnOnce() + Send>>,
}

impl PreparedAgentSession {
    /// Returns a clone of the prepared generation before lifecycle activation.
    ///
    /// Hosts use this to bind generation-scoped capabilities that must already
    /// be available to the subsequent `session_start` callbacks.
    pub(crate) fn session(&self) -> Arc<AgentSession> {
        Arc::clone(&self.session)
    }

    /// Attaches generation-external state that must become visible with this
    /// prepared session, immediately before its `session_start` hooks run.
    ///
    /// Dropping the preparation without activating it drops the callback and
    /// its captures instead. Product adapters use RAII captures to roll back
    /// staged package and provider state on failed or cancelled preparation.
    pub(crate) fn with_activation_commit(
        mut self,
        commit: impl FnOnce() + Send + 'static,
    ) -> PreparedAgentSession {
        self.activation_commit = Some(match self.activation_commit.take() {
            Some(previous) => Box::new(move || {
                previous();
                commit();
            }),
            None => Box::new(commit),
        });
        self
    }

    pub(crate) async fn activate(mut self, event: SessionStartEvent) -> Arc<AgentSession> {
        if let Some(commit) = self.activation_commit.take() {
            commit();
        }
        self.session
            .plugin_driver()
            .session_start(self.session.session_dispatch_context(), &event)
            .await;
        self.session
    }
}

impl AgentSession {
    /// Creates and activates a session using an independently assembled runtime.
    /// Product hosts use `MultiSessionManager` to own generation replacement.
    ///
    /// Preparation and activation are private to the session implementation:
    /// ```compile_fail
    /// fn prepare(runtime: pi_runtime::PiRuntime) {
    ///     pi_session::AgentSession::prepare_create(runtime, "session.jsonl", Default::default());
    /// }
    /// ```
    pub async fn create(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<Arc<Self>, SessionError> {
        let session = Self::prepare_create(runtime, path, options)?
            .activate(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            })
            .await;
        Ok(session)
    }

    pub(crate) fn prepare_create(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<PreparedAgentSession, SessionError> {
        default_configure_session_message_conversion(&runtime)?;
        let path = path.into();
        let state = runtime.agent().state();
        let effective_thinking = clamp_thinking_for_model(
            &runtime,
            &state.provider_id,
            &state.model_id,
            state.thinking_level,
        );
        if effective_thinking != state.thinking_level {
            runtime.set_thinking_level(effective_thinking)?;
        }
        let state = runtime.agent().state();
        let initial_model = crate::InitialModelRequest::default()
            .requested(state.provider_id.clone(), state.model_id.as_str());
        if let Some(session_id) = &options.session_id {
            crate::history::validate_session_id(session_id)?;
        }
        let mut header = SessionHeader::new(
            options
                .session_id
                .clone()
                .unwrap_or_else(|| next_unique_id("session")),
            runtime.cwd(),
        );
        header.metadata = options.header_metadata.clone();
        header.set_workspace(runtime.workspace().spec())?;
        header.parent_session_id = options.parent_session_id.clone();
        runtime.agent().set_session_id(Some(header.id.clone()));
        let store = SessionStore::create_deferred(path, header)?;

        let initial_entries = vec![
            SessionEntry::ModelChange(ModelChangeEntry {
                provider: state.provider_id,
                model_id: state.model_id,
            }),
            SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                thinking_level: state.thinking_level.as_str().to_string(),
            }),
            SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                active_tool_names: state.active_tools,
            }),
        ];
        store.append_batch(initial_entries)?;
        let context = store.context_with_options(&options.context)?;
        // A new session records the runtime's already-validated selection.
        // Preserve it even when the model is intentionally absent from the
        // catalog; catalog fallback applies only while restoring old state.
        let _ = restore_runtime_context_with_request(&runtime, &context, initial_model)?;
        let session = Self::assemble(runtime, store, options, RecoveredQueue::default())?;
        Ok(PreparedAgentSession {
            session,
            activation_commit: None,
        })
    }

    /// Restores only data state. Plugin code, registries and resources always
    /// come from the supplied runtime and are never deserialized from JSONL.
    pub async fn open(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<Arc<Self>, SessionError> {
        let path = path.into();
        let store = SessionStore::open_handle(&path)?;
        let recovery = SessionRecoveryPlan::prepare(&store)?;
        let session = Self::prepare_loaded(runtime, store, options, recovery)?
            .activate(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            })
            .await;
        Ok(session)
    }

    /// Reuses the in-memory log with recovery already prepared by the Manager,
    /// keeping an unmaterialized session lazy across reload.
    pub(crate) fn prepare_reuse_with_recovery(
        runtime: PiRuntime,
        store: SessionStore,
        options: AgentSessionOptions,
        recovery: SessionRecoveryPlan,
    ) -> Result<PreparedAgentSession, SessionError> {
        Self::prepare_loaded(runtime, store, options, recovery)
    }

    pub(super) fn prepare_loaded(
        runtime: PiRuntime,
        store: SessionStore,
        mut options: AgentSessionOptions,
        recovery: SessionRecoveryPlan,
    ) -> Result<PreparedAgentSession, SessionError> {
        if store.header().workspace()? != *runtime.workspace().spec() {
            return Err(SessionError::Runtime(
                "runtime workspace differs from saved session workspace".into(),
            ));
        }
        default_configure_session_message_conversion(&runtime)?;
        let recovered_queue = recover_interrupted_state(&store, recovery)?;
        let header = store.header();
        runtime.agent().set_session_id(Some(header.id.clone()));
        let mut context = store.context_with_options(&options.context)?;
        let stored_model = context.model.clone();
        let stored_thinking_level = context.thinking_level.clone();
        let stored_active_tools = context.active_tool_names.clone();
        context
            .active_tool_names
            .get_or_insert_with(|| runtime.active_tools())
            .extend(std::mem::take(&mut options.additional_active_tools));
        options.initial_model_fallback_message = restore_runtime_context_with_request(
            &runtime,
            &context,
            options.initial_model.clone().session(context.model.clone()),
        )?;
        let session = Self::assemble(runtime, store, options, recovered_queue)?;
        // Commit resolved configuration together after all initialization checks.
        // Replay must retain explicit resume overrides and new generation tools.
        let state = session.runtime.agent().state();
        let mut configuration = Vec::new();
        let model = SessionModel {
            provider: state.provider_id.clone(),
            model_id: state.model_id.clone(),
        };
        if stored_model.as_ref() != Some(&model) {
            configuration.push(SessionEntry::ModelChange(ModelChangeEntry {
                provider: state.provider_id,
                model_id: state.model_id,
            }));
        }
        if stored_thinking_level != state.thinking_level.as_str() {
            configuration.push(SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                thinking_level: state.thinking_level.as_str().to_string(),
            }));
        }
        if stored_active_tools.as_ref() != Some(&state.active_tools) {
            configuration.push(SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                active_tool_names: state.active_tools,
            }));
        }
        session.store.append_batch(configuration)?;
        Ok(PreparedAgentSession {
            session,
            activation_commit: None,
        })
    }

    pub(super) fn assemble(
        runtime: PiRuntime,
        store: SessionStore,
        options: AgentSessionOptions,
        recovered_queue: RecoveredQueue,
    ) -> Result<Arc<Self>, SessionError> {
        let events = AgentSessionEventHub::new(
            runtime.agent().state(),
            store.name(),
            recovered_queue.snapshot(),
        );
        let activity = Arc::new(SessionActivity::new(
            store.clone(),
            Arc::clone(&events),
            recovered_queue,
        ));
        let plugin_driver = runtime.plugin_driver();
        let session_dispatch_context = SessionDispatchContext {
            identity: session_identity(&store.header(), store.path().to_path_buf()),
            generation: runtime.generation(),
        };
        let session = Arc::new(Self {
            context_updates: ContextUpdates::new(
                runtime.clone(),
                store.clone(),
                options.context,
                Arc::clone(&events),
            ),
            runtime,
            store,
            runtime_inventory: options.runtime_inventory,
            plugin_driver,
            session_dispatch_context,
            compaction_settings: Arc::new(RwLock::new(options.compaction)),
            context_window: options.context_window,
            branch_summary_reserve_tokens: options.branch_summary_reserve_tokens.unwrap_or(16_384),
            lifecycle_state: Arc::new(AtomicU8::new(SESSION_OPEN)),
            usage_recording_open: Arc::new(AtomicBool::new(true)),
            events,
            activity,
            retry_settings: Arc::new(RwLock::new(options.retry)),
            shell_executor: options.shell_executor,
            compaction_policy: options
                .compaction_policy
                .unwrap_or_else(|| Arc::new(GenericCompactionPolicy)),
            initial_model_fallback_message: std::sync::Mutex::new(
                options.initial_model_fallback_message,
            ),
        });
        Self::install_session_turn_control(&session)?;
        bridge::attach(&session);
        Ok(session)
    }
}

pub(super) fn session_identity(header: &SessionHeader, path: PathBuf) -> SessionIdentity {
    SessionIdentity {
        id: header.id.clone(),
        path,
        cwd: header.cwd.clone(),
        parent_session_id: header.parent_session_id.clone(),
    }
}

pub(super) fn default_configure_session_message_conversion(
    runtime: &PiRuntime,
) -> Result<(), SessionError> {
    if runtime.agent().convert_to_llm().is_default() {
        runtime
            .agent()
            .configure(pi_agent::AgentConfigurationPatch {
                convert_to_llm: Some(runtime_message_converter()),
                ..pi_agent::AgentConfigurationPatch::default()
            })
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
    }
    Ok(())
}

pub(crate) struct AgentSessionTransitionGuard {
    lifecycle_state: Arc<AtomicU8>,
}

struct AgentSessionShutdownGuard<'a>(&'a AgentSession);

impl Drop for AgentSessionShutdownGuard<'_> {
    fn drop(&mut self) {
        self.0.usage_recording_open.store(false, Ordering::Release);
        self.0
            .lifecycle_state
            .store(SESSION_CLOSED, Ordering::Release);
        self.0.runtime.retire_plugin_context();
    }
}

impl Drop for AgentSessionTransitionGuard {
    fn drop(&mut self) {
        let _ = self.lifecycle_state.compare_exchange(
            SESSION_TRANSITIONING,
            SESSION_OPEN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl AgentSession {
    pub async fn shutdown(&self) {
        self.shutdown_with(SessionShutdownEvent {
            reason: SessionShutdownReason::Quit,
            target_session_file: None,
        })
        .await;
    }

    pub(crate) async fn shutdown_with(&self, event: SessionShutdownEvent) {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        if self.is_closed() {
            return;
        }
        self.lifecycle_state
            .store(SESSION_SHUTTING_DOWN, Ordering::Release);
        // Hooks may persist their final checkpoint while their context is still
        // valid. Retire it even if shutdown is cancelled while awaiting a hook.
        let _shutdown = AgentSessionShutdownGuard(self);
        self.plugin_driver()
            .session_shutdown(self.session_dispatch_context(), &event)
            .await;
    }

    pub fn is_closed(&self) -> bool {
        self.lifecycle_state.load(Ordering::Acquire) == SESSION_CLOSED
    }

    pub(crate) async fn begin_replacement(
        &self,
    ) -> Result<AgentSessionTransitionGuard, SessionError> {
        match self.lifecycle_state.compare_exchange(
            SESSION_OPEN,
            SESSION_TRANSITIONING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(SESSION_CLOSED) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Busy),
        }

        let transition = AgentSessionTransitionGuard {
            lifecycle_state: Arc::clone(&self.lifecycle_state),
        };

        self.abort();
        self.abort_shell();
        self.wait_for_idle().await;
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        Ok(transition)
    }

    pub(super) fn ensure_open(&self) -> Result<(), SessionError> {
        match self.lifecycle_state.load(Ordering::Acquire) {
            SESSION_OPEN => Ok(()),
            SESSION_CLOSED => Err(SessionError::Closed),
            _ => Err(SessionError::Busy),
        }
    }

    pub(super) fn try_begin_operation(&self) -> Result<ActivityGuard<'_>, SessionError> {
        let operation = self.activity.try_begin()?;
        self.ensure_open()?;
        Ok(operation)
    }
}

/// Session runtime messages already contain projected summaries and bash output.
/// Keep typed custom messages in agent state until the provider request boundary.
pub(crate) fn runtime_message_converter() -> pi_agent::ConvertToLlm {
    pi_agent::ConvertToLlm::new(|messages| async move {
        messages
            .into_iter()
            .map(Message::into_provider_message)
            .collect()
    })
}
