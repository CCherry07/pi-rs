//! One generation of a running session. Activity admission and context commits have one owner.
mod activity;
mod boundary;
mod bridge;
mod configuration;
mod configuration_hooks;
mod context;
mod delivery;
pub(crate) use delivery::SessionDelivery;
pub(crate) mod compaction;
mod control;
pub(crate) mod events;
mod freshness;
mod lifecycle;
mod navigation;
mod observers;
mod options;
mod recovery;
mod run;
mod selection_intent;
mod shell;
mod shell_executor;
mod views;
pub use observers::{SessionEventObserver, SessionEventObserverGuard};
pub use options::*;
pub use shell_executor::SessionShellExecutor;
pub use views::{SessionCatalog, SessionHistory};

use activity::SessionActivity;
use context::ContextUpdates;
pub(crate) use lifecycle::PreparedAgentSession;
pub(crate) use recovery::SessionRecoveryPlan;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, RwLock};

use pi_agent::AgentLoopOutcome;
use pi_core::{ImageContent, Message};
use pi_plugin::{PluginDriver, SessionDispatchContext};
use pi_runtime::{PiRuntime, PreparedRuntimeMessages, PreparedRuntimePrompt};
use pi_shell::DEFAULT_TIMEOUT;

use crate::runtime::events::AgentSessionEventHub;
use crate::{CompactionSettings, QueueKind, SessionCompactionPolicy, SessionContext, SessionStore};

const SESSION_OPEN: u8 = 0;
const SESSION_TRANSITIONING: u8 = 1;
const SESSION_CLOSED: u8 = 2;
const SESSION_SHUTTING_DOWN: u8 = 3;

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

/// The result of input preparation, available before an admitted run completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDisposition {
    Started,
    Handled,
    Queued,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PromptOutcome {
    /// The run and its settled callbacks have completed.
    Agent(AgentLoopOutcome),
    /// A command or input hook consumed the input without starting a run.
    Handled,
    /// The input was accepted into the active run's durable queue.
    Queued { kind: QueueKind, entry_id: String },
}

impl PromptOutcome {
    pub fn disposition(&self) -> PromptDisposition {
        match self {
            Self::Agent(_) => PromptDisposition::Started,
            Self::Handled => PromptDisposition::Handled,
            Self::Queued { .. } => PromptDisposition::Queued,
        }
    }
}

/// Text and optional image attachments submitted through the session input
/// pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInput {
    text: String,
    images: Option<Vec<ImageContent>>,
    source: pi_plugin::InputSource,
}

impl SessionInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn images(&self) -> &[ImageContent] {
        self.images.as_deref().unwrap_or_default()
    }

    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: None,
            source: pi_plugin::InputSource::Interactive,
        }
    }

    pub fn with_images(mut self, images: impl IntoIterator<Item = ImageContent>) -> Self {
        self.images = Some(images.into_iter().collect());
        self
    }

    /// Identifies the submitting frontend for extension input hooks.
    pub fn with_source(mut self, source: pi_plugin::InputSource) -> Self {
        self.source = source;
        self
    }
}

impl From<String> for SessionInput {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for SessionInput {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

pub struct AgentSession {
    runtime: PiRuntime,
    store: SessionStore,
    runtime_inventory: SessionRuntimeInventory,
    model_switch: ModelSwitchPolicy,
    scoped_models: Vec<pi_plugin::ScopedModel>,
    settings_writer: Option<Arc<dyn SessionSettingsWriter>>,
    generation_freshness: Option<Arc<dyn GenerationFreshness>>,
    generation_owner: std::sync::OnceLock<crate::WeakPiSession>,
    submission_abort_epoch: std::sync::OnceLock<Arc<std::sync::atomic::AtomicU64>>,
    context_updates: ContextUpdates,
    plugin_driver: Arc<PluginDriver>,
    diagnostic_subscriptions: Vec<pi_plugin::PluginDiagnosticSubscription>,
    executor: tokio::runtime::Handle,
    session_dispatch_context: SessionDispatchContext,
    compaction_settings: Arc<RwLock<CompactionSettings>>,
    compaction_settings_resolver: Option<Arc<dyn CompactionSettingsResolver>>,
    context_window: Option<u64>,
    branch_summary_reserve_tokens: u64,
    lifecycle_state: Arc<AtomicU8>,
    usage_recording_open: Arc<AtomicBool>,
    events: Arc<AgentSessionEventHub>,
    activity: Arc<SessionActivity>,
    retry_settings: Arc<RwLock<AutoRetrySettings>>,
    shell_executor: Option<Arc<dyn SessionShellExecutor>>,
    compaction_policy: Arc<dyn SessionCompactionPolicy>,
    initial_model_fallback_message: std::sync::Mutex<Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactionActivation {
    Immediate,
    AfterRun,
}

enum SessionSubmission {
    Text {
        display_text: String,
        input: SessionInput,
    },
    Messages(Vec<Message>),
    Queued,
}

enum PreparedSessionPrompt {
    Text(PreparedRuntimePrompt),
    Messages(PreparedRuntimeMessages),
}

/// A committed manual compaction and its estimate at the context cut point.
/// Later hook/context changes cannot replace this result with another checkpoint.
#[derive(Debug, Clone)]
pub struct SessionCompactionResult {
    record: crate::SessionRecord,
    estimated_tokens_after: u64,
}

impl SessionCompactionResult {
    pub fn record(&self) -> &crate::SessionRecord {
        &self.record
    }
    pub fn estimated_tokens_after(&self) -> u64 {
        self.estimated_tokens_after
    }
}

struct CompletedCompaction {
    context: SessionContext,
    record: crate::SessionRecord,
    estimated_tokens_after: u64,
}

impl AgentSession {
    pub(crate) fn runtime(&self) -> &PiRuntime {
        &self.runtime
    }

    /// Read-only generation, model, tool and resource queries.
    pub fn catalog(&self) -> crate::SessionCatalog<'_> {
        crate::SessionCatalog {
            runtime: &self.runtime,
        }
    }

    /// Read-only queries over this session's journal.
    pub fn history(&self) -> crate::SessionHistory<'_> {
        crate::SessionHistory { store: &self.store }
    }

    /// Generation-bound capabilities for host and plugin adapters.
    pub fn plugin_context(
        &self,
        scope: pi_plugin::PluginContextScope,
    ) -> pi_plugin::PluginContextHandle {
        self.runtime.plugin_context_handle(scope)
    }

    pub fn runtime_inventory(&self) -> &SessionRuntimeInventory {
        &self.runtime_inventory
    }

    /// Returns the context window used by compaction for the active model.
    pub fn active_context_window(&self) -> Option<u64> {
        let (provider_id, model_id) = self.runtime.agent().model_selection();
        self.context_window.or_else(|| {
            self.runtime
                .model(&provider_id, &model_id)
                .map(|model| model.context_window)
        })
    }

    /// Budget for the latest effective request. A fresh/restored or invalidated
    /// session has no snapshot until its next model request. This is not billed usage.
    pub fn context_budget(&self) -> Option<pi_core::ContextBudget> {
        let request = self.runtime.agent().context_request_usage()?;
        let (provider, model) = self.runtime.agent().model_selection();
        let settings = self
            .resolved_compaction_settings_for(&provider, &model)
            .ok()?;
        let reserve_tokens = if settings.enabled {
            settings.reserve_tokens
        } else {
            0
        };
        let context_window = self.active_context_window().filter(|window| *window > 0);
        Some(pi_core::ContextBudget {
            free_tokens: context_window.map(|window| {
                window
                    .saturating_sub(request.estimated_tokens)
                    .saturating_sub(reserve_tokens)
            }),
            request: (*request).clone(),
            context_window,
            reserve_tokens,
        })
    }

    pub(crate) fn store(&self) -> &SessionStore {
        &self.store
    }

    /// Returns the authoritative frontend state at its current revision.
    pub fn snapshot(&self) -> crate::AgentSessionSnapshot {
        self.events.snapshot()
    }

    /// Returns product-level idleness without cloning the frontend snapshot.
    pub fn is_idle(&self) -> bool {
        self.activity.is_idle()
    }

    /// Returns whether a model turn has installed the durable run record that
    /// live steer and follow-up messages must target.
    pub(crate) fn accepts_live_messages(&self) -> bool {
        self.activity.accepts_live_messages()
    }

    /// Waits for the entire product operation, including retry, compaction and
    /// settled callbacks. This is deliberately stronger than Agent idleness.
    pub async fn wait_for_idle(&self) {
        self.activity.wait_for_idle().await;
    }

    /// Returns whether either product message queue contains pending input.
    pub fn has_pending_messages(&self) -> bool {
        self.activity.has_pending_messages()
    }

    /// Atomically captures initial state and subscribes to subsequent product
    /// events. Consumers should ignore revisions at or below the snapshot's
    /// revision and refresh via [`Self::snapshot`] after receiver lag.
    pub fn subscribe(&self) -> crate::AgentSessionSubscription {
        let subscription = self.events.subscribe();
        self.publish_initial_fallback_notice();
        subscription
    }

    /// Observe publications synchronously without the broadcast capacity/lag contract.
    /// Retain the returned guard; callbacks run in revision order outside session locks.
    pub fn observe_events(
        &self,
        observer: Arc<dyn SessionEventObserver>,
    ) -> SessionEventObserverGuard {
        let guard = self.events.observe(observer);
        self.publish_initial_fallback_notice();
        guard
    }

    /// Acknowledge observer delivery through the publications preceding this call.
    /// Abort releases the producer but does not discard data already accepted by an observer.
    pub async fn flush_events(
        &self,
        signal: pi_core::AbortSignal,
    ) -> Result<(), crate::SessionError> {
        self.events
            .flush_observers(signal)
            .await
            .map_err(crate::SessionError::Runtime)
    }

    fn publish_initial_fallback_notice(&self) {
        let message = self
            .initial_model_fallback_message
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(message) = message {
            self.events
                .publish_extension_notice(message, crate::NoticeLevel::Warning);
        }
    }

    /// Publishes a transient, presentation-neutral notice from an extension.
    /// The notice is delivered to active frontend subscribers and is not
    /// written into the session log.
    pub fn notify_plugin(&self, message: String, level: crate::NoticeLevel) {
        self.events.publish_extension_notice(message, level);
    }

    pub(crate) fn plugin_driver(&self) -> &PluginDriver {
        &self.plugin_driver
    }

    pub(crate) fn session_dispatch_context(&self) -> &SessionDispatchContext {
        &self.session_dispatch_context
    }
}

#[cfg(test)]
mod tests;
