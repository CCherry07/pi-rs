#![deny(unsafe_code)]

//! Hermes Agent memory and self-improvement, adapted to Pi generations.
//! Upstream baseline: NousResearch/hermes-agent e629c900a87622ddcc31f67a4b4a756b239fbaf0.

mod anchor_search;
mod commands;
mod config;
mod consolidation;
mod content_scanner;
mod database;
mod execution;
mod flush;
mod project;
mod review_plugin;
mod skill_review;
mod skills;
mod standing;
mod store;
mod tool;
mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AgentEndEvent, AgentPlugin, AgentPluginContext, AgentSettledEvent,
    AgentStartEvent, BeforeAgentStartEvent, BeforeAgentStartPatch, MessageEndEvent,
    MessageEndPatch, NoticeLevel, PluginError, PluginId, RegisterContext, RunId, TurnEndEvent,
};
use pi_memory_loader::{
    MemoryProviderConfig, MemoryProviderFactory, MemoryProviderInitializeContext,
    MemoryProviderInitializeError, MemoryProviderPlugin,
};
use pi_session::{
    SessionBeforeCompactEvent, SessionPlugin, SessionPluginContext, SessionPluginError,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent,
};

use crate::config::HermesMemoryConfig;
use crate::execution::{HermesRunLease, HermesRunState, HermesRuns};
use crate::store::HermesMemoryStore;

pub const HERMES_MEMORY_PROVIDER_ID: &str = "hermes";
pub const HERMES_MEMORY_PLUGIN_ID: &str = "memory-hermes";

/// Factory for the built-in Hermes curated-memory provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct HermesMemoryProviderFactory;

#[async_trait]
impl MemoryProviderFactory for HermesMemoryProviderFactory {
    fn id(&self) -> &str {
        HERMES_MEMORY_PROVIDER_ID
    }

    async fn initialize(
        &self,
        context: &MemoryProviderInitializeContext,
        provider_config: &MemoryProviderConfig,
    ) -> Result<Arc<dyn MemoryProviderPlugin>, MemoryProviderInitializeError> {
        let config = HermesMemoryConfig::load(context.agent_dir(), provider_config.raw());
        let store = HermesMemoryStore::load(
            context.agent_dir(),
            context.cwd(),
            config.clone(),
            context.session_roots().to_vec(),
            context.project_trusted(),
        )?;
        Ok(Arc::new(HermesMemoryPlugin {
            store: Arc::new(store),
            config,
            runs: Arc::new(HermesRuns::default()),
            foreground_runs: Mutex::new(HashMap::new()),
            activity: Arc::new(Mutex::new(HashMap::new())),
            live_index: Mutex::new(None),
            backfill: Mutex::new(None),
            config_warning_emitted: AtomicBool::new(false),
        }))
    }
}

pub struct HermesMemoryPlugin {
    store: Arc<HermesMemoryStore>,
    config: HermesMemoryConfig,
    runs: Arc<HermesRuns>,
    foreground_runs: Mutex<HashMap<RunId, ForegroundRun>>,
    activity: Arc<Mutex<HashMap<String, SessionActivity>>>,
    live_index: Mutex<Option<tokio::task::JoinHandle<()>>>,
    backfill: Mutex<Option<tokio::task::JoinHandle<()>>>,
    config_warning_emitted: AtomicBool,
}

struct ForegroundRun {
    session_id: Option<String>,
    _lease: HermesRunLease,
}

#[derive(Default)]
struct SessionActivity {
    user_turn_count: u64,
    turns_since_review: u64,
    iterations_since_skill: u64,
    memory_due: bool,
    final_response: bool,
    running: Option<Arc<ReviewRun>>,
}

struct ReviewRun {
    abort: AbortHandle,
    finished: pi_core::AbortSignal,
}

struct ReviewCompletion(AbortHandle);
impl Drop for ReviewCompletion {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn begin_review(
    activity: &Arc<Mutex<HashMap<String, SessionActivity>>>,
    session_id: String,
) -> Option<(pi_core::AbortSignal, ReviewCompletion)> {
    let mut activities = activity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = activities.entry(session_id).or_default();
    if state
        .running
        .as_ref()
        .is_some_and(|run| !run.finished.is_aborted())
    {
        return None;
    }
    let (abort, signal) = AbortHandle::new();
    let (finish, finished) = AbortHandle::new();
    state.running = Some(Arc::new(ReviewRun { abort, finished }));
    Some((signal, ReviewCompletion(finish)))
}

/// Pi-native roots managed by Hermes. The ordinary Skills plugin remains the
/// parser/catalog owner and consumes these paths while building a generation.
pub fn managed_skill_roots(agent_dir: &Path, cwd: &Path, project_trusted: bool) -> Vec<PathBuf> {
    HermesMemoryStore::managed_skill_roots(agent_dir, cwd, project_trusted)
}

impl MemoryProviderPlugin for HermesMemoryPlugin {
    fn memory_provider_id(&self) -> &str {
        HERMES_MEMORY_PROVIDER_ID
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for HermesMemoryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(HERMES_MEMORY_PLUGIN_ID)
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        commands::register(
            context,
            Arc::clone(&self.store),
            Arc::clone(&self.runs),
            self.config.clone(),
            Arc::clone(&self.activity),
        )?;
        tool::register(context, Arc::clone(&self.store), Arc::clone(&self.runs))
    }

    async fn agent_start(
        &self,
        context: AgentPluginContext,
        _: AgentStartEvent,
    ) -> Result<(), PluginError> {
        let lease = self
            .runs
            .attach(
                context.run_id().clone(),
                Arc::new(HermesRunState::default()),
            )
            .map_err(|error| hook_error(self, "agent_start", error))?;
        self.foreground_runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                context.run_id().clone(),
                ForegroundRun {
                    session_id: context.session.id().ok(),
                    _lease: lease,
                },
            );
        Ok(())
    }

    async fn agent_end(
        &self,
        context: AgentPluginContext,
        _: AgentEndEvent,
    ) -> Result<(), PluginError> {
        self.foreground_runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(context.run_id());
        Ok(())
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        self.store
            .bind_project(context.cwd())
            .map_err(|error| hook_error(self, "before_agent_start", error))?;
        self.cancel_review(&context.session.id()?).await;
        let addition = self.store.legacy_global_context();
        Ok(if addition.is_empty() {
            BeforeAgentStartPatch::default()
        } else {
            BeforeAgentStartPatch {
                system_prompt: Some(format!("{}\n\n{addition}", event.system_prompt)),
                messages: Vec::new(),
            }
        })
    }

    async fn message_end(
        &self,
        context: AgentPluginContext,
        event: MessageEndEvent,
    ) -> Result<MessageEndPatch, PluginError> {
        let session_id = context.session.id()?;
        if matches!(event.message, pi_core::Message::User(_))
            && context.session.execution_origin()? == pi_core::SessionExecutionOrigin::User
        {
            let mut activities = self
                .activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let state = activities.entry(session_id).or_default();
            state.user_turn_count = state.user_turn_count.saturating_add(1);
            state.final_response = false;
            if self.config.nudge_interval > 0
                && context
                    .session
                    .active_tools()?
                    .iter()
                    .any(|t| t == "memory")
            {
                state.turns_since_review += 1;
                if state.turns_since_review >= self.config.nudge_interval {
                    state.turns_since_review = 0;
                    state.memory_due = true;
                }
            }
        }
        self.schedule_live_index(context);
        Ok(MessageEndPatch::default())
    }

    async fn turn_end(
        &self,
        context: AgentPluginContext,
        event: TurnEndEvent,
    ) -> Result<(), PluginError> {
        if context.session.execution_origin()? != pi_core::SessionExecutionOrigin::User {
            return Ok(());
        }
        let mut activities = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = activities.entry(context.session.id()?).or_default();
        if self.config.skill_nudge_interval > 0 {
            state.iterations_since_skill += 1;
            if event
                .message
                .tool_calls()
                .iter()
                .any(|call| call.name == "skill_manage")
            {
                state.iterations_since_skill = 0;
            }
        }
        state.final_response =
            event.message.tool_calls().is_empty()
                && event.message.error_message.is_none()
                && !matches!(
                    event.message.stop_reason,
                    pi_core::StopReason::Aborted | pi_core::StopReason::Error
                )
                && event.message.content.iter().any(
                    |b| matches!(b, pi_core::ContentBlock::Text(t) if !t.text.trim().is_empty()),
                );
        Ok(())
    }

    async fn agent_settled(
        &self,
        context: AgentPluginContext,
        _: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        if context.session.execution_origin()? != pi_core::SessionExecutionOrigin::User {
            return Ok(());
        }
        let active = context.session.active_tools()?;
        let session_id = context.session.id()?;
        let mut activities = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = activities.entry(session_id.clone()).or_default();
        let memory = std::mem::take(&mut state.memory_due);
        let skills = self.config.skill_nudge_interval > 0
            && state.iterations_since_skill >= self.config.skill_nudge_interval
            && active.iter().any(|t| t == "skill_manage");
        if skills {
            state.iterations_since_skill = 0;
        }
        if !self.config.review_enabled || !state.final_response || !(memory || skills) {
            return Ok(());
        }
        state.final_response = false;
        drop(activities);
        let Some((signal, completion)) = begin_review(&self.activity, session_id) else {
            return Ok(());
        };
        let config = self.config.clone();
        let runs = Arc::clone(&self.runs);
        tokio::spawn(async move {
            let _completion = completion;
            let result = transport::run_review(
                &context.session,
                &context.models,
                &config,
                runs,
                &transport::review_prompt(memory, skills),
                signal,
                Duration::from_secs(120),
            )
            .await;
            match result {
                Ok(outcome) => {
                    for error in
                        transport::finish_review(&context.session, &context.ui, &config, &outcome)
                    {
                        context.report_hook_error("background_review", error);
                    }
                }
                Err(error) => {
                    context.report_hook_error("background_review", error);
                }
            }
        });
        Ok(())
    }
}

impl HermesMemoryPlugin {
    async fn cancel_review(&self, session_id: &str) {
        let run = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .and_then(|state| state.running.clone());
        if let Some(run) = run {
            run.abort.abort();
            let _ = tokio::time::timeout(Duration::from_secs(2), run.finished.wait()).await;
        }
    }

    fn schedule_live_index(&self, context: AgentPluginContext) {
        let mut task = self
            .live_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let store = Arc::clone(&self.store);
        *task = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            match context.session.snapshot() {
                Ok(snapshot) => {
                    if let Err(error) = store.index_snapshot(&snapshot) {
                        context.report_hook_error("live_session_index", error.to_string());
                    }
                }
                Err(error) => context.report_hook_error("live_session_index", error.to_string()),
            }
        }));
    }
}

fn hook_error(
    plugin: &impl AgentPlugin,
    hook: &'static str,
    error: impl std::fmt::Display,
) -> PluginError {
    PluginError::Hook {
        plugin_id: AgentPlugin::id(plugin),
        hook,
        message: error.to_string(),
    }
}

#[async_trait]
impl SessionPlugin for HermesMemoryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(HERMES_MEMORY_PLUGIN_ID)
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> Result<(), SessionPluginError> {
        if !self.config_warning_emitted.swap(true, Ordering::Relaxed)
            && let Some(warning) = self.config.consolidation_timeout_warning()
        {
            let _ = context.ui.notify(NoticeLevel::Warning, warning);
        }
        self.store
            .start_session(&context.identity().cwd)
            .map_err(session_error)?;
        let user_turn_count = context
            .session
            .snapshot()
            .map(|snapshot| {
                snapshot
                    .branch()
                    .iter()
                    .filter(|entry| {
                        entry
                            .raw()
                            .get("message")
                            .and_then(|m| m.get("role"))
                            .and_then(serde_json::Value::as_str)
                            == Some("user")
                    })
                    .count() as u64
            })
            .unwrap_or(0);
        let turns_since_review = if self.config.nudge_interval > 0 {
            user_turn_count % self.config.nudge_interval
        } else {
            0
        };
        self.activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                context.identity().id.clone(),
                SessionActivity {
                    user_turn_count,
                    turns_since_review,
                    ..SessionActivity::default()
                },
            );
        let needs_backfill = match self.store.needs_session_backfill() {
            Ok(needs_backfill) => needs_backfill,
            Err(error) => {
                let _ = context.ui.notify(
                    NoticeLevel::Warning,
                    format!("⚠️ Session backfill check failed: {error}"),
                );
                false
            }
        };
        let mut task = self
            .backfill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if needs_backfill
            && task
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished)
        {
            let store = Arc::clone(&self.store);
            let ui = context.ui.clone();
            *task = Some(tokio::spawn(async move {
                let result =
                    tokio::task::spawn_blocking(move || store.backfill_sessions(Some(50))).await;
                match result {
                    Ok(Ok(result)) => {
                        let level = if result.errors.is_empty() && !result.reached_limit {
                            NoticeLevel::Info
                        } else {
                            NoticeLevel::Warning
                        };
                        let error_suffix = if result.errors.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " ({} file error{})",
                                result.errors.len(),
                                if result.errors.len() == 1 { "" } else { "s" }
                            )
                        };
                        let limit_suffix = if result.reached_limit {
                            " (startup limit reached)"
                        } else {
                            ""
                        };
                        let _ = ui.notify(
                            level,
                            format!(
                                "🧠 Session backfill complete: {} indexed, {} skipped, {} messages{error_suffix}{limit_suffix}.",
                                result.sessions_indexed,
                                result.sessions_skipped,
                                result.messages_indexed,
                            ),
                        );
                    }
                    Ok(Err(error)) => {
                        let _ = ui.notify(
                            NoticeLevel::Warning,
                            format!("⚠️ Session backfill failed: {error}"),
                        );
                    }
                    Err(error) => {
                        let _ = ui.notify(
                            NoticeLevel::Warning,
                            format!("⚠️ Session backfill failed: {error}"),
                        );
                    }
                }
            }));
        }
        Ok(())
    }

    async fn session_before_compact(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<pi_session::SessionBeforeCompactResult>, SessionPluginError> {
        self.cancel_review(&context.identity().id).await;
        if self.config.flush_on_compact
            && context.session.execution_origin().map_err(session_error)?
                == pi_core::SessionExecutionOrigin::User
        {
            let user_turns = self.user_turns(&context.identity().id);
            if let Ok(snapshot) = context.session.snapshot() {
                flush::flush_if_due(
                    context,
                    &snapshot,
                    Arc::clone(&self.runs),
                    &self.config,
                    user_turns,
                    Some(event.signal.clone()),
                    Duration::from_secs(30),
                )
                .await;
            }
        }
        Ok(None)
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        event: &SessionShutdownEvent,
    ) -> Result<(), SessionPluginError> {
        self.cancel_review(&context.identity().id).await;
        if self.config.flush_on_shutdown
            && event.reason != SessionShutdownReason::Reload
            && context.session.execution_origin().map_err(session_error)?
                == pi_core::SessionExecutionOrigin::User
        {
            let user_turns = self.user_turns(&context.identity().id);
            if let Ok(snapshot) = context.session.snapshot() {
                flush::flush_if_due(
                    context,
                    &snapshot,
                    Arc::clone(&self.runs),
                    &self.config,
                    user_turns,
                    None,
                    Duration::from_secs(10),
                )
                .await;
            }
        }
        if let Ok(snapshot) = context.session.snapshot() {
            let _ = self.store.index_snapshot(&snapshot);
        }
        wait_task(&self.live_index).await;
        wait_task(&self.backfill).await;
        let _ = self.store.checkpoint();
        self.activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&context.identity().id);
        self.foreground_runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, run| run.session_id.as_deref() != Some(context.identity().id.as_str()));
        Ok(())
    }
}

impl HermesMemoryPlugin {
    fn user_turns(&self, session_id: &str) -> u64 {
        self.activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .map_or(0, |state| state.user_turn_count)
    }
}

async fn wait_task(slot: &Mutex<Option<tokio::task::JoinHandle<()>>>) {
    let task = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(task) = task {
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}

fn session_error(error: impl std::fmt::Display) -> SessionPluginError {
    SessionPluginError::Failure(error.to_string())
}

#[cfg(test)]
mod tests;
