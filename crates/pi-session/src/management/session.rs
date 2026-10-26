//! Operations on a stable managed session handle and its owned children.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_core::{CustomMessageContent, CustomMessageInput, Message, ModelSelection};
use pi_plugin::{
    IsolatedFollowUpReceipt, IsolatedMessageReceipt, IsolatedSessionId, IsolatedSessionOptions,
    IsolatedSessionOutcome, IsolatedSessionRequest, IsolatedSessionTurnId, PluginContextError,
};
use tokio::sync::watch;

use super::IsolatedSessionObservation;
use super::paths::comparable_path;
use super::replacement::SessionTarget;
use super::{
    AgentSessionReplacement, ExistingSessionPolicy, ManagedSessionReplacement,
    MultiSessionManagerError, MultiSessionManagerInner, PiSession, SessionAcquisition,
    SessionGenerationOverlay, SessionObservation, SessionReplacementRequest,
    UnclaimedSessionPolicy, WeakPiSession,
};
use crate::{
    AgentSession, AgentSessionInitialModelSource, AgentSessionInitialState, ForkPosition,
    SessionError,
};

impl PiSession {
    pub fn downgrade(&self) -> WeakPiSession {
        WeakPiSession {
            registration_id: Arc::clone(&self.registration_id),
            manager: self.manager.clone(),
        }
    }

    /// Returns the current generation of the underlying agent session.
    pub fn current(&self) -> Arc<AgentSession> {
        self.slot.session()
    }

    /// Watches replacements and final closure of the managed handle.
    /// Prefer [`Self::observe`] when also consuming product events.
    pub fn subscribe(&self) -> watch::Receiver<Arc<AgentSession>> {
        self.slot.subscribe()
    }

    /// Observes product events across new, resume, fork and reload, with state
    /// resets on replacement or lag and termination after final closure.
    pub fn observe(&self) -> SessionObservation {
        self.slot.observe()
    }

    pub fn id(&self) -> String {
        self.current().store().header().id
    }

    pub fn path(&self) -> PathBuf {
        self.current().store().path().to_path_buf()
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
                if !parent.store().is_materialized() {
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
                    target: SessionTarget::create(self.cwd(), path).with_workspace(
                        parent.runtime().workspace().spec().clone(),
                        parent.store().header().metadata,
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
                custom_type: crate::history::ISOLATED_ORIGIN_TYPE.into(),
                data: Some(serde_json::json!({ "parentSessionId": parent.store().header().id })),
            });
            if let Err(error) = child.current().store().append_session_record(origin) {
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
                    target: SessionTarget::open(&path),
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
        if session.accepts_live_messages() {
            match session.enqueue_message(message.clone(), crate::QueueKind::Steer) {
                Ok(crate::PromptOutcome::Queued { .. }) => {
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
            let _ = session.run_messages(vec![message]).await;
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
        self.slot.abort();
    }

    pub(super) fn manager(
        &self,
    ) -> Result<Arc<MultiSessionManagerInner>, MultiSessionManagerError> {
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
        .store()
        .shared_document()
        .map_err(SessionError::from)?
        .isolated_parent_session_id()
        .map_err(SessionError::from)?;
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
