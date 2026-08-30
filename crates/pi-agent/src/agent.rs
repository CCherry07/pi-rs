use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use pi_core::{
    AbortHandle, AssistantMessage, BeforeAgentStartEvent, FrozenRegistries, Message, ModelId,
    PluginDriver, ProviderId, ProviderPluginDriver, RunId, ThinkingBudgets, ThinkingLevel,
    ToolCallId, ToolExecutionMode, UserMessage,
};
use pi_telemetry::TelemetryContext;
use tokio::sync::watch;

use crate::agent_loop::emit_run_failure_lifecycle;
use crate::event_dispatcher::{AgentEventDispatcher, AgentEventListener};
use crate::{
    AgentContext, AgentEventSink, AgentLoopConfig, AgentLoopOutcome, AgentLoopServices,
    AgentMessageQueues, AgentTurnControl, NoopAgentTurnControl, PendingMessageQueue, QueueMode,
    run_agent_loop, run_agent_loop_continue,
};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent is already processing")]
    AlreadyRunning,
    #[error("cannot reset while agent is running")]
    ResetWhileRunning,
    #[error("cannot configure while agent is running")]
    ConfigureWhileRunning,
    #[error("agent configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("agent loop failed: {0}")]
    Loop(String),
}

#[derive(Debug, Clone)]
pub enum PromptInput {
    Text(String),
    Messages(Vec<Message>),
}

impl From<&str> for PromptInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for PromptInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<Message>> for PromptInput {
    fn from(value: Vec<Message>) -> Self {
        Self::Messages(value)
    }
}

#[derive(Debug, Clone)]
pub struct AgentStateSnapshot {
    pub system_prompt: String,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub thinking_level: ThinkingLevel,
    pub active_tools: Vec<String>,
    pub messages: Vec<Message>,
    pub is_running: bool,
    pub streaming_message: Option<AssistantMessage>,
    pub pending_tool_calls: HashSet<ToolCallId>,
    pub error_message: Option<String>,
}

pub(crate) struct AgentState {
    pub(crate) snapshot: AgentStateSnapshot,
}

struct ActiveRun {
    abort_handle: AbortHandle,
    completion: watch::Receiver<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

#[derive(Clone)]
pub struct AgentOptions {
    pub system_prompt: String,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub thinking_level: ThinkingLevel,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub block_images: bool,
    pub active_tools: Vec<String>,
    pub messages: Vec<Message>,
    pub tool_execution: ToolExecutionMode,
    pub max_tool_iterations: usize,
    pub max_parallel_tools: usize,
    pub cwd: std::path::PathBuf,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub turn_control: Arc<dyn AgentTurnControl>,
    pub telemetry: TelemetryContext,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            thinking_budgets: None,
            block_images: false,
            active_tools: Vec::new(),
            messages: Vec::new(),
            tool_execution: ToolExecutionMode::Parallel,
            max_tool_iterations: 50,
            max_parallel_tools: 8,
            cwd: std::path::PathBuf::from("."),
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            turn_control: Arc::new(NoopAgentTurnControl),
            telemetry: TelemetryContext::noop(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentConfigurationPatch {
    pub system_prompt: Option<String>,
    pub active_tools: Option<Vec<String>>,
    pub provider_id: Option<ProviderId>,
    pub model_id: Option<ModelId>,
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, Clone)]
pub struct AgentRestoreState {
    pub system_prompt: Option<String>,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub thinking_level: ThinkingLevel,
    pub active_tools: Vec<String>,
    pub messages: Vec<Message>,
}

#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

/// Non-owning agent handle for listeners that are stored by the agent itself.
/// Using this in callbacks avoids an `Agent -> listener -> Agent` reference cycle.
#[derive(Clone)]
pub struct WeakAgent {
    inner: Weak<AgentInner>,
}

impl WeakAgent {
    pub fn state(&self) -> Option<AgentStateSnapshot> {
        self.inner.upgrade().map(|inner| Agent { inner }.state())
    }
}

/// Immutable plugin/runtime dependencies captured once by each agent run.
///
/// A new value can be installed between runs so registries and plugin hooks
/// always move together as one generation.
pub struct AgentRuntime {
    generation: u64,
    system_prompt: String,
    registries: Arc<FrozenRegistries>,
    plugins: Arc<PluginDriver>,
    provider_plugins: Arc<ProviderPluginDriver>,
}

impl AgentRuntime {
    pub fn new(
        generation: u64,
        system_prompt: impl Into<String>,
        registries: Arc<FrozenRegistries>,
        plugins: Arc<PluginDriver>,
        provider_plugins: Arc<ProviderPluginDriver>,
    ) -> Self {
        Self {
            generation,
            system_prompt: system_prompt.into(),
            registries,
            plugins,
            provider_plugins,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn registries(&self) -> &Arc<FrozenRegistries> {
        &self.registries
    }

    pub fn plugins(&self) -> &Arc<PluginDriver> {
        &self.plugins
    }

    pub fn provider_plugins(&self) -> &Arc<ProviderPluginDriver> {
        &self.provider_plugins
    }

    fn with_system_prompt(&self, system_prompt: String) -> Self {
        Self {
            generation: self.generation,
            system_prompt,
            registries: Arc::clone(&self.registries),
            plugins: Arc::clone(&self.plugins),
            provider_plugins: Arc::clone(&self.provider_plugins),
        }
    }
}

pub(crate) type RegisteredListeners = Vec<(SubscriptionId, Arc<dyn AgentEventListener>)>;

struct AgentInner {
    state: Arc<Mutex<AgentState>>,
    run_gate: tokio::sync::Mutex<()>,
    active_run: Mutex<Option<ActiveRun>>,
    steering: Arc<PendingMessageQueue>,
    follow_up: Arc<PendingMessageQueue>,
    listeners: Arc<RwLock<RegisteredListeners>>,
    next_subscription: AtomicU64,
    runtime: RwLock<Arc<AgentRuntime>>,
    session_id: RwLock<Option<String>>,
    config: AgentOptions,
}

struct QueueAdapter {
    steering: Arc<PendingMessageQueue>,
    follow_up: Arc<PendingMessageQueue>,
    skip_next_steering_poll: AtomicBool,
}

impl AgentMessageQueues for QueueAdapter {
    fn drain_steering(&self) -> Vec<Message> {
        if self.skip_next_steering_poll.swap(false, Ordering::AcqRel) {
            return Vec::new();
        }
        self.steering.drain()
    }

    fn drain_follow_up(&self) -> Vec<Message> {
        self.follow_up.drain()
    }
}

impl Agent {
    pub fn set_session_id(&self, session_id: Option<String>) {
        *self
            .inner
            .session_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = session_id;
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner
            .session_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn thinking_budgets(&self) -> Option<ThinkingBudgets> {
        self.inner.config.thinking_budgets
    }

    pub fn downgrade(&self) -> WeakAgent {
        WeakAgent {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn new(
        options: AgentOptions,
        registries: Arc<FrozenRegistries>,
        plugins: Arc<PluginDriver>,
    ) -> Self {
        let runtime = Arc::new(AgentRuntime::new(
            0,
            options.system_prompt.clone(),
            registries,
            plugins,
            Arc::new(
                ProviderPluginDriver::new(Vec::new())
                    .expect("an empty provider plugin driver is always valid"),
            ),
        ));
        Self::with_runtime(options, runtime)
    }

    pub fn with_runtime(options: AgentOptions, runtime: Arc<AgentRuntime>) -> Self {
        let snapshot = AgentStateSnapshot {
            system_prompt: runtime.system_prompt().to_string(),
            provider_id: options.provider_id.clone(),
            model_id: options.model_id.clone(),
            thinking_level: options.thinking_level,
            active_tools: options.active_tools.clone(),
            messages: options.messages.clone(),
            is_running: false,
            streaming_message: None,
            pending_tool_calls: HashSet::new(),
            error_message: None,
        };
        Self {
            inner: Arc::new(AgentInner {
                state: Arc::new(Mutex::new(AgentState { snapshot })),
                run_gate: tokio::sync::Mutex::new(()),
                active_run: Mutex::new(None),
                steering: Arc::new(PendingMessageQueue::new(options.steering_mode)),
                follow_up: Arc::new(PendingMessageQueue::new(options.follow_up_mode)),
                listeners: Arc::new(RwLock::new(Vec::new())),
                next_subscription: AtomicU64::new(1),
                runtime: RwLock::new(runtime),
                session_id: RwLock::new(None),
                config: options,
            }),
        }
    }

    pub async fn prompt(
        &self,
        input: impl Into<PromptInput>,
    ) -> Result<AgentLoopOutcome, AgentError> {
        let prompts = match input.into() {
            PromptInput::Text(text) => {
                vec![Message::User(UserMessage::text(text, now_ms()))]
            }
            PromptInput::Messages(messages) => messages,
        };
        self.run(RunKind::Prompt {
            messages: prompts,
            skip_initial_steering_poll: false,
        })
        .await
    }

    pub async fn continue_run(&self) -> Result<AgentLoopOutcome, AgentError> {
        self.run(RunKind::Continue).await
    }

    pub fn steer(&self, message: Message) {
        self.inner.steering.enqueue(message);
    }

    pub fn follow_up(&self, message: Message) {
        self.inner.follow_up.enqueue(message);
    }

    pub fn has_queued_messages(&self) -> bool {
        self.inner.steering.has_items() || self.inner.follow_up.has_items()
    }

    pub fn clear_all_queues(&self) {
        self.inner.steering.clear();
        self.inner.follow_up.clear();
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.inner.steering.mode()
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner.steering.set_mode(mode);
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.inner.follow_up.mode()
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner.follow_up.set_mode(mode);
    }

    pub fn abort(&self) {
        if let Some(active) = self
            .inner
            .active_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            active.abort_handle.abort();
        }
    }

    pub async fn wait_for_idle(&self) {
        let mut completion = self
            .inner
            .active_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|active| active.completion.clone());
        if let Some(receiver) = completion.as_mut() {
            while !*receiver.borrow() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        }
    }

    pub fn state(&self) -> AgentStateSnapshot {
        let mut snapshot = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot
            .clone();
        snapshot.system_prompt = self.runtime().system_prompt().to_string();
        snapshot
    }

    pub fn runtime(&self) -> Arc<AgentRuntime> {
        Arc::clone(
            &self
                .inner
                .runtime
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub fn telemetry_context(&self) -> TelemetryContext {
        self.inner.config.telemetry.clone()
    }

    /// Installs a complete runtime generation after the active run settles.
    /// The previous generation remains untouched if it is incompatible with
    /// the agent's current provider or active tool selection.
    pub async fn replace_runtime(&self, runtime: Arc<AgentRuntime>) -> Result<(), AgentError> {
        let _run_guard = self.inner.run_gate.lock().await;
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_runtime_selection(&state.snapshot, &runtime)?;
        state.snapshot.system_prompt = runtime.system_prompt().to_string();
        *self
            .inner
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = runtime;
        Ok(())
    }

    pub fn configure(&self, patch: AgentConfigurationPatch) -> Result<(), AgentError> {
        let runtime = self.runtime();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.snapshot.is_running {
            return Err(AgentError::ConfigureWhileRunning);
        }
        if let Some(tools) = &patch.active_tools {
            validate_active_tools(tools, &runtime)?;
        }
        if let Some(system_prompt) = patch.system_prompt {
            state.snapshot.system_prompt = system_prompt.clone();
            *self
                .inner
                .runtime
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Arc::new(runtime.with_system_prompt(system_prompt));
        }
        if let Some(active_tools) = patch.active_tools {
            state.snapshot.active_tools = active_tools;
        }
        if let Some(provider_id) = patch.provider_id {
            if runtime.registries().provider(&provider_id).is_none() {
                return Err(AgentError::InvalidConfiguration(format!(
                    "unknown provider: {provider_id}"
                )));
            }
            state.snapshot.provider_id = provider_id;
        }
        if let Some(model_id) = patch.model_id {
            state.snapshot.model_id = model_id;
        }
        if let Some(thinking_level) = patch.thinking_level {
            state.snapshot.thinking_level = thinking_level;
        }
        Ok(())
    }

    /// Replaces all persisted agent state while preserving transient runtime
    /// dependencies. Restore is only available while idle and validates the
    /// selected provider and tools against the active runtime generation.
    pub fn restore(&self, restored: AgentRestoreState) -> Result<(), AgentError> {
        let runtime = self.runtime();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.snapshot.is_running {
            return Err(AgentError::ConfigureWhileRunning);
        }
        validate_active_tools(&restored.active_tools, &runtime)?;
        let candidate = AgentStateSnapshot {
            system_prompt: restored
                .system_prompt
                .clone()
                .unwrap_or_else(|| runtime.system_prompt().to_string()),
            provider_id: restored.provider_id,
            model_id: restored.model_id,
            thinking_level: restored.thinking_level,
            active_tools: restored.active_tools,
            messages: restored.messages,
            is_running: false,
            streaming_message: None,
            pending_tool_calls: HashSet::new(),
            error_message: None,
        };
        validate_runtime_selection(&candidate, &runtime)?;
        if let Some(system_prompt) = restored.system_prompt {
            *self
                .inner
                .runtime
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Arc::new(runtime.with_system_prompt(system_prompt));
        }
        state.snapshot = candidate;
        self.inner.steering.clear();
        self.inner.follow_up.clear();
        Ok(())
    }

    pub fn subscribe(&self, listener: Arc<dyn AgentEventListener>) -> SubscriptionId {
        let id = SubscriptionId(self.inner.next_subscription.fetch_add(1, Ordering::Relaxed));
        self.inner
            .listeners
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        id
    }

    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut listeners = self
            .inner
            .listeners
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = listeners.len();
        listeners.retain(|(candidate, _)| *candidate != id);
        before != listeners.len()
    }

    pub fn reset(&self) -> Result<(), AgentError> {
        if self.state().is_running {
            return Err(AgentError::ResetWhileRunning);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.snapshot.messages.clear();
        state.snapshot.streaming_message = None;
        state.snapshot.pending_tool_calls.clear();
        state.snapshot.error_message = None;
        self.inner.steering.clear();
        self.inner.follow_up.clear();
        Ok(())
    }

    /// Removes only the terminal failed assistant from live context before a
    /// product-owned retry. Persistence remains the session layer's concern,
    /// and pending steering/follow-up queues are deliberately preserved.
    pub fn remove_last_failed_assistant(&self) -> Result<bool, AgentError> {
        if self.state().is_running {
            return Err(AgentError::ConfigureWhileRunning);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let failed = matches!(
            state.snapshot.messages.last(),
            Some(Message::Assistant(message))
                if matches!(message.stop_reason, pi_core::StopReason::Error | pi_core::StopReason::Length)
        );
        if failed {
            state.snapshot.messages.pop();
            state.snapshot.error_message = None;
        }
        Ok(failed)
    }

    async fn run(&self, kind: RunKind) -> Result<AgentLoopOutcome, AgentError> {
        let _run_guard = self
            .inner
            .run_gate
            .try_lock()
            .map_err(|_| AgentError::AlreadyRunning)?;
        let kind = match kind {
            RunKind::Continue => match self.state().messages.last() {
                None => return Err(AgentError::Loop("agent context is empty".to_string())),
                Some(Message::Assistant(_)) => {
                    let steering = self.inner.steering.drain();
                    if !steering.is_empty() {
                        RunKind::Prompt {
                            messages: steering,
                            // The selected steering item is already the prompt for this
                            // run. Leave the next one-at-a-time item queued until the
                            // first response completes.
                            skip_initial_steering_poll: true,
                        }
                    } else {
                        let follow_up = self.inner.follow_up.drain();
                        if follow_up.is_empty() {
                            return Err(AgentError::Loop(
                                "cannot continue from an assistant message".to_string(),
                            ));
                        }
                        RunKind::Prompt {
                            messages: follow_up,
                            skip_initial_steering_poll: false,
                        }
                    }
                }
                Some(_) => RunKind::Continue,
            },
            kind => kind,
        };
        let runtime = self.runtime();
        let run_id = RunId::next();
        let (abort_handle, abort_signal) = AbortHandle::new();
        let (completion_sender, completion) = watch::channel(false);
        *self
            .inner
            .active_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ActiveRun {
            abort_handle,
            completion,
        });
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.snapshot.is_running = true;
            state.snapshot.streaming_message = None;
            state.snapshot.error_message = None;
        }

        let snapshot = self.state();
        let input_messages = match &kind {
            RunKind::Prompt { messages, .. } => messages.clone(),
            RunKind::Continue => Vec::new(),
        };
        let hook = match runtime
            .plugins()
            .before_agent_start(
                &run_id,
                &self.inner.config.cwd,
                &abort_signal,
                BeforeAgentStartEvent {
                    system_prompt: runtime.system_prompt().to_string(),
                    input_messages,
                    active_tools: snapshot.active_tools.clone(),
                    provider_id: snapshot.provider_id.clone(),
                    model_id: snapshot.model_id.clone(),
                },
            )
            .await
        {
            Ok(hook) => hook,
            Err(error) => {
                self.finish_active_run(&completion_sender);
                return Err(AgentError::Loop(error.to_string()));
            }
        };
        let context = AgentContext {
            system_prompt: hook
                .system_prompt
                .unwrap_or_else(|| runtime.system_prompt().to_string()),
            messages: snapshot.messages,
            active_tools: snapshot.active_tools,
        };
        let config = AgentLoopConfig {
            provider_id: snapshot.provider_id,
            model_id: snapshot.model_id,
            thinking_level: snapshot.thinking_level,
            thinking_budgets: self.inner.config.thinking_budgets,
            block_images: self.inner.config.block_images,
            tool_execution: self.inner.config.tool_execution,
            max_tool_iterations: self.inner.config.max_tool_iterations,
            max_parallel_tools: self.inner.config.max_parallel_tools,
            cwd: self.inner.config.cwd.clone(),
            session_id: self.session_id(),
        };
        let queue_adapter: Arc<dyn AgentMessageQueues> = Arc::new(QueueAdapter {
            steering: Arc::clone(&self.inner.steering),
            follow_up: Arc::clone(&self.inner.follow_up),
            skip_next_steering_poll: AtomicBool::new(matches!(
                &kind,
                RunKind::Prompt {
                    skip_initial_steering_poll: true,
                    ..
                }
            )),
        });
        let events = self.event_sink(run_id.clone(), Arc::clone(runtime.plugins()));
        let services = AgentLoopServices {
            generation: runtime.generation(),
            registries: Arc::clone(runtime.registries()),
            plugins: Arc::clone(runtime.plugins()),
            provider_plugins: Arc::clone(runtime.provider_plugins()),
            queues: queue_adapter,
            turn_control: Arc::clone(&self.inner.config.turn_control),
            telemetry: self.inner.config.telemetry.clone(),
            events: Arc::clone(&events),
        };
        let failure_config = config.clone();
        let failure_signal = abort_signal.clone();
        let result = match kind {
            RunKind::Prompt { mut messages, .. } => {
                messages.extend(hook.messages);
                run_agent_loop(run_id, messages, context, config, services, abort_signal).await
            }
            RunKind::Continue if hook.messages.is_empty() => {
                run_agent_loop_continue(run_id, context, config, services, abort_signal).await
            }
            RunKind::Continue => {
                run_agent_loop(
                    run_id,
                    hook.messages,
                    context,
                    config,
                    services,
                    abort_signal,
                )
                .await
            }
        };

        if let Err(error) = &result {
            emit_run_failure_lifecycle(&events, &failure_signal, &failure_config, error).await;
        }

        self.finish_active_run(&completion_sender);
        result.map_err(|error| AgentError::Loop(error.to_string()))
    }

    fn finish_active_run(&self, completion_sender: &watch::Sender<bool>) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.snapshot.is_running = false;
            state.snapshot.streaming_message = None;
            state.snapshot.pending_tool_calls.clear();
        }
        self.inner
            .active_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let _ = completion_sender.send(true);
    }

    fn event_sink(&self, run_id: RunId, plugins: Arc<PluginDriver>) -> Arc<dyn AgentEventSink> {
        Arc::new(AgentEventDispatcher::new(
            Arc::clone(&self.inner.state),
            Arc::clone(&self.inner.listeners),
            plugins,
            run_id,
            self.inner.config.cwd.clone(),
        ))
    }
}

fn validate_runtime_selection(
    snapshot: &AgentStateSnapshot,
    runtime: &AgentRuntime,
) -> Result<(), AgentError> {
    let unknown_tools = snapshot
        .active_tools
        .iter()
        .filter(|tool| runtime.registries().tool(tool).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if !unknown_tools.is_empty() {
        return Err(AgentError::InvalidConfiguration(format!(
            "active tools are unavailable in generation {}: {}",
            runtime.generation(),
            unknown_tools.join(", ")
        )));
    }
    if runtime
        .registries()
        .provider(&snapshot.provider_id)
        .is_none()
    {
        return Err(AgentError::InvalidConfiguration(format!(
            "provider {} is unavailable in generation {}",
            snapshot.provider_id,
            runtime.generation()
        )));
    }
    Ok(())
}

fn validate_active_tools(tools: &[String], runtime: &AgentRuntime) -> Result<(), AgentError> {
    let mut seen = HashSet::new();
    for tool in tools {
        if !seen.insert(tool) {
            return Err(AgentError::InvalidConfiguration(format!(
                "duplicate active tool: {tool}"
            )));
        }
        if runtime.registries().tool(tool).is_none() {
            return Err(AgentError::InvalidConfiguration(format!(
                "unknown active tool: {tool}"
            )));
        }
    }
    Ok(())
}

enum RunKind {
    Prompt {
        messages: Vec<Message>,
        skip_initial_steering_poll: bool,
    },
    Continue,
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}
