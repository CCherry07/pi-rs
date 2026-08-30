use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use pi_agent::{AgentLoopOutcome, AgentLoopStop, EventError, PromptInput, QueueMode};
use pi_core::{
    AbortHandle, AgentEvent, CommandOutcome, ContentBlock, CustomMessage, ImageContent,
    InputStreamingBehavior, Message, ModelId, PluginId, ProviderId, StopReason, ThinkingLevel,
    UserMessage,
};
use pi_prompt::BuildSystemPromptOptions;
use pi_runtime::{
    PiRuntime, PreparedTextSubmission, QueuedTextOutcome, RuntimeCompletionRequest,
    RuntimePromptOutcome, RuntimeRestoreState,
};
use pi_shell::{DEFAULT_TIMEOUT, ShellChunk, ShellRequest, ShellResult};
use pi_telemetry::{
    OperationStartAttributes, RunEnd, RunOperationKind, RunOutcome as TelemetryRunOutcome, RunSpan,
    RunStart, SpanStatus,
};
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
    SessionPluginReloadReport, SessionPlugins, SessionRecord, SessionShutdownEvent,
    SessionShutdownReason, SessionStartEvent, SessionStartReason, SessionTreeEvent,
    ThinkingLevelEntry, TreePreparation, compact as generate_compaction, estimate_context_tokens,
    estimate_session_context_tokens, next_unique_id, now_ms, prepare_compaction, reduce_lane_state,
    should_compact,
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
    /// Optional caller correlation ID (used by Pi RPC bash events).
    pub id: Option<String>,
    pub exclude_from_context: bool,
    pub timeout: Option<std::time::Duration>,
    pub shell_path: Option<PathBuf>,
}

impl Default for ShellExecutionOptions {
    fn default() -> Self {
        Self {
            id: None,
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
            // this pi-rs diagnostic extension without coupling the session
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
    /// Tokens reserved outside abandoned-branch summary input. The selected
    /// model context window supplies the total budget.
    pub branch_summary_reserve_tokens: Option<u64>,
    /// Immutable product registration metadata prepared alongside the runtime
    /// and session plugin generations.
    pub runtime_inventory: SessionRuntimeInventory,
    /// Pi v3-compatible parent session path recorded for a new session.
    pub parent_session_path: Option<PathBuf>,
    /// Generation-local defaults for shell shorthand execution. Explicit
    /// per-call shell paths still take precedence.
    pub shell_path: Option<PathBuf>,
    pub shell_command_prefix: Option<String>,
    /// Session-owned retry policy for transient assistant/provider failures.
    pub retry: AutoRetrySettings,
}

/// Bounded, abortable retry policy used by normal assistant turns.
///
/// The initial provider call does not count toward `max_retries`; attempt one
/// waits `base_delay_ms`, attempt two waits twice that amount, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoRetrySettings {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
}

impl Default for AutoRetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2_000,
        }
    }
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

    pub fn branch_summary_reserve_tokens(mut self, reserve_tokens: u64) -> Self {
        self.branch_summary_reserve_tokens = Some(reserve_tokens);
        self
    }

    pub fn runtime_inventory(mut self, inventory: SessionRuntimeInventory) -> Self {
        self.runtime_inventory = inventory;
        self
    }

    pub fn parent_session_path(mut self, path: Option<PathBuf>) -> Self {
        self.parent_session_path = path;
        self
    }

    pub fn shell(mut self, shell_path: Option<PathBuf>, command_prefix: Option<String>) -> Self {
        self.shell_path = shell_path;
        self.shell_command_prefix = command_prefix;
        self
    }

    pub fn retry(mut self, retry: AutoRetrySettings) -> Self {
        self.retry = retry;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRuntimeInventory {
    js_extensions: Vec<String>,
    configured_native_plugins: Vec<PluginId>,
}

impl SessionRuntimeInventory {
    pub fn new(
        js_extensions: impl IntoIterator<Item = String>,
        configured_native_plugins: impl IntoIterator<Item = PluginId>,
    ) -> Self {
        Self {
            js_extensions: js_extensions.into_iter().collect(),
            configured_native_plugins: configured_native_plugins.into_iter().collect(),
        }
    }

    pub fn js_extensions(&self) -> &[String] {
        &self.js_extensions
    }

    pub fn configured_native_plugins(&self) -> &[PluginId] {
        &self.configured_native_plugins
    }
}

#[derive(Clone)]
pub struct AgentSession {
    runtime: PiRuntime,
    log: SessionLog,
    runtime_inventory: SessionRuntimeInventory,
    context_options: SessionContextBuildOptions,
    session_plugin_sources: SessionPlugins,
    session_plugin_driver: Arc<RwLock<Arc<SessionPluginDriver>>>,
    operation_gate: Arc<tokio::sync::Mutex<()>>,
    compaction_settings: Arc<RwLock<CompactionSettings>>,
    context_window: Option<u64>,
    branch_summary_reserve_tokens: u64,
    compaction_abort: Arc<std::sync::Mutex<Option<AbortHandle>>>,
    lifecycle_state: Arc<AtomicU8>,
    events: Arc<AgentSessionEventHub>,
    activity: Arc<std::sync::Mutex<SessionActivity>>,
    bash_abort: Arc<std::sync::Mutex<Option<(String, AbortHandle)>>>,
    retry_settings: Arc<RwLock<AutoRetrySettings>>,
    retry_attempt: Arc<AtomicU32>,
    retry_abort: Arc<std::sync::Mutex<Option<AbortHandle>>>,
    shell_path: Option<PathBuf>,
    shell_command_prefix: Option<String>,
}

/// A fully constructed session whose `session_start` lifecycle event has not
/// been emitted yet.
///
/// Multi-session hosts use this two-phase form to prepare a replacement while
/// the current session is still valid, then order the lifecycle transition as
/// old `session_shutdown` followed by new `session_start`.
pub struct PreparedAgentSession {
    session: Arc<AgentSession>,
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
    /// Returns a clone of the prepared generation before lifecycle activation.
    ///
    /// Hosts use this to bind generation-scoped capabilities that must already
    /// be available to the subsequent `session_start` callbacks.
    pub fn session(&self) -> Arc<AgentSession> {
        Arc::clone(&self.session)
    }

    pub async fn activate(self, event: SessionStartEvent) -> Arc<AgentSession> {
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
        let session = Self::prepare_create_with_options(runtime, path, options)
            .await?
            .activate(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            })
            .await;
        Ok(Arc::unwrap_or_clone(session))
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
        let initial_model = crate::InitialModelRequest::default()
            .requested(state.provider_id.clone(), state.model_id.as_str());
        let mut header = SessionHeader::new(next_unique_id("session"), runtime.cwd());
        header.legacy_parent_session_path = options.parent_session_path.clone();
        runtime.agent().set_session_id(Some(header.id.clone()));
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
        // A new session records the runtime's already-validated selection.
        // Preserve it even when the model is intentionally absent from the
        // catalog; catalog fallback applies only while restoring old state.
        restore_runtime_context_with_request(&runtime, &context, initial_model)?;
        let activity = Arc::new(std::sync::Mutex::new(SessionActivity::default()));
        let events = AgentSessionEventHub::new(
            runtime.agent().state(),
            log.name(),
            QueueSnapshot::default(),
        );

        let session = Arc::new(Self {
            runtime,
            log,
            runtime_inventory: options.runtime_inventory,
            context_options: options.context,
            session_plugin_sources: options.plugins,
            session_plugin_driver: Arc::new(RwLock::new(session_plugin_driver)),
            operation_gate: Arc::new(tokio::sync::Mutex::new(())),
            compaction_settings: Arc::new(RwLock::new(options.compaction)),
            context_window: options.context_window,
            branch_summary_reserve_tokens: options.branch_summary_reserve_tokens.unwrap_or(16_384),
            compaction_abort: Arc::new(std::sync::Mutex::new(None)),
            lifecycle_state: Arc::new(AtomicU8::new(SESSION_OPEN)),
            events,
            activity,
            bash_abort: Arc::new(std::sync::Mutex::new(None)),
            retry_settings: Arc::new(RwLock::new(options.retry)),
            retry_attempt: Arc::new(AtomicU32::new(0)),
            retry_abort: Arc::new(std::sync::Mutex::new(None)),
            shell_path: options.shell_path,
            shell_command_prefix: options.shell_command_prefix,
        });
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
        let session = Self::prepare_open_with_options(runtime, path, options)
            .await?
            .activate(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            })
            .await;
        Ok(Arc::unwrap_or_clone(session))
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
        mut document: SessionDocument,
        options: AgentSessionOptions,
    ) -> Result<PreparedAgentSession, SessionError> {
        let agent_state = runtime.agent().state();
        let recovery_defaults = crate::EffectiveLaneConfiguration {
            model: SessionModel {
                provider: agent_state.provider_id.clone(),
                model_id: agent_state.model_id.clone(),
            },
            thinking_level: agent_state.thinking_level.as_str().to_string(),
            active_tool_names: runtime.active_tools(),
        };
        let recovered_queue = recover_interrupted_state(&log, &document, recovery_defaults)?;
        // Recovery may commit accepted deferred writes before runtime context is
        // restored, so always project the reconciled document rather than the
        // stale open snapshot.
        document = log.load()?;
        runtime
            .agent()
            .set_session_id(Some(document.header.id.clone()));
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
        let session = Arc::new(Self {
            runtime,
            log,
            runtime_inventory: options.runtime_inventory,
            context_options: options.context,
            session_plugin_sources: options.plugins,
            session_plugin_driver: Arc::new(RwLock::new(session_plugin_driver)),
            operation_gate: Arc::new(tokio::sync::Mutex::new(())),
            compaction_settings: Arc::new(RwLock::new(options.compaction)),
            context_window: options.context_window,
            branch_summary_reserve_tokens: options.branch_summary_reserve_tokens.unwrap_or(16_384),
            compaction_abort: Arc::new(std::sync::Mutex::new(None)),
            lifecycle_state: Arc::new(AtomicU8::new(SESSION_OPEN)),
            events,
            activity,
            bash_abort: Arc::new(std::sync::Mutex::new(None)),
            retry_settings: Arc::new(RwLock::new(options.retry)),
            retry_attempt: Arc::new(AtomicU32::new(0)),
            retry_abort: Arc::new(std::sync::Mutex::new(None)),
            shell_path: options.shell_path,
            shell_command_prefix: options.shell_command_prefix,
        });
        session.attach_agent_bridge();
        Ok(PreparedAgentSession { session })
    }

    pub fn runtime(&self) -> &PiRuntime {
        &self.runtime
    }

    pub fn runtime_inventory(&self) -> &SessionRuntimeInventory {
        &self.runtime_inventory
    }

    /// Returns the context window used by compaction for the active model.
    pub fn active_context_window(&self) -> Option<u64> {
        let state = self.runtime.agent().state();
        self.context_window.or_else(|| {
            self.runtime
                .model(&state.provider_id, &state.model_id)
                .map(|model| model.context_window)
        })
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

    /// Publishes a transient, presentation-neutral notice from an extension.
    /// The notice is delivered to active frontend subscribers and is not
    /// written into the session log.
    pub fn notify_extension(&self, message: String, level: crate::ExtensionNoticeLevel) {
        self.events.publish_extension_notice(message, level);
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
        let mut text = text.into();
        if self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_run
            .is_some()
        {
            return self.queue_text(text, QueueKind::Steer).await;
        }
        let display_text = text.clone();
        if let Some(outcome) = self.runtime.execute_command(&text).await? {
            match outcome {
                CommandOutcome::Handled => return Ok(SubmitOutcome::Handled),
                CommandOutcome::TransformInput(transformed) => text = transformed,
            }
        }
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        self.maybe_threshold_compact_locked().await;
        let prepared = match self
            .runtime
            .prepare_text_submission_after_command(display_text, text)
            .await?
        {
            PreparedTextSubmission::Handled => return Ok(SubmitOutcome::Handled),
            PreparedTextSubmission::Agent(prepared) => prepared,
        };
        let run_id = self.begin_run(
            prepared.generation(),
            prepared.display_text(),
            prepared.text(),
            prepared.images(),
        )?;
        let telemetry = self.runtime.agent().telemetry_context();
        let run_span = telemetry.start_span::<RunSpan>(RunStart {
            operation: OperationStartAttributes {
                session_id: self
                    .runtime
                    .agent()
                    .session_id()
                    .unwrap_or_else(|| "unsaved".to_string()),
                lane_name: MAIN_LANE.to_string(),
                operation_id: run_id.clone(),
                recovery: false,
            },
            kind: RunOperationKind::Run,
        });
        let result = match prepared.run().await {
            Ok(recorded) => self.finish_prompt_locked(recorded).await,
            Err(error) => Err(SessionError::from(error)),
        };
        let finish_result = self.finish_run(&run_id, &result);
        self.emit_agent_settled().await;
        let (telemetry_outcome, telemetry_error) = match (&result, &finish_result) {
            (Ok(outcome), Ok(())) if outcome.stop == AgentLoopStop::Aborted => {
                (TelemetryRunOutcome::Aborted, None)
            }
            (Ok(_), Ok(())) => (TelemetryRunOutcome::Completed, None),
            (Err(_), _) | (Ok(_), Err(_)) => (
                TelemetryRunOutcome::Failed,
                Some(("session_run_failed".to_string(), "session".to_string())),
            ),
        };
        run_span.set_end_attributes(RunEnd {
            outcome: Some(telemetry_outcome),
            error_code: telemetry_error.as_ref().map(|(code, _)| code.clone()),
            error_type: telemetry_error.map(|(_, message)| message),
        });
        if telemetry_outcome == TelemetryRunOutcome::Failed {
            run_span.set_status(SpanStatus::Error);
        }
        run_span.finish();
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

    /// Enqueues an extension-created user or custom message without running
    /// text preprocessing. The durable queue record is committed before the
    /// live agent can observe steer/follow-up delivery.
    pub fn enqueue_extension_message(
        &self,
        message: Message,
        kind: QueueKind,
    ) -> Result<SubmitOutcome, SessionError> {
        self.ensure_open()?;
        let display_text = message_display_text(&message);
        let target = ProvisionedEntry {
            id: next_unique_id("entry"),
            entry: match &message {
                Message::Custom(custom) => SessionEntry::custom_message(custom),
                _ => SessionEntry::message(message.clone()),
            },
        };
        let entry_id = target.id.clone();
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if kind == QueueKind::NextRun {
            self.log.append_record(NewLaneRecord {
                id: next_unique_id("queue"),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::QueueEnqueued {
                    queue: kind,
                    run_id: None,
                    target: target.clone(),
                },
            })?;
            activity.recovered_queue.push(PendingSessionMessage {
                kind: Some(kind),
                run_id: None,
                display_text,
                message: message.clone(),
                target,
            });
        } else {
            let run = activity.active_run.as_mut().ok_or(SessionError::Busy)?;
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
        }
        let snapshot = activity.queue_snapshot();
        drop(activity);
        self.events.publish_queue(snapshot);
        match kind {
            QueueKind::Steer => self.runtime.agent().steer(message),
            QueueKind::FollowUp => self.runtime.agent().follow_up(message),
            QueueKind::NextRun => {}
        }
        Ok(SubmitOutcome::Queued { kind, entry_id })
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
        self.abort_retry();
        self.runtime.abort();
    }

    pub fn abort_retry(&self) {
        if let Some(retry) = self
            .retry_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            retry.abort();
        }
    }

    pub fn is_retrying(&self) -> bool {
        self.retry_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub fn auto_compaction_enabled(&self) -> bool {
        self.compaction_settings().enabled
    }

    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.compaction_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = enabled;
    }

    pub fn auto_retry_enabled(&self) -> bool {
        self.retry_settings().enabled
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        self.retry_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = enabled;
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.runtime.agent().steering_mode()
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.runtime.agent().set_steering_mode(mode);
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.runtime.agent().follow_up_mode()
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.runtime.agent().set_follow_up_mode(mode);
    }

    fn compaction_settings(&self) -> CompactionSettings {
        *self
            .compaction_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn retry_settings(&self) -> AutoRetrySettings {
        *self
            .retry_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let id = options.id.unwrap_or_else(|| next_unique_id("bash"));
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
        let resolved_command = self
            .shell_command_prefix
            .as_deref()
            .filter(|prefix| !prefix.is_empty())
            .map_or_else(|| command.clone(), |prefix| format!("{prefix}\n{command}"));
        let events = Arc::clone(&self.events);
        let update_id = id.clone();
        let execution = pi_shell::execute(ShellRequest {
            command: resolved_command,
            cwd: self.runtime.cwd().to_path_buf(),
            timeout: options.timeout,
            shell_path: options.shell_path.or_else(|| self.shell_path.clone()),
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
            "fullOutputPath": result.full_output_path,
            "timestamp": now_ms(),
            "excludeFromContext": options.exclude_from_context,
        }))?;
        let entry = SessionEntry::message(message);
        let record = match (|| -> Result<SessionRecord, SessionError> {
            let record = self.log.append_session_record(entry)?;
            let context = self
                .log
                .load()?
                .context_with_options(&self.context_options)?;
            restore_runtime_context(&self.runtime, &context)?;
            Ok(record)
        })() {
            Ok(record) => record,
            Err(error) => {
                self.events
                    .publish_bash_end(id, Some(result), Some(error.to_string()));
                return Err(error);
            }
        };
        self.events.publish_entry(record);
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
        images: &[ImageContent],
    ) -> Result<String, SessionError> {
        let message = Message::User(input_user_message(text, images, now_ms()));
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
        let behavior = match kind {
            QueueKind::Steer => InputStreamingBehavior::Steer,
            QueueKind::FollowUp => InputStreamingBehavior::FollowUp,
            QueueKind::NextRun => {
                return Err(SessionError::InvalidEntry(
                    "next-run queue cannot target an active run".to_string(),
                ));
            }
        };
        let (generation, display_text, text, images) =
            match self.runtime.process_queued_text(text, behavior).await? {
                QueuedTextOutcome::Handled => return Ok(SubmitOutcome::Handled),
                QueuedTextOutcome::Message {
                    generation,
                    display_text,
                    text,
                    images,
                } => (generation, display_text, text, images),
            };
        if generation != expected.1 {
            return Err(SessionError::Busy);
        }
        let message = Message::User(input_user_message(&text, &images, now_ms()));
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
            PromptInput::Text(text) => match self.runtime.prepare_text_submission(text).await? {
                PreparedTextSubmission::Handled => {
                    return Err(SessionError::Runtime(
                        "input was handled without starting an agent run; use submit()".to_string(),
                    ));
                }
                PreparedTextSubmission::Agent(prepared) => {
                    prepared.run().await.map_err(SessionError::from)
                }
            },
            messages => self
                .runtime
                .prompt_recorded(messages)
                .await
                .map_err(SessionError::from),
        };
        let result = match recorded {
            Ok(recorded) => self.finish_prompt_locked(recorded).await,
            Err(error) => Err(error),
        };
        self.emit_agent_settled().await;
        result
    }

    /// Runs a protocol-neutral, already-structured message batch.
    ///
    /// External adapters use this when their prompt vocabulary contains
    /// images or other blocks that cannot be represented by [`Self::submit`]'s
    /// text-only interface.
    pub async fn prompt_messages(
        &self,
        messages: Vec<Message>,
    ) -> Result<AgentLoopOutcome, SessionError> {
        self.prompt(PromptInput::Messages(messages)).await
    }

    async fn emit_agent_settled(&self) {
        self.runtime.dispatch_agent_settled().await;
        self.events
            .publish_agent_settled(self.runtime.agent().state());
    }

    async fn finish_prompt_locked(
        &self,
        recorded: RuntimePromptOutcome,
    ) -> Result<AgentLoopOutcome, SessionError> {
        self.retry_attempt.store(0, Ordering::Release);
        let _retry_attempt_reset = RetryAttemptReset(&self.retry_attempt);
        let mut recorded = recorded;
        let mut retry_attempt = 0_u32;
        let mut overflow_recovery_attempted = false;
        loop {
            let outcome = self.record_prompt(recorded)?;

            if self.is_overflow_outcome(&outcome) {
                if retry_attempt > 0 {
                    self.events.publish_auto_retry_end(
                        false,
                        retry_attempt,
                        assistant_error_message(&outcome).map(ToOwned::to_owned),
                    );
                    retry_attempt = 0;
                    self.retry_attempt.store(0, Ordering::Release);
                }
                if self.compaction_settings().enabled && !overflow_recovery_attempted {
                    overflow_recovery_attempted = true;
                    if self
                        .run_compaction_locked(crate::CompactionReason::Overflow, true, None, true)
                        .await
                        .is_ok()
                        && let Ok(retried) = self.runtime.continue_recorded().await
                    {
                        recorded = retried;
                        continue;
                    }
                }
                return Ok(outcome);
            }

            let retry_settings = self.retry_settings();
            if retry_settings.enabled
                && retry_attempt < retry_settings.max_retries
                && is_retryable_assistant_outcome(&outcome)
            {
                retry_attempt = retry_attempt.saturating_add(1);
                self.retry_attempt.store(retry_attempt, Ordering::Release);
                let delay_ms = retry_delay_ms(retry_settings.base_delay_ms, retry_attempt);
                self.events.publish_auto_retry_start(
                    retry_attempt,
                    retry_settings.max_retries,
                    delay_ms,
                    assistant_error_message(&outcome)
                        .unwrap_or("Unknown error")
                        .to_string(),
                );
                self.runtime
                    .agent()
                    .remove_last_failed_assistant()
                    .map_err(|error| SessionError::Runtime(error.to_string()))?;
                if !self.wait_for_retry_delay(delay_ms).await {
                    self.events.publish_auto_retry_end(
                        false,
                        retry_attempt,
                        Some("Retry cancelled".to_string()),
                    );
                    return Ok(outcome);
                }
                match self.runtime.continue_recorded().await {
                    Ok(retried) => {
                        recorded = retried;
                        continue;
                    }
                    Err(error) => {
                        self.events.publish_auto_retry_end(
                            false,
                            retry_attempt,
                            Some(error.to_string()),
                        );
                        return Err(error.into());
                    }
                }
            }

            if retry_attempt > 0 {
                self.events.publish_auto_retry_end(
                    assistant_outcome_succeeded(&outcome),
                    retry_attempt,
                    assistant_error_message(&outcome).map(ToOwned::to_owned),
                );
            }
            self.maybe_threshold_compact_locked().await;
            return Ok(outcome);
        }
    }

    async fn wait_for_retry_delay(&self, delay_ms: u64) -> bool {
        let (abort, signal) = AbortHandle::new();
        *self
            .retry_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(abort);
        let cancelled = tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => false,
            () = signal.wait() => true,
        };
        self.retry_abort
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        !cancelled
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
        let record = self.log.append_session_record(entry)?;
        self.events.publish_entry(record);
        Ok(outcome)
    }

    /// Persists extension state that is deliberately excluded from model
    /// context and publishes the same semantic entry event as other session
    /// mutations.
    pub fn append_custom_entry(
        &self,
        custom_type: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        let entry = SessionEntry::Custom(CustomEntry {
            custom_type: custom_type.into(),
            data,
        });
        let record = self.log.append_session_record(entry)?;
        let id = record.id.clone();
        self.events.publish_entry(record);
        Ok(id)
    }

    /// Persists a custom agent message without triggering a provider turn.
    /// Rebuilding runtime context keeps the next turn and resumed sessions in
    /// agreement with the JSONL tree.
    pub fn append_custom_message(&self, message: CustomMessage) -> Result<String, SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        let entry = SessionEntry::custom_message(&message);
        let record = self.log.append_session_record(entry)?;
        let id = record.id.clone();
        let context = self
            .log
            .load()?
            .context_with_options(&self.context_options)?;
        restore_runtime_context(&self.runtime, &context)?;
        self.events.publish_entry(record);
        Ok(id)
    }

    pub fn set_label(&self, entry_id: &str, label: Option<String>) -> Result<(), SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        self.log.set_label(entry_id, label)
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
        let record = self.log.append_session_record(entry)?;
        self.events.publish_entry(record);
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
        let record = self.log.append_session_record(entry)?;
        self.events.publish_entry(record);
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
        let record = self.log.append_session_record(entry)?;
        self.events.publish_entry(record);
        self.events
            .publish_thinking(thinking_level, self.runtime.agent().state());
        Ok(())
    }

    pub async fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        let _operation = self.operation_gate.lock().await;
        self.ensure_open()?;
        let normalized = self.set_name_locked(name)?;
        self.session_plugin_driver()
            .session_info_changed(&SessionInfoChangedEvent {
                name: normalized.clone(),
            })
            .await;
        self.events.publish_session_info(normalized);
        Ok(())
    }

    /// Applies the synchronous portion of a session-name change. Adapters
    /// whose public API is synchronous can publish the metadata immediately
    /// and dispatch the async session-plugin hook afterward.
    pub fn set_name_immediate(&self, name: Option<String>) -> Result<Option<String>, SessionError> {
        let _operation = self
            .operation_gate
            .try_lock()
            .map_err(|_| SessionError::Busy)?;
        self.ensure_open()?;
        let normalized = self.set_name_locked(name)?;
        self.events.publish_session_info(normalized.clone());
        Ok(normalized)
    }

    fn set_name_locked(&self, name: Option<String>) -> Result<Option<String>, SessionError> {
        let normalized = name.and_then(|name| {
            let mut sanitized = String::with_capacity(name.len());
            let mut newline_run = false;
            for character in name.chars() {
                if matches!(character, '\r' | '\n') {
                    if !newline_run {
                        sanitized.push(' ');
                    }
                    newline_run = true;
                } else {
                    sanitized.push(character);
                    newline_run = false;
                }
            }
            let name = sanitized.trim().to_string();
            (!name.is_empty()).then_some(name)
        });
        self.log.set_name(normalized.clone())?;
        Ok(normalized)
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
        let compaction_settings = self.compaction_settings();
        let preparation =
            prepare_compaction(&branch_entries, compaction_settings, &self.context_options)
                .unwrap_or_else(|| CompactionPreparation {
                    messages_to_summarize: Vec::new(),
                    turn_prefix_messages: Vec::new(),
                    retained_tail: compaction.retained_tail.clone(),
                    is_split_turn: false,
                    tokens_before: compaction.tokens_before,
                    previous_summary: None,
                    file_ops: FileOperations::default(),
                    settings: compaction_settings,
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

        let result: Result<SessionRecord, SessionError> = (|| {
            let record = self
                .log
                .append_session_record(SessionEntry::Compaction(compaction.clone()))?;
            let context = self
                .log
                .load()?
                .context_with_options(&self.context_options)?;
            restore_runtime_context(&self.runtime, &context)?;
            Ok(record)
        })();

        match result {
            Ok(record) => {
                let id = record.id.clone();
                self.events.publish_entry(record.clone());
                self.session_plugin_driver()
                    .session_compact(&SessionCompactEvent {
                        compaction_entry: compaction.clone(),
                        from_extension,
                        reason,
                        will_retry: false,
                    })
                    .await;
                self.events
                    .publish_compaction_end(reason, Some(record), false, false, None);
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
        let Some(context_window) = self.active_context_window() else {
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
        if should_compact(tokens, context_window, self.compaction_settings()) {
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
        let Some(context_window) = self.active_context_window() else {
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
                self.compaction_settings(),
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
            self.compaction_settings(),
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

        let persisted: Result<SessionRecord, SessionError> = (|| {
            let record = self
                .log
                .append_session_record(SessionEntry::Compaction(compaction.clone()))?;
            let context = self
                .log
                .load()?
                .context_with_options(&self.context_options)?;
            restore_runtime_context(&self.runtime, &context)?;
            Ok(record)
        })();
        match persisted {
            Ok(record) => {
                let id = record.id.clone();
                self.events.publish_entry(record.clone());
                self.session_plugin_driver()
                    .session_compact(&SessionCompactEvent {
                        compaction_entry: compaction.clone(),
                        from_extension,
                        reason,
                        will_retry,
                    })
                    .await;
                self.events
                    .publish_compaction_end(reason, Some(record), false, will_retry, None);
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
        let record = self
            .log
            .append_session_record(SessionEntry::BranchSummary(summary.clone()))?;
        let id = record.id.clone();
        if let Some(label) = label {
            self.log.set_label(&id, Some(label))?;
        }
        let context = self
            .log
            .load()?
            .context_with_options(&self.context_options)?;
        restore_runtime_context(&self.runtime, &context)?;
        self.events.publish_entry(record);
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

    /// Generates a Pi-compatible abandoned-branch summary with the selected
    /// provider, checks out the target, and appends the summary as the new
    /// leaf. Extension `session_before_tree` hooks may still replace it.
    pub async fn summarize_branch_and_checkout(
        &self,
        leaf_id: &str,
        custom_instructions: Option<String>,
        replace_instructions: bool,
        label: Option<String>,
    ) -> Result<String, SessionError> {
        const PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";
        const PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.\n\nUse this EXACT format:\n\n## Goal\n[What was the user trying to accomplish in this branch?]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Work that was started but not finished]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [What should happen next to continue this work]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

        self.ensure_open()?;
        let document = self.log.load()?;
        let preparation = tree_preparation(&document, Some(leaf_id), true)?;
        let context =
            crate::build_session_context(&preparation.entries_to_summarize, &self.context_options);
        let summary_messages = branch_summary_input_messages(
            context.messages,
            self.active_context_window().unwrap_or(128_000),
            self.branch_summary_reserve_tokens,
        );
        let (summary, usage) = if summary_messages.is_empty() {
            ("No content to summarize".to_string(), None)
        } else {
            let conversation = serde_json::to_string_pretty(&summary_messages)
                .map_err(|error| SessionError::InvalidPayload(error.to_string()))?;
            let instructions = match (custom_instructions, replace_instructions) {
                (Some(custom), true) => custom,
                (Some(custom), false) => format!("{PROMPT}\n\nAdditional focus: {custom}"),
                (None, _) => PROMPT.to_string(),
            };
            let (_, signal) = AbortHandle::new();
            let response = self
                .runtime
                .complete(
                    RuntimeCompletionRequest {
                        system_prompt: crate::SUMMARIZATION_SYSTEM_PROMPT.to_string(),
                        messages: vec![Message::User(UserMessage::text(
                            format!(
                                "<conversation>\n{conversation}\n</conversation>\n\n{instructions}"
                            ),
                            now_ms(),
                        ))],
                        thinking_level: self.runtime.agent().state().thinking_level,
                        thinking_budgets: self.runtime.agent().thinking_budgets(),
                        max_output_tokens: Some(2_048),
                    },
                    signal,
                )
                .await?;
            if response.stop_reason == StopReason::Error {
                return Err(SessionError::Runtime(
                    response
                        .error_message
                        .unwrap_or_else(|| "branch summarization failed".to_string()),
                ));
            }
            if response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
            {
                return Err(SessionError::Runtime(
                    "branch summarization attempted to call a tool".to_string(),
                ));
            }
            let text = response
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (
                format!(
                    "{PREAMBLE}{}",
                    if text.is_empty() {
                        "No summary generated"
                    } else {
                        &text
                    }
                ),
                Some(response.usage),
            )
        };
        let id = self
            .branch_with_summary(
                Some(leaf_id),
                BranchSummaryEntry {
                    from_id: String::new(),
                    summary,
                    details: None,
                    usage,
                },
            )
            .await?;
        if let Some(label) = label {
            self.set_label(&id, Some(label))?;
        }
        Ok(id)
    }

    fn attach_agent_bridge(&self) {
        let log = self.log.clone();
        let events = Arc::clone(&self.events);
        let agent = self.runtime.agent().downgrade();
        let activity = Arc::clone(&self.activity);
        let retry_settings = Arc::clone(&self.retry_settings);
        let retry_attempt = Arc::clone(&self.retry_attempt);
        self.runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let log = log.clone();
                let events = Arc::clone(&events);
                let agent = agent.clone();
                let activity = Arc::clone(&activity);
                let retry_settings = Arc::clone(&retry_settings);
                let retry_attempt = Arc::clone(&retry_attempt);
                async move {
                    let display_text = product_display_text(&activity, &event);
                    let (persisted_entry, queue_snapshot) = if let AgentEvent::MessageEnd {
                        message,
                    } = &event
                    {
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
                            let equivalent = pending_message_equivalent(&pending, message);
                            let mut target = pending.target;
                            if !equivalent {
                                target.entry = match message {
                                    Message::Custom(message) => {
                                        SessionEntry::custom_message(message)
                                    }
                                    message => SessionEntry::message(message.clone()),
                                };
                                if let Some(run_id) = &pending.run_id {
                                    log.append_record(NewLaneRecord {
                                        id: next_unique_id("message-replacement"),
                                        lane: MAIN_LANE.to_string(),
                                        record: LaneRecordEntry::WriteDeferred {
                                            run_id: run_id.clone(),
                                            target: target.clone(),
                                        },
                                    })
                                    .map_err(|error| EventError(error.to_string()))?;
                                }
                            }
                            let record = log
                                .append_entry(target.clone(), MAIN_LANE)
                                .map_err(|error| EventError(error.to_string()))?;
                            let queue_snapshot =
                                pending.kind.is_some().then(|| activity.queue_snapshot());
                            (Some(record), queue_snapshot)
                        } else {
                            let entry = match message {
                                Message::Custom(message) => SessionEntry::custom_message(message),
                                message => SessionEntry::message(message.clone()),
                            };
                            let record = log
                                .append_session_record(entry)
                                .map_err(|error| EventError(error.to_string()))?;
                            (Some(record), None)
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
                    match project_product_user_event(event, display_text.as_deref()) {
                        AgentEvent::AgentEnd { messages } => {
                            let settings = *retry_settings
                                .read()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let will_retry = will_retry_after_agent_end(
                                &messages,
                                settings,
                                retry_attempt.load(Ordering::Acquire),
                            );
                            events.publish_agent_end(messages, will_retry, agent_state);
                        }
                        event => events.publish_agent(event, agent_state),
                    }
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

fn input_user_message(text: &str, images: &[ImageContent], timestamp_ms: i64) -> UserMessage {
    let mut content = vec![ContentBlock::Text(pi_core::TextContent::new(text))];
    content.extend(images.iter().cloned().map(ContentBlock::Image));
    UserMessage {
        content,
        timestamp_ms,
    }
}

fn last_assistant(outcome: &AgentLoopOutcome) -> Option<&pi_core::AssistantMessage> {
    outcome
        .final_context
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message) => Some(message.as_ref()),
            _ => None,
        })
}

fn assistant_error_message(outcome: &AgentLoopOutcome) -> Option<&str> {
    last_assistant(outcome)?.error_message.as_deref()
}

fn assistant_outcome_succeeded(outcome: &AgentLoopOutcome) -> bool {
    last_assistant(outcome).is_some_and(|message| message.stop_reason != StopReason::Error)
}

struct RetryAttemptReset<'a>(&'a AtomicU32);

impl Drop for RetryAttemptReset<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}

fn will_retry_after_agent_end(
    messages: &[Message],
    settings: AutoRetrySettings,
    attempt: u32,
) -> bool {
    if !settings.enabled || attempt >= settings.max_retries {
        return false;
    }
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(message) => Some(
            message.stop_reason == StopReason::Error
                && message
                    .error_message
                    .as_deref()
                    .is_some_and(pi_core::is_retryable_provider_error_message),
        ),
        _ => None,
    }) == Some(true)
}

fn retry_delay_ms(base_delay_ms: u64, attempt: u32) -> u64 {
    let multiplier = 1_u64
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u64::MAX);
    base_delay_ms.saturating_mul(multiplier)
}

fn is_retryable_assistant_outcome(outcome: &AgentLoopOutcome) -> bool {
    let Some(message) = last_assistant(outcome) else {
        return false;
    };
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error) = message.error_message.as_deref() else {
        return false;
    };
    pi_core::is_retryable_provider_error_message(error)
}

fn branch_summary_input_messages(
    messages: Vec<AgentMessage>,
    context_window: u64,
    reserve_tokens: u64,
) -> Vec<AgentMessage> {
    let messages = messages
        .into_iter()
        .filter(|message| !matches!(message.as_standard(), Some(Message::ToolResult(_))))
        .collect::<Vec<_>>();
    let Some(token_budget) = context_window.checked_sub(reserve_tokens) else {
        return messages;
    };
    if token_budget == 0 {
        return messages;
    }
    let mut total_tokens = 0_u64;
    let mut selected = Vec::new();
    for message in messages.into_iter().rev() {
        let tokens = estimate_context_tokens(std::slice::from_ref(&message)).tokens;
        if total_tokens.saturating_add(tokens) > token_budget {
            break;
        }
        total_tokens = total_tokens.saturating_add(tokens);
        selected.push(message);
    }
    selected.reverse();
    selected
}

fn message_display_text(message: &Message) -> String {
    let blocks = match message {
        Message::User(message) => message.content.as_slice(),
        Message::Custom(message) => match &message.content {
            pi_core::CustomMessageContent::Text(text) => return text.clone(),
            pi_core::CustomMessageContent::Blocks(blocks) => blocks.as_slice(),
        },
        Message::Assistant(_) | Message::ToolResult(_) => return String::new(),
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pending_message_matches(pending: &PendingSessionMessage, message: &Message) -> bool {
    match (&pending.message, message) {
        (Message::User(_), Message::User(_)) => true,
        _ => pending.message == *message,
    }
}

fn pending_message_equivalent(pending: &PendingSessionMessage, message: &Message) -> bool {
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
        .and_then(|pending| {
            pending_message_equivalent(pending, message).then(|| pending.display_text.clone())
        })
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

fn pending_session_message(
    kind: QueueKind,
    run_id: Option<String>,
    target: ProvisionedEntry,
    timestamp_ms: Option<i64>,
) -> Option<PendingSessionMessage> {
    let (message, display_text) = match &target.entry {
        SessionEntry::Message(message_entry) => {
            let message = message_entry.message.as_standard()?.clone();
            if !matches!(message, Message::User(_)) {
                return None;
            }
            let display_text = message_entry
                .message
                .display_text()
                .map(str::to_string)
                .unwrap_or_else(|| message_display_text(&message));
            (message, display_text)
        }
        SessionEntry::CustomMessage(custom) => {
            let message = custom.to_message(timestamp_ms.unwrap_or_else(now_ms));
            let display_text = message_display_text(&message);
            (message, display_text)
        }
        _ => return None,
    };
    Some(PendingSessionMessage {
        kind: Some(kind),
        run_id,
        display_text,
        message,
        target,
    })
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
            pending_session_message(
                *queue,
                run_id.clone(),
                target.clone(),
                Some(record.timestamp_ms),
            )
        })
        .collect()
}

fn recover_interrupted_state(
    log: &SessionLog,
    document: &SessionDocument,
    defaults: crate::EffectiveLaneConfiguration,
) -> Result<Vec<PendingSessionMessage>, SessionError> {
    let open_operations = log.find_open_operations(MAIN_LANE, None)?;
    let mut missing_initial_messages = Vec::new();
    if let Some(started) = open_operations.first() {
        let branch = document.branch()?;
        let reduction = reduce_lane_state(&crate::LaneReductionInput {
            slice: crate::RecordLogSlice {
                lane: MAIN_LANE.to_string(),
                open_operations: open_operations.clone(),
                records: document
                    .records
                    .iter()
                    .filter(|record| record.lane == MAIN_LANE)
                    .cloned()
                    .collect(),
                entries: document.entries.clone(),
            },
            leaf_id: document
                .lanes
                .iter()
                .find(|lane| lane.lane == MAIN_LANE)
                .and_then(|lane| lane.leaf_id.clone()),
            own_entries: branch
                .iter()
                .filter(|entry| entry.seq > started.seq)
                .map(|entry| (*entry).clone())
                .collect(),
            configuration_entries: branch
                .iter()
                .filter(|entry| entry.seq <= started.seq)
                .map(|entry| (*entry).clone())
                .collect(),
            defaults,
        })
        .map_err(|error| {
            SessionError::InvalidPayload(format!("invalid interrupted operation log: {error}"))
        })?;
        if let Some(operation) = reduction.lane_state.operation {
            // A deferred write records an entry already accepted by the live
            // operation. Applying it is idempotent because the reducer only
            // returns targets whose exact IDs are still absent.
            for target in operation.pending_writes {
                log.append_entry(target, MAIN_LANE)?;
            }
            missing_initial_messages = operation.missing_initial_messages;
        }
    }

    let reconciled = log.load()?;
    let mut recovered = recover_pending_queue(&reconciled);
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

    // If the process stopped before Agent emitted the initial message_end,
    // preserve the provisioned user input as a next-run item. Replaying it
    // automatically during open would make session construction perform
    // provider I/O and could duplicate external side effects.
    for target in missing_initial_messages {
        if recovered.iter().any(|item| item.target.id == target.id) {
            continue;
        }
        log.append_record(NewLaneRecord {
            id: next_unique_id("queue-recovery"),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::QueueEnqueued {
                queue: QueueKind::NextRun,
                run_id: None,
                target: target.clone(),
            },
        })?;
        if let Some(item) = pending_session_message(QueueKind::NextRun, None, target, None) {
            recovered.push(item);
        }
    }

    let finished = reconciled
        .records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::OperationFinished { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    for operation in reconciled.records.iter().filter(|record| {
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
        messages: context.runtime_messages(),
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
        AgentPlugin, AgentSettledEvent, BeforeAgentStartEvent, BeforeAgentStartPatch, Command,
        CommandContext, CommandError, CommandOutcome, CommandSpec, ContentBlock, CustomMessage,
        CustomMessageContent, Message, MessageEndEvent, MessageEndPatch, PluginContext,
        PluginError, PluginId, RegisterContext, ResponseMetadata, StreamEvent, TextContent, Usage,
        UserMessage,
    };
    use pi_runtime::SystemPrompt;
    use pi_test_support::TestToolsPlugin;
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

    use super::*;
    use crate::{
        AgentSessionEvent, EntryOrder, EntryQuery, SUMMARIZATION_SYSTEM_PROMPT,
        SessionBeforeCompactResult, SessionBeforeTreeResult, SessionEntryType, SessionPlugin,
        SessionPluginContext, SessionPluginError, SessionPlugins,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn text_turn_with_usage(text: &str, total_tokens: u64) -> ScriptedTurn {
        ScriptedTurn::Events(vec![
            StreamEvent::Start {
                metadata: ResponseMetadata::new(
                    "scripted".into(),
                    "test".into(),
                    "scripted",
                    now_ms().saturating_add(60_000),
                ),
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

    struct PersistMessageEndReplacement;

    struct FailingAgentSettledPlugin;

    struct BlockingAgentSettledPlugin {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    struct CountingAgentLifecyclePlugin {
        ends: Arc<AtomicUsize>,
        settled: Arc<AtomicUsize>,
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for FailingAgentSettledPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("failing-agent-settled")
        }

        async fn agent_settled(
            &self,
            _context: PluginContext,
            _event: AgentSettledEvent,
        ) -> Result<(), PluginError> {
            Err(PluginError::Hook {
                plugin_id: self.id(),
                hook: "agent_settled",
                message: "intentional settled failure".to_string(),
            })
        }
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for BlockingAgentSettledPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("blocking-agent-settled")
        }

        async fn agent_settled(
            &self,
            _context: PluginContext,
            _event: AgentSettledEvent,
        ) -> Result<(), PluginError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for CountingAgentLifecyclePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("counting-agent-lifecycle")
        }

        async fn agent_end(
            &self,
            _context: PluginContext,
            _event: pi_core::AgentEndEvent,
        ) -> Result<(), PluginError> {
            self.ends.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn agent_settled(
            &self,
            _context: PluginContext,
            _event: AgentSettledEvent,
        ) -> Result<(), PluginError> {
            self.settled.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for PersistMessageEndReplacement {
        fn id(&self) -> PluginId {
            PluginId::new("persist-message-end-replacement")
        }

        async fn message_end(
            &self,
            _context: PluginContext,
            event: MessageEndEvent,
        ) -> Result<MessageEndPatch, PluginError> {
            let message = match event.message {
                Message::User(mut user) => {
                    user.content = vec![ContentBlock::Text(TextContent::new("persisted user"))];
                    Message::User(user)
                }
                Message::Assistant(assistant) => {
                    let mut assistant = (*assistant).clone();
                    assistant.content =
                        vec![ContentBlock::Text(TextContent::new("persisted assistant"))];
                    Message::assistant(assistant)
                }
                message => message,
            };
            Ok(MessageEndPatch {
                message: Some(message),
            })
        }
    }

    fn scripted_runtime(turns: impl IntoIterator<Item = ScriptedTurn>) -> PiRuntime {
        PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted(turns))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn extension_messages_and_entries_update_live_and_resumed_session_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let provider_plugin =
            ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();
        let custom = CustomMessage {
            custom_type: "fixture-context".to_string(),
            content: CustomMessageContent::Text("remember this".to_string()),
            display: true,
            details: Some(serde_json::json!({"source": "extension"})),
            timestamp_ms: now_ms(),
        };

        let custom_id = session.append_custom_message(custom.clone()).unwrap();
        assert_eq!(
            session
                .set_name_immediate(Some("  Core\r\n\n session  ".to_string()))
                .unwrap()
                .as_deref(),
            Some("Core  session")
        );
        assert!(session.runtime().agent().state().messages.iter().any(
            |message| matches!(message, Message::Custom(value)
                    if value.custom_type == custom.custom_type
                        && value.content == custom.content
                        && value.display == custom.display
                        && value.details == custom.details)
        ));
        let state_id = session
            .append_custom_entry("fixture-state", Some(serde_json::json!({"count": 1})))
            .unwrap();
        session
            .set_label(&custom_id, Some("checkpoint".to_string()))
            .unwrap();
        session.prompt("continue").await.unwrap();

        assert!(provider.requests()[0].messages.iter().any(|message| {
            matches!(message, Message::User(user)
                if user.content.iter().any(|block| matches!(block,
                    ContentBlock::Text(text) if text.text == "remember this")))
        }));
        let document = session.log().load().unwrap();
        assert_eq!(document.name.as_deref(), Some("Core  session"));
        assert_eq!(
            document.labels.get(&custom_id).map(String::as_str),
            Some("checkpoint")
        );
        assert!(document.entries.iter().any(|record| {
            record.id == state_id
                && matches!(&record.entry, SessionEntry::Custom(entry)
                    if entry.custom_type == "fixture-state")
        }));

        let queued = Message::custom(CustomMessage {
            custom_type: "fixture-next".to_string(),
            content: CustomMessageContent::Text("next run".to_string()),
            display: false,
            details: None,
            timestamp_ms: now_ms(),
        });
        session
            .enqueue_extension_message(queued, QueueKind::NextRun)
            .unwrap();
        let queued_at = session
            .log()
            .load()
            .unwrap()
            .records
            .into_iter()
            .find_map(|record| {
                matches!(
                    &record.record,
                    LaneRecordEntry::QueueEnqueued {
                        queue: QueueKind::NextRun,
                        ..
                    }
                )
                .then_some(record.timestamp_ms)
            })
            .unwrap();
        drop(session);

        let reopened = AgentSession::open(scripted_runtime([]), &path)
            .await
            .unwrap();
        assert_eq!(reopened.snapshot().queue.follow_up, vec!["next run"]);
        let activity = reopened
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(matches!(
            activity.recovered_queue.first().map(|item| &item.message),
            Some(Message::Custom(custom)) if custom.timestamp_ms == queued_at
        ));
    }

    #[tokio::test]
    async fn new_session_options_preserve_the_pi_parent_session_path() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent.jsonl");
        let session = AgentSession::create_with_options(
            scripted_runtime([]),
            directory.path().join("child.jsonl"),
            AgentSessionOptions::default().parent_session_path(Some(parent.clone())),
        )
        .await
        .unwrap();

        assert_eq!(
            session.log().header().legacy_parent_session_path,
            Some(parent)
        );
    }

    #[tokio::test]
    async fn summarized_tree_navigation_uses_a_standalone_provider_completion() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = scripted_runtime([
            ScriptedTurn::Text("root answer".to_string()),
            ScriptedTurn::Text("branch answer".to_string()),
            ScriptedTurn::Text("## Goal\nPreserve the branch".to_string()),
        ]);
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();
        session.prompt("root").await.unwrap();
        let target = session.log().leaf_id().unwrap();
        session.prompt("branch work").await.unwrap();

        let summary_id = session
            .summarize_branch_and_checkout(
                &target,
                Some("Focus on decisions".to_string()),
                false,
                Some("abandoned work".to_string()),
            )
            .await
            .unwrap();
        let document = session.log().load().unwrap();
        assert_eq!(
            document.labels.get(&summary_id).map(String::as_str),
            Some("abandoned work")
        );
        assert!(document.entries.iter().any(|record| {
            record.id == summary_id
                && matches!(&record.entry, SessionEntry::BranchSummary(summary)
                    if summary.summary.starts_with(
                        "The user explored a different conversation branch"
                    ) && summary.summary.contains("Preserve the branch"))
        }));
    }

    struct ExpandingCommandPlugin;

    struct BeforeStartInjectionPlugin;

    #[test]
    fn tree_preparation_collects_only_the_abandoned_branch_in_chronological_order() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("session.jsonl"),
            SessionHeader::new("session", directory.path()),
        )
        .unwrap();
        let root = log
            .append_message(Message::User(UserMessage::text("root", 1)))
            .unwrap();
        let common = log
            .append_message(Message::User(UserMessage::text("common", 2)))
            .unwrap();
        let abandoned = [
            log.append_message(Message::User(UserMessage::text("abandoned one", 3)))
                .unwrap(),
            log.append_message(Message::User(UserMessage::text("abandoned two", 4)))
                .unwrap(),
        ];
        log.create_lane("target", Some(&common)).unwrap();
        let target = log
            .append_to_lane(
                SessionEntry::message(Message::User(UserMessage::text("target", 5))),
                "target",
            )
            .unwrap();

        let preparation = tree_preparation(&log.load().unwrap(), Some(&target), true).unwrap();
        assert_eq!(
            preparation.common_ancestor_id.as_deref(),
            Some(common.as_str())
        );
        assert_eq!(
            preparation
                .entries_to_summarize
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            abandoned.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(
            !preparation
                .entries_to_summarize
                .iter()
                .any(|entry| entry.id == root)
        );
        assert!(preparation.user_wants_summary);

        let empty_log = SessionLog::create(
            directory.path().join("empty.jsonl"),
            SessionHeader::new("empty", directory.path()),
        )
        .unwrap();
        empty_log.create_lane("target", None).unwrap();
        let only_target = empty_log
            .append_to_lane(
                SessionEntry::message(Message::User(UserMessage::text("target", 1))),
                "target",
            )
            .unwrap();
        let empty = tree_preparation(&empty_log.load().unwrap(), Some(&only_target), true).unwrap();
        assert!(empty.entries_to_summarize.is_empty());
        assert!(empty.common_ancestor_id.is_none());
    }

    #[test]
    fn branch_summary_reserve_keeps_newest_messages_within_model_budget() {
        let selected = branch_summary_input_messages(
            vec![
                AgentMessage::from(Message::User(UserMessage::text("old ".repeat(200), 1))),
                AgentMessage::from(Message::User(UserMessage::text("new", 2))),
            ],
            100,
            90,
        );

        assert_eq!(selected.len(), 1);
        assert!(matches!(
            selected[0].as_standard(),
            Some(Message::User(user))
                if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "new")
        ));
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for BeforeStartInjectionPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("before-start-injection")
        }

        async fn before_agent_start(
            &self,
            _context: PluginContext,
            event: BeforeAgentStartEvent,
        ) -> Result<BeforeAgentStartPatch, PluginError> {
            Ok(BeforeAgentStartPatch {
                system_prompt: Some(format!("{}|injected", event.system_prompt)),
                messages: vec![Message::custom(CustomMessage {
                    custom_type: "fixture-context".to_string(),
                    content: CustomMessageContent::Text("injected context".to_string()),
                    display: false,
                    details: Some(serde_json::json!({"source": "fixture"})),
                    timestamp_ms: 1,
                })],
            })
        }
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for ExpandingCommandPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("expanding-command")
        }

        fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
            context.register_command(Arc::new(ExpandingCommand))
        }
    }

    #[tokio::test]
    async fn before_agent_start_custom_messages_follow_the_user_and_persist_as_custom_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("before-start.jsonl");
        let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .agent_plugin(BeforeStartInjectionPlugin)
            .provider_plugin(scripted)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();

        session.submit("hello").await.unwrap();

        let requests = provider.requests();
        assert!(requests[0].system_prompt.ends_with("|injected"));
        assert!(matches!(
            &requests[0].messages[..2],
            [Message::User(user), Message::User(injected)]
                if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "hello")
                    && matches!(&injected.content[0], ContentBlock::Text(text) if text.text == "injected context")
        ));

        let (_, document) = SessionLog::open(&path).unwrap();
        let branch = document.branch().unwrap();
        let conversation = branch
            .into_iter()
            .filter(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Message(_) | SessionEntry::CustomMessage(_)
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            &conversation[0].entry,
            SessionEntry::Message(message) if message.message.role() == "user"
        ));
        assert!(matches!(
            &conversation[1].entry,
            SessionEntry::CustomMessage(message)
                if message.custom_type == "fixture-context"
                    && message.details == Some(serde_json::json!({"source": "fixture"}))
        ));
        assert!(matches!(
            &session.runtime().agent().state().messages[1],
            Message::Custom(message) if message.custom_type == "fixture-context"
        ));
    }

    #[tokio::test]
    async fn message_end_replacements_are_the_messages_persisted_and_restored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .agent_plugin(PersistMessageEndReplacement)
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "provider assistant".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();

        session.submit("submitted user").await.unwrap();
        drop(session);

        let (_, document) = SessionLog::open(&path).unwrap();
        let messages = document.context().unwrap().runtime_messages();
        assert!(matches!(&messages[0], Message::User(user)
            if matches!(&user.content[0], ContentBlock::Text(text)
                if text.text == "persisted user")));
        assert!(matches!(&messages[1], Message::Assistant(assistant)
            if matches!(&assistant.content[0], ContentBlock::Text(text)
                if text.text == "persisted assistant")));

        let reopened = AgentSession::open(scripted_runtime([]), &path)
            .await
            .unwrap();
        assert_eq!(reopened.runtime().agent().state().messages, messages);
    }

    #[tokio::test]
    async fn agent_settled_hooks_finish_before_the_product_event_and_isolate_failures() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-settled.jsonl");
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let runtime = PiRuntime::builder()
            .agent_plugin(FailingAgentSettledPlugin)
            .agent_plugin(BlockingAgentSettledPlugin {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            })
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, &path).await.unwrap();
        let mut subscription = session.subscribe();

        let submit = {
            let session = session.clone();
            tokio::spawn(async move { session.submit("hello").await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        while let Ok(event) = subscription.events.try_recv() {
            assert!(!matches!(event.event, AgentSessionEvent::AgentSettled));
        }

        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), submit)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut saw_settled = false;
        while let Ok(event) = subscription.events.try_recv() {
            saw_settled |= matches!(event.event, AgentSessionEvent::AgentSettled);
        }
        assert!(saw_settled);
        assert!(
            session
                .runtime()
                .plugin_diagnostics()
                .iter()
                .any(|diagnostic| {
                    diagnostic.plugin_id == PluginId::new("failing-agent-settled")
                        && diagnostic.hook == "agent_settled"
                        && diagnostic.message.contains("intentional settled failure")
                })
        );
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
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::WaitForAbort,
            ]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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

        let reopened = AgentSession::open(scripted_runtime([]), &path)
            .await
            .unwrap();
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
            scripted_runtime([ScriptedTurn::Text("first answer".to_string())]),
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

    #[tokio::test]
    async fn shell_defaults_affect_execution_but_preserve_the_submitted_command() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shell-settings.jsonl");
        let original = "printf '%s' \"$configured_value\"";
        let session = AgentSession::create_with_options(
            scripted_runtime([]),
            &path,
            AgentSessionOptions::default().shell(
                if cfg!(unix) {
                    Some(PathBuf::from("/bin/sh"))
                } else {
                    None
                },
                Some("configured_value=from-settings".to_string()),
            ),
        )
        .await
        .unwrap();

        let result = session
            .execute_shell(original, ShellExecutionOptions::default())
            .await
            .unwrap();

        assert_eq!(result.output, "from-settings");
        let message = session
            .log()
            .load()
            .unwrap()
            .messages()
            .into_iter()
            .find(|message| message.role() == "bashExecution")
            .expect("bash execution message");
        assert_eq!(message.as_custom().unwrap()["command"], original);
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

    #[pi_session::session_plugin]
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
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
    async fn submit_emits_typed_product_run_and_provider_request_spans() {
        let directory = tempfile::tempdir().unwrap();
        let sink = Arc::new(pi_telemetry::InMemoryTelemetrySink::default());
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "done".to_string(),
            )]))
            .agent_options(AgentOptions {
                telemetry: pi_telemetry::TelemetryContext::new(sink.clone()),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("telemetry.jsonl"))
            .await
            .unwrap();

        session.submit("trace me").await.unwrap();

        let records = sink.records();
        assert_eq!(records.len(), 4);
        assert!(matches!(
            &records[0],
            pi_telemetry::TelemetryRecord::Start { name, attributes, .. }
                if name == "pi.harness.run"
                    && attributes["pi.operation.kind"] == "run"
                    && attributes["pi.operation.recovery"] == false
        ));
        assert!(matches!(
            &records[1],
            pi_telemetry::TelemetryRecord::Start { name, .. } if name == "pi.ai.request"
        ));
        assert!(matches!(
            &records[2],
            pi_telemetry::TelemetryRecord::End { name, .. } if name == "pi.ai.request"
        ));
        assert!(matches!(
            &records[3],
            pi_telemetry::TelemetryRecord::End { name, attributes, status, .. }
                if name == "pi.harness.run"
                    && attributes["pi.operation.outcome"] == "completed"
                    && *status == pi_telemetry::SpanStatus::Ok
        ));
    }

    #[tokio::test]
    async fn durable_queue_revisions_survive_abort_and_process_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queued-session.jsonl");
        let session = Arc::new(
            AgentSession::create(scripted_runtime([ScriptedTurn::WaitForAbort]), &path)
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

        let reopened = AgentSession::open(scripted_runtime([]), &path)
            .await
            .unwrap();
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
    async fn open_reconciles_interrupted_run_from_reducer_without_replaying_side_effects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("interrupted-session.jsonl");
        let log = SessionLog::create(
            &path,
            SessionHeader::new("interrupted-session", directory.path()),
        )
        .unwrap();
        let user = AgentMessage::from(Message::User(UserMessage::text("recover me", 1)));
        let initial = ProvisionedEntry {
            id: "initial-user".to_string(),
            entry: SessionEntry::message(user.clone()),
        };
        log.append_record(NewLaneRecord {
            id: "interrupted-run".to_string(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: None,
                intent: OperationIntent::Run {
                    original_prompt: vec![user],
                    initial_messages: vec![initial],
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        })
        .unwrap();
        log.append_record(NewLaneRecord {
            id: "accepted-write".to_string(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::WriteDeferred {
                run_id: "interrupted-run".to_string(),
                target: ProvisionedEntry {
                    id: "accepted-entry".to_string(),
                    entry: SessionEntry::Custom(CustomEntry {
                        custom_type: "accepted".to_string(),
                        data: Some(serde_json::json!({ "value": true })),
                    }),
                },
            },
        })
        .unwrap();
        drop(log);

        let reopened = AgentSession::open(scripted_runtime([]), &path)
            .await
            .unwrap();
        assert_eq!(
            reopened.snapshot().queue.follow_up,
            vec!["recover me".to_string()]
        );
        let document = reopened.log().load().unwrap();
        assert!(
            document
                .entries
                .iter()
                .any(|entry| entry.id == "accepted-entry")
        );
        assert!(
            reopened
                .log()
                .find_open_operations(MAIN_LANE, None)
                .unwrap()
                .is_empty()
        );
        let interrupted_finishes = document
            .records
            .iter()
            .filter(|record| {
                matches!(
                    &record.record,
                    LaneRecordEntry::OperationFinished {
                        run_id,
                        outcome: OperationOutcome::Aborted,
                        error: Some(OperationError { code, .. }),
                    } if run_id == "interrupted-run" && code == "interrupted"
                )
            })
            .count();
        assert_eq!(interrupted_finishes, 1);
        reopened.shutdown().await;
        drop(reopened);

        let reopened_again = AgentSession::open(scripted_runtime([]), &path)
            .await
            .unwrap();
        assert_eq!(
            reopened_again.snapshot().queue.follow_up,
            vec!["recover me".to_string()]
        );
        assert_eq!(
            reopened_again
                .log()
                .load()
                .unwrap()
                .records
                .iter()
                .filter(|record| matches!(
                    &record.record,
                    LaneRecordEntry::OperationFinished { run_id, .. }
                        if run_id == "interrupted-run"
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn open_and_checkout_restore_the_selected_v4_branch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::Text("first answer".to_string()),
                ScriptedTurn::Text("abandoned answer".to_string()),
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
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
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
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
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
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
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
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
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
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
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
    async fn new_session_preserves_a_custom_model_outside_the_catalog() {
        struct Catalog;

        #[pi_core::provider_plugin]
        impl pi_core::ProviderPlugin for Catalog {
            fn id(&self) -> pi_core::PluginId {
                pi_core::PluginId::new("catalog")
            }

            fn register(
                &self,
                context: &mut pi_core::ProviderRegisterContext<'_>,
            ) -> pi_core::Result<()> {
                context.register_model(pi_core::ModelSpec::new(
                    "scripted",
                    "registered",
                    "Registered",
                    "test",
                ))
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin(Catalog)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("unlisted"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let state = session.runtime().agent().state();

        assert_eq!(state.provider_id.as_str(), "scripted");
        assert_eq!(state.model_id.as_str(), "unlisted");
    }

    #[tokio::test]
    async fn active_context_window_tracks_catalog_model_switches() {
        struct Catalog;

        #[pi_core::provider_plugin]
        impl pi_core::ProviderPlugin for Catalog {
            fn id(&self) -> pi_core::PluginId {
                pi_core::PluginId::new("catalog")
            }

            fn register(
                &self,
                context: &mut pi_core::ProviderRegisterContext<'_>,
            ) -> pi_core::Result<()> {
                let mut small = pi_core::ModelSpec::new("scripted", "small", "Small", "test");
                small.context_window = 100;
                context.register_model(small)?;
                let mut large = pi_core::ModelSpec::new("scripted", "large", "Large", "test");
                large.context_window = 1_000;
                context.register_model(large)
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin(Catalog)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("small"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();

        assert_eq!(session.active_context_window(), Some(100));
        session
            .set_model(ProviderId::new("scripted"), ModelId::new("large"))
            .unwrap();
        assert_eq!(session.active_context_window(), Some(1_000));
    }

    #[tokio::test]
    async fn manual_compaction_uses_an_isolated_tool_free_completion() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("answer".to_string()),
            ScriptedTurn::Text("## Original Request\nKeep going".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
    async fn manual_compaction_uses_active_model_reasoning_and_output_limits() {
        struct LimitedCatalog;

        #[pi_core::provider_plugin]
        impl pi_core::ProviderPlugin for LimitedCatalog {
            fn id(&self) -> pi_core::PluginId {
                pi_core::PluginId::new("limited-catalog")
            }

            fn register(
                &self,
                context: &mut pi_core::ProviderRegisterContext<'_>,
            ) -> pi_core::Result<()> {
                let mut model = pi_core::ModelSpec::new("scripted", "limited", "Limited", "test");
                model.reasoning = false;
                model.max_tokens = 32;
                context.register_model(model)
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("answer".to_string()),
            ScriptedTurn::Text("## Original Request\nKeep going".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .provider_plugin(LimitedCatalog)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("limited"),
                thinking_level: ThinkingLevel::High,
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().compaction(CompactionSettings {
                reserve_tokens: 1_000,
                keep_recent_tokens: 1,
                enabled: true,
            }),
        )
        .await
        .unwrap();

        session.prompt("do the work").await.unwrap();
        session.compact(None).await.unwrap();

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].max_output_tokens, Some(32));
        assert_eq!(requests[1].thinking_level, ThinkingLevel::Off);
    }

    #[tokio::test]
    async fn threshold_compaction_runs_after_a_completed_turn() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            text_turn_with_usage("large answer", 200),
            ScriptedTurn::Text("threshold summary".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
        let agent_ends = Arc::new(AtomicUsize::new(0));
        let agent_settled = Arc::new(AtomicUsize::new(0));
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Error("context window token length exceeded".to_string()),
            ScriptedTurn::Text("overflow summary".to_string()),
            ScriptedTurn::Text("recovered answer".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .agent_plugin(CountingAgentLifecyclePlugin {
                ends: Arc::clone(&agent_ends),
                settled: Arc::clone(&agent_settled),
            })
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
        assert_eq!(agent_ends.load(Ordering::SeqCst), 2);
        assert_eq!(agent_settled.load(Ordering::SeqCst), 1);
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
    async fn transient_provider_failure_retries_without_reprojecting_failed_assistant() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Error("503 service unavailable".to_string()),
            ScriptedTurn::Text("recovered".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().retry(AutoRetrySettings {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 0,
            }),
        )
        .await
        .unwrap();
        let mut subscription = session.subscribe();

        let outcome = session.prompt("recover this").await.unwrap();

        assert_eq!(outcome.stop, AgentLoopStop::Completed);
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().all(|message| {
            !matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Error)
        }));
        let messages = session
            .log()
            .load()
            .unwrap()
            .entries
            .iter()
            .filter_map(|record| match &record.entry {
                SessionEntry::Message(message) => message.message.as_standard(),
                _ => None,
            })
            .cloned()
            .collect::<Vec<_>>();
        assert!(messages.iter().any(|message| {
            matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Error)
        }));
        assert!(messages.iter().any(|message| {
            matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Stop)
        }));

        let mut starts = Vec::new();
        let mut ends = Vec::new();
        let mut agent_ends = Vec::new();
        while let Ok(event) = subscription.events.try_recv() {
            match event.event {
                AgentSessionEvent::AgentEnd { will_retry, .. } => {
                    agent_ends.push(will_retry);
                }
                AgentSessionEvent::AutoRetryStart {
                    attempt,
                    max_attempts,
                    delay_ms,
                    ..
                } => starts.push((attempt, max_attempts, delay_ms)),
                AgentSessionEvent::AutoRetryEnd {
                    success, attempt, ..
                } => ends.push((success, attempt)),
                _ => {}
            }
        }
        assert_eq!(starts, vec![(1, 3, 0)]);
        assert_eq!(ends, vec![(true, 1)]);
        assert_eq!(agent_ends, vec![true, false]);
        assert!(session.snapshot().auto_retry.is_none());
    }

    #[tokio::test]
    async fn quota_failure_is_not_retried() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Error(
            "429 insufficient_quota: check billing".to_string(),
        )]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().retry(AutoRetrySettings {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 0,
            }),
        )
        .await
        .unwrap();
        let mut subscription = session.subscribe();

        let outcome = session.prompt("do not retry quota").await.unwrap();

        assert_eq!(outcome.stop, AgentLoopStop::ProviderError);
        assert_eq!(provider.requests().len(), 1);
        assert!(
            !std::iter::from_fn(|| subscription.events.try_recv().ok()).any(|event| {
                matches!(
                    event.event,
                    AgentSessionEvent::AutoRetryStart { .. }
                        | AgentSessionEvent::AutoRetryEnd { .. }
                )
            })
        );
    }

    #[tokio::test]
    async fn abort_cancels_retry_backoff_without_starting_another_provider_call() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Error("connection lost".to_string()),
            ScriptedTurn::Text("must not run".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create_with_options(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().retry(AutoRetrySettings {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 30_000,
            }),
        )
        .await
        .unwrap();
        let mut subscription = session.subscribe();
        let prompting = tokio::spawn({
            let session = session.clone();
            async move { session.prompt("cancel retry").await }
        });

        loop {
            let event = subscription.events.recv().await.unwrap();
            if matches!(event.event, AgentSessionEvent::AutoRetryStart { .. }) {
                break;
            }
        }
        assert!(session.is_retrying());
        session.abort();

        let outcome = prompting.await.unwrap().unwrap();
        assert_eq!(outcome.stop, AgentLoopStop::ProviderError);
        assert_eq!(provider.requests().len(), 1);
        assert!(!session.is_retrying());
        let retry_end = loop {
            let event = subscription.events.recv().await.unwrap();
            if let AgentSessionEvent::AutoRetryEnd {
                success,
                attempt,
                final_error,
            } = event.event
            {
                break (success, attempt, final_error);
            }
        };
        assert_eq!(retry_end, (false, 1, Some("Retry cancelled".to_string())));
    }

    #[tokio::test]
    async fn manual_compaction_can_be_aborted_without_waiting_for_the_operation_gate() {
        let directory = tempfile::tempdir().unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("answer".to_string()),
            ScriptedTurn::WaitForAbort,
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
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
