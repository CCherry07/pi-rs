use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use pi_agent::{AgentLoopOutcome, AgentLoopStop, EventError, PromptInput};
use pi_core::{
    AbortHandle, AgentEvent, ContentBlock, Message, ModelId, ProviderId, StopReason, ThinkingLevel,
    UserMessage,
};
use pi_prompt::BuildSystemPromptOptions;
use pi_runtime::{
    PiRuntime, PreparedTextSubmission, QueuedTextOutcome, RuntimePromptOutcome,
    RuntimeRestoreState, TextSubmissionOutcome,
};
use pi_shell::{DEFAULT_TIMEOUT, ShellChunk, ShellRequest, ShellResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::event::AgentSessionEventHub;
use crate::{
    ActiveToolsEntry, AgentMessage, BranchSummaryEntry, CompactionEntry, CompactionError,
    CompactionPreparation, CompactionSettings, CustomEntry, FileOperations, LaneRecordEntry,
    MAIN_LANE, ModelChangeEntry, NewLaneRecord, OperationError, OperationIntent, OperationOutcome,
    ProvisionedEntry, QueueKind, QueueSnapshot, SessionBeforeCompactEvent, SessionBeforeTreeEvent,
    SessionCompactEvent, SessionCompactFailedEvent, SessionContext, SessionContextBuildOptions,
    SessionDocument, SessionEntry, SessionError, SessionHeader, SessionIdentity,
    SessionInfoChangedEvent, SessionLog, SessionModel, SessionPluginDriver,
    SessionPluginReloadReport, SessionPlugins, SessionShutdownEvent, SessionShutdownReason,
    SessionStartEvent, SessionStartReason, SessionTreeEvent, ThinkingLevelEntry, TreePreparation,
    compact as generate_compaction, estimate_context_tokens, estimate_session_context_tokens,
    next_unique_id, now_ms, prepare_compaction, should_compact,
};

pub const PROMPT_SNAPSHOT_CUSTOM_TYPE: &str = "pi.prompt_snapshot";
pub const RESOURCE_DIAGNOSTIC_CUSTOM_TYPE: &str = "pi.resource_diagnostic";
const SESSION_OPEN: u8 = 0;
const SESSION_TRANSITIONING: u8 = 1;
const SESSION_CLOSED: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSnapshot {
    pub path: PathBuf,
    pub content_sha256: String,
}

#[derive(Debug, Clone)]
pub struct ShellExecutionOptions {
    pub exclude_from_context: bool,
    pub timeout: Option<std::time::Duration>,
    pub shell_path: Option<PathBuf>,
}

impl Default for ShellExecutionOptions {
    fn default() -> Self {
        Self {
            exclude_from_context: false,
            timeout: Some(DEFAULT_TIMEOUT),
            shell_path: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSnapshot {
    pub timestamp_ms: i64,
    #[serde(default)]
    pub generation: u64,
    pub base_system_prompt: String,
    pub effective_system_prompt: String,
    pub active_tools: Vec<String>,
    pub context_files: Vec<ResourceSnapshot>,
    pub skills: Vec<ResourceSnapshot>,
}

impl PromptSnapshot {
    fn capture(
        generation: u64,
        base_system_prompt: String,
        effective_system_prompt: String,
        active_tools: Vec<String>,
        options: Option<&BuildSystemPromptOptions>,
    ) -> Self {
        let context_files = options.map_or_else(Vec::new, |options| {
            options
                .context_files
                .iter()
                .map(|file| ResourceSnapshot {
                    path: file.path.clone(),
                    content_sha256: sha256(file.content.as_bytes()),
                })
                .collect()
        });
        Self {
            timestamp_ms: now_ms(),
            generation,
            base_system_prompt,
            effective_system_prompt,
            active_tools,
            context_files,
            // Skills are plugin-owned generation resources. The field stays in
            // this pi_rs diagnostic extension without coupling the session
            // backend to SkillsPlugin.
            skills: Vec::new(),
        }
    }
}

impl SessionDocument {
    pub fn latest_prompt_snapshot(&self) -> Option<PromptSnapshot> {
        self.branch().ok()?.into_iter().rev().find_map(|record| {
            let SessionEntry::Custom(custom) = &record.entry else {
                return None;
            };
            if custom.custom_type != PROMPT_SNAPSHOT_CUSTOM_TYPE {
                return None;
            }
            serde_json::from_value(custom.data.clone()?).ok()
        })
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SubmitOutcome {
    Agent(AgentLoopOutcome),
    Handled,
    Queued { kind: QueueKind, entry_id: String },
}

#[derive(Debug, Clone)]
struct PendingSessionMessage {
    kind: Option<QueueKind>,
    run_id: Option<String>,
    display_text: String,
    message: Message,
    target: ProvisionedEntry,
}

#[derive(Debug)]
struct ActiveSessionRun {
    id: String,
    generation: u64,
    pending: Vec<PendingSessionMessage>,
}

#[derive(Debug, Default)]
struct SessionActivity {
    active_run: Option<ActiveSessionRun>,
    recovered_queue: Vec<PendingSessionMessage>,
}

impl SessionActivity {
    fn queue_snapshot(&self) -> QueueSnapshot {
        let items = self
            .recovered_queue
            .iter()
            .chain(self.active_run.iter().flat_map(|run| run.pending.iter()));
        let mut snapshot = QueueSnapshot::default();
        for item in items {
            match item.kind {
                Some(QueueKind::Steer) => snapshot.steering.push(item.display_text.clone()),
                Some(QueueKind::FollowUp | QueueKind::NextRun) => {
                    snapshot.follow_up.push(item.display_text.clone());
                }
                None => {}
            }
        }
        snapshot
    }
}

#[derive(Clone, Default)]
pub struct AgentSessionOptions {
    pub context: SessionContextBuildOptions,
    pub plugins: SessionPlugins,
    pub compaction: CompactionSettings,
    /// Product-level model request to merge with a resumed session model.
    pub initial_model: crate::InitialModelRequest,
    /// Automatic compaction is enabled only when the model context window is
    /// known. Manual compaction remains available without this value.
    pub context_window: Option<u64>,
}

impl AgentSessionOptions {
    pub fn context(mut self, context: SessionContextBuildOptions) -> Self {
        self.context = context;
        self
    }

    pub fn plugins(mut self, plugins: SessionPlugins) -> Self {
        self.plugins = plugins;
        self
    }

    pub fn compaction(mut self, compaction: CompactionSettings) -> Self {
        self.compaction = compaction;
        self
    }

    pub fn initial_model(mut self, request: crate::InitialModelRequest) -> Self {
        self.initial_model = request;
        self
    }

    pub fn context_window(mut self, context_window: u64) -> Self {
        self.context_window = Some(context_window);
        self
    }
}

#[derive(Clone)]
pub struct AgentSession {
    runtime: PiRuntime,
    log: SessionLog,
    context_options: SessionContextBuildOptions,
    session_plugin_sources: SessionPlugins,
    session_plugin_driver: Arc<RwLock<Arc<SessionPluginDriver>>>,
    operation_gate: Arc<tokio::sync::Mutex<()>>,
    compaction_settings: CompactionSettings,
    context_window: Option<u64>,
    compaction_abort: Arc<std::sync::Mutex<Option<AbortHandle>>>,
    lifecycle_state: Arc<AtomicU8>,
    events: Arc<AgentSessionEventHub>,
    activity: Arc<std::sync::Mutex<SessionActivity>>,
    bash_abort: Arc<std::sync::Mutex<Option<(String, AbortHandle)>>>,
}

/// A fully constructed session whose `session_start` lifecycle event has not
/// been emitted yet.
///
/// Multi-session hosts use this two-phase form to prepare a replacement while
/// the current session is still valid, then order the lifecycle transition as
/// old `session_shutdown` followed by new `session_start`.
pub struct PreparedAgentSession {
    session: AgentSession,
}

pub(crate) struct AgentSessionTransitionGuard {
    lifecycle_state: Arc<AtomicU8>,
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

impl PreparedAgentSession {
    pub async fn activate(self, event: SessionStartEvent) -> AgentSession {
        self.session
            .session_plugin_driver()
            .session_start(&event)
            .await;
        self.session
    }
}

impl AgentSession {
    pub async fn create(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
    ) -> Result<Self, SessionError> {
        Self::create_with_options(runtime, path, AgentSessionOptions::default()).await
    }

    pub async fn create_with_context_options(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        context_options: SessionContextBuildOptions,
    ) -> Result<Self, SessionError> {
        Self::create_with_options(
            runtime,
            path,
            AgentSessionOptions::default().context(context_options),
        )
        .await
    }

    pub async fn create_with_options(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<Self, SessionError> {
        Ok(Self::prepare_create_with_options(runtime, path, options)
            .await?
            .activate(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            })
            .await)
    }

    pub async fn prepare_create(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
    ) -> Result<PreparedAgentSession, SessionError> {
        Self::prepare_create_with_options(runtime, path, AgentSessionOptions::default()).await
    }

    pub async fn prepare_create_with_options(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<PreparedAgentSession, SessionError> {
        let path = path.into();
        let state = runtime.agent().state();
        let header = SessionHeader::new(next_unique_id("session"), runtime.cwd());
        let identity = session_identity(&header, path.clone());
        let session_plugin_driver = Arc::new(options.plugins.build(identity)?);
        let log = SessionLog::create_deferred(path, header)?;

        let mut initial_entries = vec![
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
        initial_entries.extend(
            runtime
                .resource_diagnostics()
                .into_iter()
                .map(|diagnostic| {
                    SessionEntry::Custom(CustomEntry {
                        custom_type: RESOURCE_DIAGNOSTIC_CUSTOM_TYPE.to_string(),
                        data: serde_json::to_value(diagnostic).ok(),
                    })
                }),
        );
        log.append_batch(initial_entries)?;
        let context = log.load()?.context_with_options(&options.context)?;
        restore_runtime_context(&runtime, &context)?;
        let activity = Arc::new(std::sync::Mutex::new(SessionActivity::default()));
        let events = AgentSessionEventHub::new(
            runtime.agent().state(),
            log.name(),
            QueueSnapshot::default(),
        );

        let session = Self {
            runtime,
            log,
            context_options: options.context,
            session_plugin_sources: options.plugins,
            session_plugin_driver: Arc::new(RwLock::new(session_plugin_driver)),
            operation_gate: Arc::new(tokio::sync::Mutex::new(())),
            compaction_settings: options.compaction,
            context_window: options.context_window,
            compaction_abort: Arc::new(std::sync::Mutex::new(None)),
            lifecycle_state: Arc::new(AtomicU8::new(SESSION_OPEN)),
            events,
            activity,
            bash_abort: Arc::new(std::sync::Mutex::new(None)),
        };
        session.attach_agent_bridge();
        Ok(PreparedAgentSession { session })
    }

    /// Restores only data state. Plugin code, registries and resources always
    /// come from the supplied runtime and are never deserialized from JSONL.
    pub async fn open(runtime: PiRuntime, path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        Self::open_with_options(runtime, path, AgentSessionOptions::default()).await
    }

    pub async fn open_with_context_options(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        context_options: SessionContextBuildOptions,
    ) -> Result<Self, SessionError> {
        Self::open_with_options(
            runtime,
            path,
            AgentSessionOptions::default().context(context_options),
        )
        .await
    }

    pub async fn open_with_options(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<Self, SessionError> {
        Ok(Self::prepare_open_with_options(runtime, path, options)
            .await?
            .activate(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            })
            .await)
    }

    pub async fn prepare_open(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
    ) -> Result<PreparedAgentSession, SessionError> {
        Self::prepare_open_with_options(runtime, path, AgentSessionOptions::default()).await
    }

    pub async fn prepare_open_with_options(
        runtime: PiRuntime,
        path: impl Into<PathBuf>,
        options: AgentSessionOptions,
    ) -> Result<PreparedAgentSession, SessionError> {
        let path = path.into();
        let (log, document) = SessionLog::open(&path)?;
        Self::prepare_loaded(runtime, log, document, options)
    }

    /// Rebuilds runtime and plugin generations around an existing in-memory
    /// log. This keeps an unmaterialized session lazy across `/reload`.
    pub async fn prepare_reuse_with_options(
        runtime: PiRuntime,
        log: SessionLog,
        options: AgentSessionOptions,
    ) -> Result<PreparedAgentSession, SessionError> {
        let document = log.load()?;
        Self::prepare_loaded(runtime, log, document, options)
    }

    fn prepare_loaded(
        runtime: PiRuntime,
        log: SessionLog,
        document: SessionDocument,
        options: AgentSessionOptions,
    ) -> Result<PreparedAgentSession, SessionError> {
        let recovered_queue = recover_interrupted_state(&log, &document)?;
        let identity = session_identity(&document.header, log.path().to_path_buf());
        let session_plugin_driver = Arc::new(options.plugins.build(identity)?);
        let context = document.context_with_options(&options.context)?;
        restore_runtime_context_with_request(
            &runtime,
            &context,
            options.initial_model.clone().session(context.model.clone()),
        )?;
        let activity = Arc::new(std::sync::Mutex::new(SessionActivity {
            active_run: None,
            recovered_queue,
        }));
        let queue = activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .queue_snapshot();
        let events = AgentSessionEventHub::new(runtime.agent().state(), log.name(), queue);
        let session = Self {
            runtime,
            log,
            context_options: options.context,
            session_plugin_sources: options.plugins,
            session_plugin_driver: Arc::new(RwLock::new(session_plugin_driver)),
            operation_gate: Arc::new(tokio::sync::Mutex::new(())),
            compaction_settings: options.compaction,
            context_window: options.context_window,
            compaction_abort: Arc::new(std::sync::Mutex::new(None)),
            lifecycle_state: Arc::new(AtomicU8::new(SESSION_OPEN)),
            events,
            activity,
            bash_abort: Arc::new(std::sync::Mutex::new(None)),
        };
        session.attach_agent_bridge();
        Ok(PreparedAgentSession { session })
    }

    pub fn runtime(&self) -> &PiRuntime {
        &self.runtime
    }

    pub fn log(&self) -> &SessionLog {
        &self.log
    }

    /// Returns the authoritative frontend state at its current revision.
    pub fn snapshot(&self) -> crate::AgentSessionSnapshot {
        self.events.snapshot()
    }

    /// Atomically captures initial state and subscribes to subsequent product
    /// events. Consumers should ignore revisions at or below the snapshot's
    /// revision and refresh via [`Self::snapshot`] after receiver lag.
    pub fn subscribe(&self) -> crate::AgentSessionSubscription {
        self.events.subscribe()
    }

    pub fn session_plugin_driver(&self) -> Arc<SessionPluginDriver> {
        Arc::clone(
            &self
                .session_plugin_driver
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Builds the complete next generation before shutting down the old one.
    /// A load failure therefore leaves the active generation untouched.
    pub async fn reload_session_plugins(&self) -> Result<SessionPluginReloadReport, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        let previous = self.session_plugin_driver();
        let prepared = Arc::new(previous.next_generation(&self.session_plugin_sources)?);
        let report = SessionPluginReloadReport {
            previous_generation: previous.generation(),
            generation: prepared.generation(),
            plugin_order: prepared.plugin_order(),
        };
        previous
            .session_shutdown(&SessionShutdownEvent {
                reason: SessionShutdownReason::Reload,
                target_session_file: None,
            })
            .await;
        *self
            .session_plugin_driver
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(&prepared);
        prepared
            .session_start(&SessionStartEvent {
                reason: SessionStartReason::Reload,
                previous_session_file: None,
            })
            .await;
        Ok(report)
    }

    pub async fn shutdown(&self) {
        self.shutdown_with(SessionShutdownEvent {
            reason: SessionShutdownReason::Quit,
            target_session_file: None,
        })
        .await;
    }

    pub async fn shutdown_with(&self, event: SessionShutdownEvent) {
        let _operation = self.operation_gate.lock().await;
        if self.lifecycle_state.swap(SESSION_CLOSED, Ordering::AcqRel) == SESSION_CLOSED {
            return;
        }
        self.session_plugin_driver().session_shutdown(&event).await;
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

        self.runtime.abort();
        self.abort_compaction();
        self.abort_shell();
        self.runtime.wait_for_idle().await;
        let _operation = self.operation_gate.lock().await;
        Ok(transition)
    }

    pub async fn submit(&self, text: impl Into<String>) -> Result<SubmitOutcome, SessionError> {
        let text = text.into();
        if self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_run
            .is_some()
        {
            return self.queue_text(text, QueueKind::Steer).await;
        }
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        self.maybe_threshold_compact_locked().await;
        let prepared = match self.runtime.prepare_text_submission(text).await? {
            PreparedTextSubmission::Handled => return Ok(SubmitOutcome::Handled),
            PreparedTextSubmission::Agent(prepared) => prepared,
        };
        let run_id = self.begin_run(
            prepared.generation(),
            prepared.display_text(),
            prepared.text(),
        )?;
        let result = match prepared.run().await {
            Ok(recorded) => self.finish_prompt_locked(recorded).await,
            Err(error) => Err(SessionError::from(error)),
        };
        let finish_result = self.finish_run(&run_id, &result);
        self.events
            .publish_agent_settled(self.runtime.agent().state());
        match (result, finish_result) {
            (Ok(outcome), Ok(())) => Ok(SubmitOutcome::Agent(outcome)),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Queues a steering message for the active run after command/input-hook
    /// preprocessing. The durable queue record is committed before the Agent
    /// can observe the message.
    pub async fn steer(&self, text: impl Into<String>) -> Result<SubmitOutcome, SessionError> {
        self.queue_text(text.into(), QueueKind::Steer).await
    }

    /// Queues a follow-up message for the active run.
    pub async fn follow_up(&self, text: impl Into<String>) -> Result<SubmitOutcome, SessionError> {
        self.queue_text(text.into(), QueueKind::FollowUp).await
    }

    /// Clears queued messages and returns their editor-ready text. The
    /// cancellation records make the same result survive process recovery.
    pub fn clear_queue(&self) -> Result<QueueSnapshot, SessionError> {
        self.ensure_open()?;
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cleared = activity.queue_snapshot();
        let cancel = |item: &PendingSessionMessage| -> Result<(), SessionError> {
            if item.kind.is_none() {
                return Ok(());
            }
            self.log.append_record(NewLaneRecord {
                id: next_unique_id("queue-cancel"),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::QueueCancelled {
                    run_id: item.run_id.clone(),
                    entry_id: item.target.id.clone(),
                },
            })?;
            Ok(())
        };
        for item in &activity.recovered_queue {
            cancel(item)?;
        }
        if let Some(run) = &activity.active_run {
            for item in &run.pending {
                cancel(item)?;
            }
        }
        activity.recovered_queue.clear();
        if let Some(run) = &mut activity.active_run {
            run.pending.retain(|item| item.kind.is_none());
        }
        self.runtime.agent().clear_all_queues();
        let snapshot = activity.queue_snapshot();
        drop(activity);
        self.events.publish_queue(snapshot);
        Ok(cleared)
    }

    /// Requests cancellation without waiting for the operation gate. Any
    /// undelivered queue items are retained for [`Self::clear_queue`].
    pub fn abort(&self) {
        self.runtime.abort();
    }

    pub async fn execute_shell(
        &self,
        command: impl Into<String>,
        options: ShellExecutionOptions,
    ) -> Result<ShellResult, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        if self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_run
            .is_some()
        {
            return Err(SessionError::Busy);
        }
        let command = command.into();
        if command.trim().is_empty() {
            return Err(SessionError::InvalidPayload(
                "shell command cannot be empty".to_string(),
            ));
        }
        let id = next_unique_id("bash");
        let (abort, signal) = AbortHandle::new();
        {
            let mut current = self
                .bash_abort
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if current.is_some() {
                return Err(SessionError::Busy);
            }
            *current = Some((id.clone(), abort));
        }
        self.events
            .publish_bash_start(id.clone(), command.clone(), options.exclude_from_context);
        let events = Arc::clone(&self.events);
        let update_id = id.clone();
        let execution = pi_shell::execute(ShellRequest {
            command: command.clone(),
            cwd: self.runtime.cwd().to_path_buf(),
            timeout: options.timeout,
            shell_path: options.shell_path,
            abort_signal: signal,
            on_chunk: Some(Arc::new(move |chunk: ShellChunk| {
                events.publish_bash_update(update_id.clone(), chunk.stream, chunk.text);
            })),
        })
        .await;
        self.clear_bash_abort(&id);
        let result = match execution {
            Ok(result) => result,
            Err(error) => {
                self.events
                    .publish_bash_end(id, None, Some(error.to_string()));
                return Err(SessionError::Runtime(error.to_string()));
            }
        };
        let message = AgentMessage::custom(serde_json::json!({
            "role": "bashExecution",
            "command": command,
            "output": result.output.clone(),
            "exitCode": result.exit_code,
            "cancelled": result.cancelled,
            "truncated": result.truncated,
            "timestamp": now_ms(),
            "excludeFromContext": options.exclude_from_context,
        }))?;
        let entry = SessionEntry::message(message);
        if let Err(error) = (|| -> Result<(), SessionError> {
            self.log.append(entry.clone())?;
            let context = self
                .log
                .load()?
                .context_with_options(&self.context_options)?;
            restore_runtime_context(&self.runtime, &context)
        })() {
            self.events
                .publish_bash_end(id, Some(result), Some(error.to_string()));
            return Err(error);
        }
        self.events.publish_entry(entry);
        self.events.publish_bash_end(id, Some(result.clone()), None);
        Ok(result)
    }

    pub fn abort_shell(&self) {
        if let Some((_, abort)) = self
            .bash_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            abort.abort();
        }
    }

    fn clear_bash_abort(&self, id: &str) {
        let mut current = self
            .bash_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current
            .as_ref()
            .is_some_and(|(current_id, _)| current_id == id)
        {
            current.take();
        }
    }

    fn begin_run(
        &self,
        generation: u64,
        display_text: &str,
        text: &str,
    ) -> Result<String, SessionError> {
        let message = Message::User(UserMessage::text(text, now_ms()));
        let session_message = if display_text == text {
            AgentMessage::from(message.clone())
        } else {
            AgentMessage::with_display_text(message.clone(), display_text)?
        };
        let target = ProvisionedEntry {
            id: next_unique_id("entry"),
            entry: SessionEntry::message(session_message.clone()),
        };
        let run_id = next_unique_id("run");
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if activity.active_run.is_some() {
            return Err(SessionError::Busy);
        }
        self.log.append_record(NewLaneRecord {
            id: run_id.clone(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: self.log.leaf_id(),
                intent: OperationIntent::Run {
                    original_prompt: vec![session_message],
                    initial_messages: vec![target.clone()],
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        })?;
        activity.active_run = Some(ActiveSessionRun {
            id: run_id.clone(),
            generation,
            pending: vec![PendingSessionMessage {
                kind: None,
                run_id: Some(run_id.clone()),
                display_text: display_text.to_string(),
                message,
                target,
            }],
        });
        Ok(run_id)
    }

    async fn queue_text(
        &self,
        text: String,
        kind: QueueKind,
    ) -> Result<SubmitOutcome, SessionError> {
        self.ensure_open()?;
        let expected = {
            let activity = self
                .activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let run = activity.active_run.as_ref().ok_or(SessionError::Busy)?;
            (run.id.clone(), run.generation)
        };
        let (generation, display_text, text) = match self.runtime.process_queued_text(text).await? {
            QueuedTextOutcome::Handled => return Ok(SubmitOutcome::Handled),
            QueuedTextOutcome::Message {
                generation,
                display_text,
                text,
            } => (generation, display_text, text),
        };
        if generation != expected.1 {
            return Err(SessionError::Busy);
        }
        let message = Message::User(UserMessage::text(&text, now_ms()));
        let session_message = if display_text == text {
            AgentMessage::from(message.clone())
        } else {
            AgentMessage::with_display_text(message.clone(), &display_text)?
        };
        let target = ProvisionedEntry {
            id: next_unique_id("entry"),
            entry: SessionEntry::message(session_message),
        };
        let entry_id = target.id.clone();
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let run = activity.active_run.as_mut().ok_or(SessionError::Busy)?;
        if run.id != expected.0 || run.generation != generation {
            return Err(SessionError::Busy);
        }
        self.log.append_record(NewLaneRecord {
            id: next_unique_id("queue"),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::QueueEnqueued {
                queue: kind,
                run_id: Some(run.id.clone()),
                target: target.clone(),
            },
        })?;
        run.pending.push(PendingSessionMessage {
            kind: Some(kind),
            run_id: Some(run.id.clone()),
            display_text,
            message: message.clone(),
            target,
        });
        let snapshot = activity.queue_snapshot();
        drop(activity);
        self.events.publish_queue(snapshot);
        match kind {
            QueueKind::Steer => self.runtime.agent().steer(message),
            QueueKind::FollowUp => self.runtime.agent().follow_up(message),
            QueueKind::NextRun => {
                return Err(SessionError::InvalidEntry(
                    "next-run queue cannot target an active run".to_string(),
                ));
            }
        }
        Ok(SubmitOutcome::Queued { kind, entry_id })
    }

    fn finish_run(
        &self,
        run_id: &str,
        result: &Result<AgentLoopOutcome, SessionError>,
    ) -> Result<(), SessionError> {
        let snapshot = {
            let mut activity = self
                .activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(run) = activity.active_run.take() else {
                return Err(SessionError::Runtime(
                    "active session operation disappeared before completion".to_string(),
                ));
            };
            if run.id != run_id {
                activity.active_run = Some(run);
                return Err(SessionError::Runtime(
                    "active session operation id changed before completion".to_string(),
                ));
            }
            for mut item in run.pending.into_iter().filter(|item| item.kind.is_some()) {
                self.log.append_record(NewLaneRecord {
                    id: next_unique_id("queue-cancel"),
                    lane: MAIN_LANE.to_string(),
                    record: LaneRecordEntry::QueueCancelled {
                        run_id: item.run_id.clone(),
                        entry_id: item.target.id.clone(),
                    },
                })?;
                item.target.id = next_unique_id("entry");
                item.run_id = None;
                self.log.append_record(NewLaneRecord {
                    id: next_unique_id("queue-recovery"),
                    lane: MAIN_LANE.to_string(),
                    record: LaneRecordEntry::QueueEnqueued {
                        queue: QueueKind::NextRun,
                        run_id: None,
                        target: item.target.clone(),
                    },
                })?;
                activity.recovered_queue.push(item);
            }
            activity.queue_snapshot()
        };
        self.runtime.agent().clear_all_queues();
        self.events.publish_queue(snapshot);

        let (outcome, error) = match result {
            Ok(outcome) => match outcome.stop {
                AgentLoopStop::Aborted => (OperationOutcome::Aborted, None),
                AgentLoopStop::ProviderError | AgentLoopStop::MaxToolIterations => (
                    OperationOutcome::Failed,
                    Some(OperationError {
                        code: "agent_run_failed".to_string(),
                        message: format!("agent run stopped with {:?}", outcome.stop),
                    }),
                ),
                AgentLoopStop::Completed | AgentLoopStop::TerminatedByTools => {
                    (OperationOutcome::Completed, None)
                }
            },
            Err(error) => (
                OperationOutcome::Failed,
                Some(OperationError {
                    code: "session_run_failed".to_string(),
                    message: error.to_string(),
                }),
            ),
        };
        self.log.append_record(NewLaneRecord {
            id: next_unique_id("run-finish"),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::OperationFinished {
                run_id: run_id.to_string(),
                outcome,
                error,
            },
        })?;
        Ok(())
    }

    pub async fn prompt(
        &self,
        input: impl Into<PromptInput>,
    ) -> Result<AgentLoopOutcome, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        self.maybe_threshold_compact_locked().await;
        let recorded = match input.into() {
            PromptInput::Text(text) => match self.runtime.submit_text(text).await? {
                TextSubmissionOutcome::Handled => {
                    return Err(SessionError::Runtime(
                        "input was handled without starting an agent run; use submit()".to_string(),
                    ));
                }
                TextSubmissionOutcome::Agent(recorded) => *recorded,
            },
            messages => self.runtime.prompt_recorded(messages).await?,
        };
        let result = self.finish_prompt_locked(recorded).await;
        self.events
            .publish_agent_settled(self.runtime.agent().state());
        result
    }

    async fn finish_prompt_locked(
        &self,
        recorded: RuntimePromptOutcome,
    ) -> Result<AgentLoopOutcome, SessionError> {
        let outcome = self.record_prompt(recorded)?;
        if self.compaction_settings.enabled && self.is_overflow_outcome(&outcome) {
            if self
                .run_compaction_locked(crate::CompactionReason::Overflow, true, None, true)
                .await
                .is_ok()
                && let Ok(retried) = self.runtime.continue_recorded().await
            {
                return self.record_prompt(retried);
            }
            return Ok(outcome);
        }
        self.maybe_threshold_compact_locked().await;
        Ok(outcome)
    }

    fn record_prompt(
        &self,
        recorded: RuntimePromptOutcome,
    ) -> Result<AgentLoopOutcome, SessionError> {
        let outcome = recorded.outcome;
        let snapshot = PromptSnapshot::capture(
            recorded.generation,
            recorded.base_system_prompt,
            outcome.final_context.system_prompt.clone(),
            recorded.active_tools,
            recorded.prompt_options.as_ref(),
        );
        let entry = SessionEntry::Custom(CustomEntry {
            custom_type: PROMPT_SNAPSHOT_CUSTOM_TYPE.to_string(),
            data: Some(
                serde_json::to_value(snapshot)
                    .map_err(|error| SessionError::InvalidPayload(error.to_string()))?,
            ),
        });
        self.log.append(entry.clone())?;
        self.events.publish_entry(entry);
        Ok(outcome)
    }

    pub fn set_active_tools(
        &self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        self.runtime
            .set_active_tools(tools.into_iter().map(Into::into))?;
        let entry = SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: self.runtime.active_tools(),
        });
        self.log.append(entry.clone())?;
        self.events.publish_entry(entry);
        Ok(())
    }

    pub fn set_model(&self, provider: ProviderId, model_id: ModelId) -> Result<(), SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        self.runtime.set_model(provider.clone(), model_id.clone())?;
        let entry = SessionEntry::ModelChange(ModelChangeEntry { provider, model_id });
        self.log.append(entry.clone())?;
        self.events.publish_entry(entry);
        Ok(())
    }

    pub fn set_thinking_level(&self, thinking_level: ThinkingLevel) -> Result<(), SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        self.runtime.set_thinking_level(thinking_level)?;
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
            thinking_level: thinking_level.as_str().to_string(),
        });
        self.log.append(entry.clone())?;
        self.events.publish_entry(entry);
        self.events
            .publish_thinking(thinking_level, self.runtime.agent().state());
        Ok(())
    }

    pub async fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        let normalized = name.and_then(|name| {
            let name = name.trim().to_string();
            (!name.is_empty()).then_some(name)
        });
        self.log.set_name(normalized.clone())?;
        self.session_plugin_driver()
            .session_info_changed(&SessionInfoChangedEvent {
                name: normalized.clone(),
            })
            .await;
        self.events.publish_session_info(normalized);
        Ok(())
    }

    pub async fn checkout(&self, leaf_id: Option<&str>) -> Result<SessionContext, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        let document = self.log.load()?;
        let context = document.context_at_with_options(leaf_id, &self.context_options)?;
        let previous_leaf = self.log.leaf_id();
        if previous_leaf.as_deref() == leaf_id {
            return Ok(context);
        }
        let preparation = tree_preparation(&document, leaf_id, false)?;
        let (_, signal) = AbortHandle::new();
        let before = self
            .session_plugin_driver()
            .session_before_tree(&SessionBeforeTreeEvent {
                preparation,
                signal,
            })
            .await;
        if before.as_ref().is_some_and(|result| result.cancel) {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        self.log.move_lane(crate::MAIN_LANE, leaf_id)?;
        if let Err(error) = restore_runtime_context(&self.runtime, &context) {
            let _ = self
                .log
                .move_lane(crate::MAIN_LANE, previous_leaf.as_deref());
            return Err(error);
        }
        if let Some(label) = before.and_then(|result| result.label)
            && let Some(target_id) = leaf_id
        {
            self.log.set_label(target_id, Some(label))?;
        }
        self.session_plugin_driver()
            .session_tree(&SessionTreeEvent {
                new_leaf_id: self.log.leaf_id(),
                old_leaf_id: previous_leaf,
                summary_entry: None,
                from_extension: None,
            })
            .await;
        Ok(context)
    }

    pub async fn append_compaction(
        &self,
        mut compaction: CompactionEntry,
    ) -> Result<String, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        let reason = crate::CompactionReason::Manual;
        let document = self.log.load()?;
        let branch_entries = document.branch()?.into_iter().cloned().collect::<Vec<_>>();
        let preparation = prepare_compaction(
            &branch_entries,
            self.compaction_settings,
            &self.context_options,
        )
        .unwrap_or_else(|| CompactionPreparation {
            messages_to_summarize: Vec::new(),
            turn_prefix_messages: Vec::new(),
            retained_tail: compaction.retained_tail.clone(),
            is_split_turn: false,
            tokens_before: compaction.tokens_before,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: self.compaction_settings,
        });
        self.events.publish_compaction_start(reason);
        let (_, signal) = AbortHandle::new();
        let before = self
            .session_plugin_driver()
            .session_before_compact(&SessionBeforeCompactEvent {
                preparation,
                branch_entries,
                custom_instructions: None,
                reason,
                will_retry: false,
                signal,
            })
            .await;
        if before.as_ref().is_some_and(|result| result.cancel) {
            self.session_plugin_driver()
                .session_compact_failed(&SessionCompactFailedEvent {
                    reason,
                    error_message: None,
                    aborted: true,
                    will_retry: false,
                    from_extension: false,
                })
                .await;
            self.events
                .publish_compaction_end(reason, None, true, false, None);
            return Err(SessionError::Cancelled("session compaction"));
        }
        let from_extension = before
            .and_then(|result| result.compaction)
            .map(|replacement| compaction = replacement)
            .is_some();

        let result: Result<String, SessionError> = (|| {
            let id = self
                .log
                .append(SessionEntry::Compaction(compaction.clone()))?;
            let context = self
                .log
                .load()?
                .context_with_options(&self.context_options)?;
            restore_runtime_context(&self.runtime, &context)?;
            Ok(id)
        })();

        match result {
            Ok(id) => {
                let entry = SessionEntry::Compaction(compaction.clone());
                self.events.publish_entry(entry);
                self.session_plugin_driver()
                    .session_compact(&SessionCompactEvent {
                        compaction_entry: compaction.clone(),
                        from_extension,
                        reason,
                        will_retry: false,
                    })
                    .await;
                self.events
                    .publish_compaction_end(reason, Some(compaction), false, false, None);
                Ok(id)
            }
            Err(error) => {
                self.session_plugin_driver()
                    .session_compact_failed(&SessionCompactFailedEvent {
                        reason,
                        error_message: Some(error.to_string()),
                        aborted: false,
                        will_retry: false,
                        from_extension,
                    })
                    .await;
                self.events.publish_compaction_end(
                    reason,
                    None,
                    false,
                    false,
                    Some(error.to_string()),
                );
                Err(error)
            }
        }
    }

    /// Generates, persists, and activates a Pi-style compaction checkpoint.
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<CompactionEntry, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        self.run_compaction_locked(
            crate::CompactionReason::Manual,
            false,
            custom_instructions,
            false,
        )
        .await
        .map(|(_, compaction)| compaction)
    }

    /// Aborts an in-flight summary request without waiting for the session
    /// operation gate, matching Pi's `abortCompaction` behavior.
    pub fn abort_compaction(&self) {
        if let Some(handle) = self
            .compaction_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            handle.abort();
        }
    }

    async fn maybe_threshold_compact_locked(&self) {
        let Some(context_window) = self.context_window else {
            return;
        };
        let Ok(document) = self.log.load() else {
            return;
        };
        let Ok(branch) = document.branch() else {
            return;
        };
        let entries = branch.into_iter().cloned().collect::<Vec<_>>();
        let context = crate::build_session_context(&entries, &self.context_options);
        let tokens = estimate_session_context_tokens(&entries, &context.messages).tokens;
        if should_compact(tokens, context_window, self.compaction_settings) {
            let _ = self
                .run_compaction_locked(crate::CompactionReason::Threshold, false, None, false)
                .await;
        }
    }

    fn is_overflow_outcome(&self, outcome: &AgentLoopOutcome) -> bool {
        let Some(Message::Assistant(message)) = outcome.final_context.messages.last() else {
            return false;
        };
        let explicit_context_error = message.stop_reason == StopReason::Error
            && message.error_message.as_deref().is_some_and(|error| {
                let error = error.to_ascii_lowercase();
                error.contains("context")
                    && ["length", "window", "token", "overflow", "too long"]
                        .iter()
                        .any(|needle| error.contains(needle))
            });
        if explicit_context_error {
            return true;
        }
        let Some(context_window) = self.context_window else {
            return false;
        };
        message.stop_reason == StopReason::Length
            && should_compact(
                estimate_context_tokens(
                    &outcome
                        .final_context
                        .messages
                        .iter()
                        .cloned()
                        .map(crate::AgentMessage::from)
                        .collect::<Vec<_>>(),
                )
                .tokens,
                context_window,
                self.compaction_settings,
            )
    }

    async fn run_compaction_locked(
        &self,
        reason: crate::CompactionReason,
        will_retry: bool,
        custom_instructions: Option<String>,
        remove_failed_assistant: bool,
    ) -> Result<(String, CompactionEntry), SessionError> {
        let document = self.log.load()?;
        let branch_entries = document.branch()?.into_iter().cloned().collect::<Vec<_>>();
        let mut preparation_entries = branch_entries.clone();
        if remove_failed_assistant
            && let Some(index) = preparation_entries.iter().rposition(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Message(message)
                        if matches!(
                            message.message.as_standard(),
                            Some(Message::Assistant(assistant))
                                if matches!(assistant.stop_reason, StopReason::Error | StopReason::Length)
                        )
                )
            })
        {
            preparation_entries.remove(index);
        }
        let Some(preparation) = prepare_compaction(
            &preparation_entries,
            self.compaction_settings,
            &self.context_options,
        ) else {
            return Err(SessionError::InvalidEntry(
                "there is no uncompacted history to compact".to_string(),
            ));
        };
        self.events.publish_compaction_start(reason);

        let (abort_handle, signal) = AbortHandle::new();
        *self
            .compaction_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(abort_handle);
        let before = self
            .session_plugin_driver()
            .session_before_compact(&SessionBeforeCompactEvent {
                preparation: preparation.clone(),
                branch_entries,
                custom_instructions: custom_instructions.clone(),
                reason,
                will_retry,
                signal: signal.clone(),
            })
            .await;
        if signal.is_aborted() || before.as_ref().is_some_and(|result| result.cancel) {
            self.clear_compaction_abort();
            self.session_plugin_driver()
                .session_compact_failed(&SessionCompactFailedEvent {
                    reason,
                    error_message: None,
                    aborted: true,
                    will_retry,
                    from_extension: false,
                })
                .await;
            self.events
                .publish_compaction_end(reason, None, true, will_retry, None);
            return Err(SessionError::Cancelled("session compaction"));
        }

        let replacement = before.and_then(|result| result.compaction);
        let from_extension = replacement.is_some();
        let generated = match replacement {
            Some(compaction) => Ok(compaction),
            None => generate_compaction(
                &preparation,
                &self.runtime,
                custom_instructions.as_deref(),
                self.runtime.agent().state().thinking_level,
                signal,
            )
            .await
            .map_err(|error| match error {
                CompactionError::Aborted(_) => SessionError::Cancelled("session compaction"),
                CompactionError::SummarizationFailed(message) => {
                    SessionError::Runtime(format!("compaction summarization failed: {message}"))
                }
            }),
        };
        self.clear_compaction_abort();
        let compaction = match generated {
            Ok(compaction) => compaction,
            Err(error) => {
                self.session_plugin_driver()
                    .session_compact_failed(&SessionCompactFailedEvent {
                        reason,
                        error_message: Some(error.to_string()),
                        aborted: matches!(error, SessionError::Cancelled(_)),
                        will_retry,
                        from_extension,
                    })
                    .await;
                self.events.publish_compaction_end(
                    reason,
                    None,
                    matches!(error, SessionError::Cancelled(_)),
                    will_retry,
                    Some(error.to_string()),
                );
                return Err(error);
            }
        };

        let persisted: Result<String, SessionError> = (|| {
            let id = self
                .log
                .append(SessionEntry::Compaction(compaction.clone()))?;
            let context = self
                .log
                .load()?
                .context_with_options(&self.context_options)?;
            restore_runtime_context(&self.runtime, &context)?;
            Ok(id)
        })();
        match persisted {
            Ok(id) => {
                self.events
                    .publish_entry(SessionEntry::Compaction(compaction.clone()));
                self.session_plugin_driver()
                    .session_compact(&SessionCompactEvent {
                        compaction_entry: compaction.clone(),
                        from_extension,
                        reason,
                        will_retry,
                    })
                    .await;
                self.events.publish_compaction_end(
                    reason,
                    Some(compaction.clone()),
                    false,
                    will_retry,
                    None,
                );
                Ok((id, compaction))
            }
            Err(error) => {
                self.session_plugin_driver()
                    .session_compact_failed(&SessionCompactFailedEvent {
                        reason,
                        error_message: Some(error.to_string()),
                        aborted: false,
                        will_retry,
                        from_extension,
                    })
                    .await;
                self.events.publish_compaction_end(
                    reason,
                    None,
                    false,
                    will_retry,
                    Some(error.to_string()),
                );
                Err(error)
            }
        }
    }

    fn clear_compaction_abort(&self) {
        self.compaction_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    fn ensure_open(&self) -> Result<(), SessionError> {
        match self.lifecycle_state.load(Ordering::Acquire) {
            SESSION_OPEN => Ok(()),
            SESSION_CLOSED => Err(SessionError::Closed),
            _ => Err(SessionError::Busy),
        }
    }

    pub async fn branch_with_summary(
        &self,
        leaf_id: Option<&str>,
        mut summary: BranchSummaryEntry,
    ) -> Result<String, SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        let document = self.log.load()?;
        document.context_at_with_options(leaf_id, &self.context_options)?;
        let previous_leaf = self.log.leaf_id();
        let preparation = tree_preparation(&document, leaf_id, true)?;
        let (_, signal) = AbortHandle::new();
        let before = self
            .session_plugin_driver()
            .session_before_tree(&SessionBeforeTreeEvent {
                preparation,
                signal,
            })
            .await;
        if before.as_ref().is_some_and(|result| result.cancel) {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        let mut from_extension = false;
        let mut label = None;
        if let Some(result) = before {
            if let Some(replacement) = result.summary {
                summary.summary = replacement.summary;
                summary.details = replacement.details;
                summary.usage = replacement.usage;
                from_extension = true;
            }
            label = result.label;
        }
        self.log.move_lane(crate::MAIN_LANE, leaf_id)?;
        summary.from_id = previous_leaf.clone().unwrap_or_else(|| "root".to_string());
        let id = self
            .log
            .append(SessionEntry::BranchSummary(summary.clone()))?;
        if let Some(label) = label {
            self.log.set_label(&id, Some(label))?;
        }
        let context = self
            .log
            .load()?
            .context_with_options(&self.context_options)?;
        restore_runtime_context(&self.runtime, &context)?;
        self.session_plugin_driver()
            .session_tree(&SessionTreeEvent {
                new_leaf_id: self.log.leaf_id(),
                old_leaf_id: previous_leaf,
                summary_entry: Some(summary),
                from_extension: Some(from_extension),
            })
            .await;
        Ok(id)
    }

    fn attach_agent_bridge(&self) {
        let log = self.log.clone();
        let events = Arc::clone(&self.events);
        let agent = self.runtime.agent().downgrade();
        let activity = Arc::clone(&self.activity);
        self.runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let log = log.clone();
                let events = Arc::clone(&events);
                let agent = agent.clone();
                let activity = Arc::clone(&activity);
                async move {
                    let display_text = product_display_text(&activity, &event);
                    let (persisted_entry, queue_snapshot) =
                        if let AgentEvent::MessageEnd { message } = &event {
                            let mut activity = activity
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let matched = activity.active_run.as_ref().and_then(|run| {
                                run.pending
                                    .iter()
                                    .position(|pending| pending_message_matches(pending, message))
                            });
                            if let Some(index) = matched {
                                let run = activity
                                    .active_run
                                    .as_mut()
                                    .expect("matched pending message requires active run");
                                let pending = run.pending.remove(index);
                                log.append_entry(pending.target.clone(), MAIN_LANE)
                                    .map_err(|error| EventError(error.to_string()))?;
                                let queue_snapshot =
                                    pending.kind.is_some().then(|| activity.queue_snapshot());
                                (Some(pending.target.entry), queue_snapshot)
                            } else {
                                let entry = SessionEntry::message(message.clone());
                                log.append(entry.clone())
                                    .map_err(|error| EventError(error.to_string()))?;
                                (Some(entry), None)
                            }
                        } else {
                            (None, None)
                        };
                    if matches!(
                        &event,
                        AgentEvent::MessageEnd { message }
                            if matches!(message, Message::Assistant(_))
                    ) {
                        log.materialize()
                            .map_err(|error| EventError(error.to_string()))?;
                    }
                    let Some(agent_state) = agent.state() else {
                        return Ok(());
                    };
                    events.publish_agent(
                        project_product_user_event(event, display_text.as_deref()),
                        agent_state,
                    );
                    if let Some(snapshot) = queue_snapshot {
                        events.publish_queue(snapshot);
                    }
                    if let Some(entry) = persisted_entry {
                        events.publish_entry(entry);
                    } else {
                        // Non-message agent events have no corresponding entry.
                    }
                    Ok(())
                }
            },
        ));
    }
}

fn pending_message_matches(pending: &PendingSessionMessage, message: &Message) -> bool {
    match (&pending.message, message) {
        (Message::User(expected), Message::User(actual)) => expected.content == actual.content,
        _ => pending.message == *message,
    }
}

fn product_display_text(
    activity: &Arc<std::sync::Mutex<SessionActivity>>,
    event: &AgentEvent,
) -> Option<String> {
    let message = match event {
        AgentEvent::MessageStart {
            message: message @ Message::User(_),
        }
        | AgentEvent::MessageEnd {
            message: message @ Message::User(_),
        } => message,
        _ => return None,
    };
    let activity = activity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    activity
        .active_run
        .as_ref()?
        .pending
        .iter()
        .find(|pending| pending_message_matches(pending, message))
        .map(|pending| pending.display_text.clone())
}

fn project_product_user_event(event: AgentEvent, display_text: Option<&str>) -> AgentEvent {
    let Some(display_text) = display_text else {
        return event;
    };
    let display_message = |message: Message| match message {
        Message::User(user) => {
            let mut displayed = UserMessage::text(display_text, user.timestamp_ms);
            displayed.content.extend(
                user.content
                    .iter()
                    .filter(|block| matches!(block, ContentBlock::Image(_)))
                    .cloned(),
            );
            Message::User(displayed)
        }
        other => other,
    };
    match event {
        AgentEvent::MessageStart { message } => AgentEvent::MessageStart {
            message: display_message(message),
        },
        AgentEvent::MessageEnd { message } => AgentEvent::MessageEnd {
            message: display_message(message),
        },
        other => other,
    }
}

fn recover_pending_queue(document: &SessionDocument) -> Vec<PendingSessionMessage> {
    let persisted = document
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let cancelled = document
        .records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::QueueCancelled { entry_id, .. } => Some(entry_id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    document
        .records
        .iter()
        .filter_map(|record| {
            let LaneRecordEntry::QueueEnqueued {
                queue,
                run_id,
                target,
            } = &record.record
            else {
                return None;
            };
            if persisted.contains(target.id.as_str()) || cancelled.contains(target.id.as_str()) {
                return None;
            }
            let SessionEntry::Message(message_entry) = &target.entry else {
                return None;
            };
            let message = message_entry.message.as_standard()?.clone();
            let Message::User(user) = &message else {
                return None;
            };
            let text = user
                .content
                .iter()
                .filter_map(|content| match content {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(PendingSessionMessage {
                kind: Some(*queue),
                run_id: run_id.clone(),
                display_text: message_entry
                    .message
                    .display_text()
                    .unwrap_or(&text)
                    .to_string(),
                message,
                target: target.clone(),
            })
        })
        .collect()
}

fn recover_interrupted_state(
    log: &SessionLog,
    document: &SessionDocument,
) -> Result<Vec<PendingSessionMessage>, SessionError> {
    let mut recovered = recover_pending_queue(document);
    for item in &mut recovered {
        if item.run_id.is_none() {
            continue;
        }
        log.append_record(NewLaneRecord {
            id: next_unique_id("queue-cancel"),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::QueueCancelled {
                run_id: item.run_id.clone(),
                entry_id: item.target.id.clone(),
            },
        })?;
        item.target.id = next_unique_id("entry");
        item.run_id = None;
        log.append_record(NewLaneRecord {
            id: next_unique_id("queue-recovery"),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::QueueEnqueued {
                queue: QueueKind::NextRun,
                run_id: None,
                target: item.target.clone(),
            },
        })?;
    }
    let finished = document
        .records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::OperationFinished { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    for operation in document.records.iter().filter(|record| {
        matches!(record.record, LaneRecordEntry::OperationStarted { .. })
            && !finished.contains(record.id.as_str())
    }) {
        log.append_record(NewLaneRecord {
            id: next_unique_id("recovery"),
            lane: operation.lane.clone(),
            record: LaneRecordEntry::OperationFinished {
                run_id: operation.id.clone(),
                outcome: OperationOutcome::Aborted,
                error: Some(OperationError {
                    code: "interrupted".to_string(),
                    message: "operation was interrupted by process termination".to_string(),
                }),
            },
        })?;
    }
    Ok(recovered)
}

fn tree_preparation(
    document: &SessionDocument,
    target_id: Option<&str>,
    user_wants_summary: bool,
) -> Result<TreePreparation, SessionError> {
    let old_branch = document.branch()?;
    let target_branch = document.branch_at(target_id)?;
    let common_len = old_branch
        .iter()
        .zip(&target_branch)
        .take_while(|(old, target)| old.id == target.id)
        .count();
    let common_ancestor_id = common_len
        .checked_sub(1)
        .map(|index| old_branch[index].id.clone());
    Ok(TreePreparation {
        target_id: target_id.map(str::to_string),
        old_leaf_id: document.leaf_id(crate::MAIN_LANE)?.map(str::to_string),
        common_ancestor_id,
        entries_to_summarize: old_branch[common_len..]
            .iter()
            .map(|entry| (*entry).clone())
            .collect(),
        user_wants_summary,
        custom_instructions: None,
        replace_instructions: false,
        label: None,
    })
}

fn session_identity(header: &SessionHeader, path: PathBuf) -> SessionIdentity {
    SessionIdentity {
        id: header.id.clone(),
        path,
        cwd: header.cwd.clone(),
        parent_session_id: header.parent_session_id.clone(),
    }
}

fn restore_runtime_context(
    runtime: &PiRuntime,
    context: &SessionContext,
) -> Result<(), SessionError> {
    restore_runtime_context_with_request(
        runtime,
        context,
        crate::InitialModelRequest::default().session(context.model.clone()),
    )
}

fn restore_runtime_context_with_request(
    runtime: &PiRuntime,
    context: &SessionContext,
    request: crate::InitialModelRequest,
) -> Result<(), SessionError> {
    let current = runtime.agent().state();
    let selection = crate::ModelRuntimeServices::new(runtime)
        .resolve_initial_model(request)
        .map_err(|error| SessionError::Runtime(error.to_string()))?;
    let SessionModel { provider, model_id } = selection.model;
    runtime.restore_state(RuntimeRestoreState {
        provider_id: provider,
        model_id,
        thinking_level: context
            .thinking_level
            .parse()
            .map_err(SessionError::InvalidPayload)?,
        active_tools: context
            .active_tool_names
            .clone()
            .unwrap_or(current.active_tools),
        messages: context.provider_messages(),
    })?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn read_prompt_snapshot(path: &Path) -> Result<Option<PromptSnapshot>, SessionError> {
    let (_, document) = SessionLog::open(path)?;
    Ok(document.latest_prompt_snapshot())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use pi_agent::{AgentLoopStop, AgentOptions};
    use pi_core::{
        AgentPlugin, Command, CommandContext, CommandError, CommandOutcome, CommandSpec,
        ContentBlock, Message, PluginId, RegisterContext, ResponseMetadata, StreamEvent, Usage,
        UserMessage,
    };
    use pi_plugin_faux_provider::{FauxProviderPlugin, FauxTurn};
    use pi_plugin_test_tools::TestToolsPlugin;
    use pi_runtime::SystemPrompt;

    use super::*;
    use crate::{
        EntryOrder, EntryQuery, SUMMARIZATION_SYSTEM_PROMPT, SessionBeforeCompactResult,
        SessionBeforeTreeResult, SessionEntryType, SessionPlugin, SessionPluginContext,
        SessionPluginError, SessionPlugins,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn text_turn_with_usage(text: &str, total_tokens: u64) -> FauxTurn {
        FauxTurn::Events(vec![
            StreamEvent::Start {
                metadata: ResponseMetadata::new("faux".into(), "test".into(), "faux", 0),
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

    fn faux_runtime(turns: impl IntoIterator<Item = FauxTurn>) -> PiRuntime {
        PiRuntime::builder()
            .provider_plugin(FauxProviderPlugin::scripted(turns))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap()
    }

    struct ExpandingCommandPlugin;

    #[async_trait]
    impl AgentPlugin for ExpandingCommandPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("expanding-command")
        }

        fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
            context.register_command(Arc::new(ExpandingCommand))
        }
    }

    struct ExpandingCommand;

    #[async_trait]
    impl Command for ExpandingCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: "review".to_string(),
                description: "Expand a review request".to_string(),
                argument_hint: Some("[focus]".to_string()),
            }
        }

        async fn execute(
            &self,
            _context: CommandContext,
            arguments: String,
        ) -> Result<CommandOutcome, CommandError> {
            Ok(CommandOutcome::TransformInput(format!(
                "Run the private review prompt for {}",
                arguments.trim()
            )))
        }
    }

    #[tokio::test]
    async fn transformed_command_persists_model_text_and_original_display_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .agent_plugin(ExpandingCommandPlugin)
            .provider_plugin(FauxProviderPlugin::scripted([FauxTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();

        session.submit("/review accessibility").await.unwrap();

        let (_, document) = SessionLog::open(&path).unwrap();
        let user = document
            .messages()
            .into_iter()
            .find(|message| message.role() == "user")
            .unwrap();
        assert!(matches!(
            user.as_standard(),
            Some(Message::User(message))
                if matches!(&message.content[0], ContentBlock::Text(text)
                    if text.text == "Run the private review prompt for accessibility")
        ));
        assert_eq!(user.display_text(), Some("/review accessibility"));
    }

    #[tokio::test]
    async fn transformed_command_publishes_the_original_text_to_product_frontends() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = PiRuntime::builder()
            .agent_plugin(ExpandingCommandPlugin)
            .provider_plugin(FauxProviderPlugin::scripted([FauxTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let mut subscription = session.subscribe();

        session.submit("/review accessibility").await.unwrap();

        let displayed = std::iter::from_fn(|| subscription.events.try_recv().ok()).find_map(
            |event| match event.event {
                crate::AgentSessionEvent::Agent(event) => match *event {
                    AgentEvent::MessageStart {
                        message: Message::User(user),
                    } => Some(
                        user.content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::Text(text) => Some(text.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                },
                _ => None,
            },
        );
        assert_eq!(displayed.as_deref(), Some("/review accessibility"));
    }

    #[tokio::test]
    async fn queued_command_restores_the_original_text_after_process_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queued-command.jsonl");
        let runtime = PiRuntime::builder()
            .agent_plugin(ExpandingCommandPlugin)
            .provider_plugin(FauxProviderPlugin::scripted([FauxTurn::WaitForAbort]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = Arc::new(AgentSession::create(runtime, &path).await.unwrap());
        let running = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.submit("first").await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !session.snapshot().agent.is_running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        session.steer("/review accessibility").await.unwrap();
        assert_eq!(
            session.snapshot().queue.steering,
            vec!["/review accessibility".to_string()]
        );
        session.abort();
        running.await.unwrap().unwrap();
        session.shutdown().await;
        drop(session);

        let reopened = AgentSession::open(faux_runtime([]), &path).await.unwrap();
        assert_eq!(
            reopened.snapshot().queue.follow_up,
            vec!["/review accessibility".to_string()]
        );
    }

    #[tokio::test]
    async fn new_session_is_saved_only_after_the_first_assistant_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions/session.jsonl");
        let session = AgentSession::create(
            faux_runtime([FauxTurn::Text("first answer".to_string())]),
            &path,
        )
        .await
        .unwrap();

        assert!(!path.exists());
        assert!(!session.log().is_materialized());
        session
            .execute_shell("printf shell-only", ShellExecutionOptions::default())
            .await
            .unwrap();
        assert!(!path.exists());

        session.prompt("hello").await.unwrap();

        assert!(path.exists());
        assert!(session.log().is_materialized());
        let (_, document) = SessionLog::open(&path).unwrap();
        assert!(document.messages().iter().any(|message| {
            matches!(
                message.as_standard(),
                Some(Message::User(user))
                    if matches!(
                        &user.content[0],
                        ContentBlock::Text(text) if text.text == "hello"
                    )
            )
        }));
        assert!(
            document
                .messages()
                .iter()
                .any(|message| { matches!(message.as_standard(), Some(Message::Assistant(_))) })
        );
    }

    struct LifecyclePlugin {
        value: usize,
        contexts: Arc<Mutex<Vec<SessionPluginContext>>>,
        events: Arc<Mutex<Vec<String>>>,
        cancel_tree: bool,
        replace_compaction: bool,
    }

    impl LifecyclePlugin {
        fn record(&self, event: impl Into<String>) {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event.into());
        }

        fn record_context(&self, context: &SessionPluginContext) {
            self.contexts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(context.clone());
        }
    }

    #[async_trait]
    impl SessionPlugin for LifecyclePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("lifecycle")
        }

        async fn session_start(
            &self,
            context: &SessionPluginContext,
            event: &SessionStartEvent,
        ) -> Result<(), SessionPluginError> {
            self.record_context(context);
            self.record(format!("start:{}:{:?}", self.value, event.reason));
            Ok(())
        }

        async fn session_shutdown(
            &self,
            context: &SessionPluginContext,
            event: &SessionShutdownEvent,
        ) -> Result<(), SessionPluginError> {
            self.record_context(context);
            self.record(format!("shutdown:{}:{:?}", self.value, event.reason));
            Ok(())
        }

        async fn session_before_tree(
            &self,
            context: &SessionPluginContext,
            _event: &SessionBeforeTreeEvent,
        ) -> Result<Option<SessionBeforeTreeResult>, SessionPluginError> {
            self.record_context(context);
            self.record(format!("before_tree:{}", self.value));
            Ok(self.cancel_tree.then_some(SessionBeforeTreeResult {
                cancel: true,
                ..SessionBeforeTreeResult::default()
            }))
        }

        async fn session_before_compact(
            &self,
            context: &SessionPluginContext,
            event: &SessionBeforeCompactEvent,
        ) -> Result<Option<SessionBeforeCompactResult>, SessionPluginError> {
            self.record_context(context);
            self.record(format!("before_compact:{}", self.value));
            if !self.replace_compaction {
                return Ok(None);
            }
            let compaction = CompactionEntry {
                summary: "extension summary".to_string(),
                retained_tail: event.preparation.retained_tail.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
                usage: None,
            };
            Ok(Some(SessionBeforeCompactResult {
                cancel: false,
                compaction: Some(compaction),
            }))
        }

        async fn session_compact(
            &self,
            context: &SessionPluginContext,
            event: &SessionCompactEvent,
        ) -> Result<(), SessionPluginError> {
            self.record_context(context);
            self.record(format!(
                "compact:{}:{}:{}",
                self.value, event.compaction_entry.summary, event.from_extension
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn create_records_v4_configuration_messages_and_prompt_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .provider_plugin(FauxProviderPlugin::scripted([FauxTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                thinking_level: ThinkingLevel::High,
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();

        let outcome = session.prompt("hello").await.unwrap();
        assert_eq!(outcome.new_messages.len(), 2);
        let document = session.log().load().unwrap();
        assert_eq!(document.header.version, 4);
        assert_eq!(document.messages().len(), 2);
        assert!(document.latest_prompt_snapshot().is_some());
        assert_eq!(document.context().unwrap().thinking_level, "high");

        let values = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values[0]["version"], 4);
        assert_eq!(values[1]["type"], "model_change");
        assert_eq!(values[2]["type"], "thinking_level_change");
        assert_eq!(values[3]["type"], "active_tools_change");
    }

    #[tokio::test]
    async fn durable_queue_revisions_survive_abort_and_process_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queued-session.jsonl");
        let session = Arc::new(
            AgentSession::create(faux_runtime([FauxTurn::WaitForAbort]), &path)
                .await
                .unwrap(),
        );
        let mut subscription = session.subscribe();
        let running = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.submit("first").await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !session.snapshot().agent.is_running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let queued = session.steer("keep this draft").await.unwrap();
        assert!(matches!(
            queued,
            SubmitOutcome::Queued {
                kind: QueueKind::Steer,
                ..
            }
        ));
        assert_eq!(
            session.snapshot().queue.steering,
            vec!["keep this draft".to_string()]
        );
        session.abort();
        let outcome = running.await.unwrap().unwrap();
        assert!(matches!(outcome, SubmitOutcome::Agent(_)));
        assert_eq!(
            session.snapshot().queue.steering,
            vec!["keep this draft".to_string()]
        );

        let mut revisions = Vec::new();
        let mut saw_queue = false;
        let mut saw_settled = false;
        while let Ok(event) = subscription.events.try_recv() {
            if event.revision <= subscription.snapshot.revision {
                continue;
            }
            revisions.push(event.revision);
            saw_queue |= matches!(event.event, crate::AgentSessionEvent::QueueUpdate { .. });
            saw_settled |= matches!(event.event, crate::AgentSessionEvent::AgentSettled);
        }
        assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(saw_queue && saw_settled);
        session.shutdown().await;
        drop(session);

        let reopened = AgentSession::open(faux_runtime([]), &path).await.unwrap();
        assert_eq!(
            reopened.snapshot().queue.follow_up,
            vec!["keep this draft".to_string()]
        );
        let restored = reopened.clear_queue().unwrap();
        assert_eq!(restored.follow_up, vec!["keep this draft".to_string()]);
        assert!(reopened.snapshot().queue.follow_up.is_empty());
        let document = reopened.log().load().unwrap();
        crate::validate_record_log(&crate::RecordLogSlice {
            lane: MAIN_LANE.to_string(),
            open_operations: reopened
                .log()
                .find_open_operations(MAIN_LANE, None)
                .unwrap(),
            records: document.records,
            entries: document.entries,
        })
        .unwrap();
    }

    #[tokio::test]
    async fn open_and_checkout_restore_the_selected_v4_branch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(FauxProviderPlugin::scripted([
                FauxTurn::Text("first answer".to_string()),
                FauxTurn::Text("abandoned answer".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["echo".to_string()],
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();
        session.prompt("first").await.unwrap();
        let first_assistant = session
            .log()
            .find_entries(&EntryQuery {
                entry_type: Some(SessionEntryType::Message),
                order: EntryOrder::OldestFirst,
                ..EntryQuery::default()
            })
            .unwrap()
            .into_iter()
            .find(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Message(message)
                        if matches!(message.message.as_standard(), Some(Message::Assistant(_)))
                )
            })
            .unwrap()
            .id;
        session.prompt("abandoned").await.unwrap();
        let context = session.checkout(Some(&first_assistant)).await.unwrap();
        assert_eq!(context.messages.len(), 2);

        drop(session);
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(FauxProviderPlugin::scripted([]))
            .agent_options(AgentOptions {
                active_tools: vec!["echo".to_string()],
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let reopened = AgentSession::open(runtime, &path).await.unwrap();
        assert_eq!(reopened.runtime().agent().state().messages.len(), 2);
        assert_eq!(
            reopened.log().leaf_id().as_deref(),
            Some(first_assistant.as_str())
        );
    }

    #[tokio::test]
    async fn compaction_uses_persisted_retained_tail() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = PiRuntime::builder()
            .provider_plugin(FauxProviderPlugin::scripted([]))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();
        session
            .log()
            .append_message(Message::User(UserMessage::text("old", 1)))
            .unwrap();
        session
            .append_compaction(CompactionEntry {
                summary: "short".to_string(),
                retained_tail: vec![Message::User(UserMessage::text("retained", 2)).into()],
                tokens_before: 100,
                details: None,
                usage: None,
            })
            .await
            .unwrap();
        let messages = &session.runtime().agent().state().messages;
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::User(user)
            if matches!(&user.content[0], ContentBlock::Text(text) if text.text.contains("short"))));
    }

    #[tokio::test]
    async fn session_plugin_reload_emits_shutdown_then_start_and_exposes_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .provider_plugin(FauxProviderPlugin::scripted([]))
            .agent_options(AgentOptions {
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let builds = Arc::new(AtomicUsize::new(0));
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let plugins = SessionPlugins::new().plugin_factory({
            let builds = Arc::clone(&builds);
            let contexts = Arc::clone(&contexts);
            let events = Arc::clone(&events);
            move || LifecyclePlugin {
                value: builds.fetch_add(1, Ordering::SeqCst) + 1,
                contexts: Arc::clone(&contexts),
                events: Arc::clone(&events),
                cancel_tree: false,
                replace_compaction: false,
            }
        });
        let session = AgentSession::create_with_options(
            runtime,
            &path,
            AgentSessionOptions::default().plugins(plugins),
        )
        .await
        .unwrap();

        let previous_driver = session.session_plugin_driver();
        let report = session.reload_session_plugins().await.unwrap();
        assert_eq!(report.previous_generation, 1);
        assert_eq!(report.generation, 2);
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(report.plugin_order, vec![PluginId::new("lifecycle")]);
        assert_eq!(previous_driver.generation(), 1);
        assert_eq!(session.session_plugin_driver().generation(), 2);
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                "start:1:Startup".to_string(),
                "shutdown:1:Reload".to_string(),
                "start:2:Reload".to_string(),
            ]
        );

        let contexts = contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(contexts.len(), 3);
        assert_eq!(contexts[0].plugin_id, PluginId::new("lifecycle"));
        assert_eq!(contexts[0].generation, 1);
        assert_eq!(contexts[1].generation, 1);
        assert_eq!(contexts[2].generation, 2);
        assert_eq!(contexts[0].session.id, session.log().header().id);
        assert_eq!(contexts[0].session.path, path);
        assert_eq!(contexts[0].session.cwd, directory.path());
    }

    #[tokio::test]
    async fn failed_session_plugin_reload_keeps_previous_generation_running() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .provider_plugin(FauxProviderPlugin::scripted([]))
            .build()
            .unwrap();
        let builds = Arc::new(AtomicUsize::new(0));
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let plugins = SessionPlugins::new().try_plugin_factory({
            let builds = Arc::clone(&builds);
            let contexts = Arc::clone(&contexts);
            let events = Arc::clone(&events);
            move || {
                let value = builds.fetch_add(1, Ordering::SeqCst) + 1;
                if value > 1 {
                    Err("fixture reload failed")
                } else {
                    Ok(LifecyclePlugin {
                        value,
                        contexts: Arc::clone(&contexts),
                        events: Arc::clone(&events),
                        cancel_tree: false,
                        replace_compaction: false,
                    })
                }
            }
        });
        let session = AgentSession::create_with_options(
            runtime,
            &path,
            AgentSessionOptions::default().plugins(plugins),
        )
        .await
        .unwrap();

        let previous_driver = session.session_plugin_driver();
        let error = session.reload_session_plugins().await.unwrap_err();
        assert!(error.to_string().contains("fixture reload failed"));
        assert_eq!(session.session_plugin_driver().generation(), 1);
        assert!(Arc::ptr_eq(
            &previous_driver,
            &session.session_plugin_driver()
        ));
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["start:1:Startup".to_string()]
        );
    }

    #[tokio::test]
    async fn lifecycle_hooks_can_replace_compaction_and_cancel_tree_navigation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .provider_plugin(FauxProviderPlugin::scripted([]))
            .build()
            .unwrap();
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let plugins = SessionPlugins::new().plugin(LifecyclePlugin {
            value: 1,
            contexts,
            events: Arc::clone(&events),
            cancel_tree: true,
            replace_compaction: true,
        });
        let session = AgentSession::create_with_options(
            runtime,
            &path,
            AgentSessionOptions::default().plugins(plugins),
        )
        .await
        .unwrap();
        let leaf = session
            .log()
            .append_message(Message::User(UserMessage::text("branch", 1)))
            .unwrap();

        let error = session.checkout(None).await.unwrap_err();
        assert!(matches!(error, SessionError::Cancelled(_)));
        assert_eq!(session.log().leaf_id().as_deref(), Some(leaf.as_str()));

        session
            .append_compaction(CompactionEntry {
                summary: "default summary".to_string(),
                retained_tail: Vec::new(),
                tokens_before: 10,
                details: None,
                usage: None,
            })
            .await
            .unwrap();
        let document = session.log().load().unwrap();
        assert!(document.entries.iter().any(|record| {
            matches!(
                &record.entry,
                SessionEntry::Compaction(entry) if entry.summary == "extension summary"
            )
        }));
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                "start:1:Startup".to_string(),
                "before_tree:1".to_string(),
                "before_compact:1".to_string(),
                "compact:1:extension summary:true".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn manual_compaction_uses_an_isolated_tool_free_completion() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = FauxProviderPlugin::scripted([
            FauxTurn::Text("answer".to_string()),
            FauxTurn::Text("## Original Request\nKeep going".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().compaction(CompactionSettings {
                keep_recent_tokens: 1,
                ..CompactionSettings::default()
            }),
        )
        .await
        .unwrap();

        session.prompt("do the work").await.unwrap();
        let compaction = session.compact(None).await.unwrap();

        assert!(compaction.summary.contains("Keep going"));
        assert!(!compaction.retained_tail.is_empty());
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].tools.is_empty());
        assert_eq!(requests[1].system_prompt, SUMMARIZATION_SYSTEM_PROMPT);
        assert_eq!(
            requests[1].max_output_tokens,
            Some(CompactionSettings::default().reserve_tokens / 2)
        );
        assert!(
            session
                .log()
                .load()
                .unwrap()
                .entries
                .iter()
                .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
        );
    }

    #[tokio::test]
    async fn threshold_compaction_runs_after_a_completed_turn() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = FauxProviderPlugin::scripted([
            text_turn_with_usage("large answer", 200),
            FauxTurn::Text("threshold summary".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let settings = CompactionSettings {
            reserve_tokens: 10,
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        };
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default()
                .compaction(settings)
                .context_window(100),
        )
        .await
        .unwrap();

        let outcome = session.prompt("large request").await.unwrap();

        assert_eq!(outcome.stop, AgentLoopStop::Completed);
        assert_eq!(provider.requests().len(), 2);
        assert!(session.log().load().unwrap().entries.iter().any(|record| {
            matches!(
                &record.entry,
                SessionEntry::Compaction(entry) if entry.summary.contains("threshold summary")
            )
        }));
    }

    #[tokio::test]
    async fn overflow_compaction_drops_the_failed_assistant_and_retries_once() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = FauxProviderPlugin::scripted([
            FauxTurn::Error("context window token length exceeded".to_string()),
            FauxTurn::Text("overflow summary".to_string()),
            FauxTurn::Text("recovered answer".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().compaction(CompactionSettings {
                keep_recent_tokens: 1,
                ..CompactionSettings::default()
            }),
        )
        .await
        .unwrap();

        let outcome = session.prompt("recover this request").await.unwrap();

        assert_eq!(outcome.stop, AgentLoopStop::Completed);
        assert_eq!(provider.requests().len(), 3);
        assert!(
            matches!(outcome.final_context.messages.last(), Some(Message::Assistant(message))
            if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "recovered answer"))
        );
        let document = session.log().load().unwrap();
        let compaction = document
            .entries
            .iter()
            .find_map(|record| match &record.entry {
                SessionEntry::Compaction(entry) => Some(entry),
                _ => None,
            });
        assert!(compaction.is_some());
        assert!(compaction.unwrap().retained_tail.iter().all(|message| {
            !matches!(message.as_standard(), Some(Message::Assistant(assistant)) if assistant.stop_reason == StopReason::Error)
        }));
    }

    #[tokio::test]
    async fn manual_compaction_can_be_aborted_without_waiting_for_the_operation_gate() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = FauxProviderPlugin::scripted([
            FauxTurn::Text("answer".to_string()),
            FauxTurn::WaitForAbort,
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("faux"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().compaction(CompactionSettings {
                keep_recent_tokens: 1,
                ..CompactionSettings::default()
            }),
        )
        .await
        .unwrap();
        session.prompt("work").await.unwrap();

        let compacting = {
            let session = session.clone();
            tokio::spawn(async move { session.compact(None).await })
        };
        for _ in 0..100 {
            if provider.requests().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(provider.requests().len(), 2);
        session.abort_compaction();

        let error = compacting.await.unwrap().unwrap_err();
        assert!(matches!(
            error,
            SessionError::Cancelled("session compaction")
        ));
        assert!(
            !session
                .log()
                .load()
                .unwrap()
                .entries
                .iter()
                .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
        );
    }
}
