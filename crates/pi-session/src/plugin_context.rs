use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use pi_core::{
    AbortSignal, AssistantMessage, CompactOptions, ContentBlock, CustomMessage,
    CustomMessageContent, CustomMessageInput, DirectCompletionRequest, ForkOptions,
    IsolatedSessionId, IsolatedSessionOutcome, IsolatedSessionRequest, Message, MessageDelivery,
    ModelId, ModelsContextAccess, NavigateTreeOptions, NewSessionOptions, NoticeLevel,
    PluginContextError, PluginContextReplacement, PluginContextScope, PresentationMode, ProviderId,
    SendMessageOptions, SendUserMessageOptions, SessionContextAccess, SessionEntryKind,
    SessionEntryView, SessionSnapshot, ThinkingLevel, UiContextAccess, Usage, UserMessage,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::{
    AgentSession, AgentSessionReplacement, MAIN_LANE, PiSession, QueueKind, SessionDocument,
    SessionEntryType, SessionRecord, WeakPiSession, build_context_entries,
    current_session_context_tokens,
};

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
    async fn confirm(&self, title: String, message: String) -> Result<bool, String>;

    async fn select(&self, _title: String, _options: Vec<String>) -> Result<Option<usize>, String> {
        Err("interactive selection is unavailable".to_string())
    }

    async fn multi_select(
        &self,
        _request: pi_core::UiMultiSelectRequest,
    ) -> Result<Option<pi_core::UiMultiSelectResponse>, String> {
        Err("interactive multi-selection is unavailable".to_string())
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

    fn session(&self, generation_session_id: Option<&str>) -> Option<PiSession> {
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

    fn register_project_trust(&self, session_id: String, trusted: bool) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project_trust_by_session_id
            .insert(session_id, trusted);
    }

    fn project_trust(&self, session_id: &str) -> Option<bool> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project_trust_by_session_id
            .get(session_id)
            .copied()
    }

    fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

/// `pi-session` implementation of the core plugin-context contract.
///
/// This handle is generation-bound by `pi-core::PluginContextEpoch`, but
/// its behavior is implemented against the real `AgentSession`, `PiSession`,
/// and each session's `PiRuntime`. It is not a JavaScript protocol adapter.
pub struct PiPluginContext {
    mode: PresentationMode,
    project_trusted: bool,
    generation_session: RwLock<Option<Weak<AgentSession>>>,
    binding: PluginContextBinding,
    runtime: tokio::runtime::Handle,
    provider_mutations: Option<Arc<dyn PluginProviderMutationAccess>>,
    ui_bridge: Option<Arc<dyn PluginUiBridge>>,
    provider_reload_gate: Arc<tokio::sync::Mutex<()>>,
    model_scope_patterns: Vec<String>,
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
            .register_project_trust(session.log().id().to_owned(), self.project_trusted);
        *self
            .generation_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(&session));
    }

    fn session(&self) -> Result<Arc<AgentSession>, PluginContextError> {
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
                    .map(|session| session.log().id().to_owned())
                    .as_deref(),
            )
            .map(|session| session.current())
            .or(generation)
            .ok_or(PluginContextError::Unbound)
    }

    fn pi_session(&self) -> Result<PiSession, PluginContextError> {
        let generation_session_id = self
            .generation_session
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|session| session.log().id().to_owned());
        self.binding
            .session(generation_session_id.as_deref())
            .ok_or(PluginContextError::Unbound)
    }

    fn document(&self) -> Result<(Arc<AgentSession>, SessionDocument), PluginContextError> {
        let session = self.session()?;
        let document = session
            .log()
            .load()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        Ok((session, document))
    }
}

#[async_trait]
impl UiContextAccess for PiPluginContext {
    fn mode(&self) -> Result<PresentationMode, PluginContextError> {
        Ok(self.mode)
    }

    fn has_ui(&self) -> Result<bool, PluginContextError> {
        Ok(matches!(self.mode, PresentationMode::Tui))
    }

    fn ui_notify(&self, level: NoticeLevel, message: String) -> Result<(), PluginContextError> {
        self.session()?.notify_plugin(message, level);
        Ok(())
    }

    async fn ui_confirm(&self, title: String, message: String) -> Result<bool, PluginContextError> {
        if !matches!(self.mode, PresentationMode::Tui) {
            return Ok(false);
        }
        let bridge = self.ui_bridge.as_ref().ok_or_else(|| {
            PluginContextError::Unavailable("interactive confirmation is not configured".into())
        })?;
        bridge.confirm(title, message).await.map_err(context_failed)
    }

    async fn ui_select(
        &self,
        title: String,
        options: Vec<String>,
    ) -> Result<Option<usize>, PluginContextError> {
        if !matches!(self.mode, PresentationMode::Tui) || options.is_empty() {
            return Ok(None);
        }
        let bridge = self.ui_bridge.as_ref().ok_or_else(|| {
            PluginContextError::Unavailable("interactive selection is not configured".into())
        })?;
        bridge.select(title, options).await.map_err(context_failed)
    }

    async fn ui_multi_select(
        &self,
        request: pi_core::UiMultiSelectRequest,
    ) -> Result<Option<pi_core::UiMultiSelectResponse>, PluginContextError> {
        if !matches!(self.mode, PresentationMode::Tui) {
            return Ok(None);
        }
        let bridge = self.ui_bridge.as_ref().ok_or_else(|| {
            PluginContextError::Unavailable("interactive multi-selection is not configured".into())
        })?;
        bridge.multi_select(request).await.map_err(context_failed)
    }
}

#[async_trait]
impl ModelsContextAccess for PiPluginContext {
    fn model_selection(&self) -> Result<Option<pi_core::ModelSelection>, PluginContextError> {
        let (provider, model_id) = self.session()?.runtime().agent().model_selection();
        Ok(Some(pi_core::ModelSelection { provider, model_id }))
    }

    fn model(&self) -> Result<Option<pi_core::ModelSpec>, PluginContextError> {
        let session = self.session()?;
        let (provider_id, model_id) = session.runtime().agent().model_selection();
        Ok(session.runtime().model(&provider_id, &model_id))
    }

    fn scoped_models(&self) -> Result<Vec<pi_core::ScopedModel>, PluginContextError> {
        Ok(crate::resolve_model_scope(
            &self.model_scope_patterns,
            &self.session()?.runtime().available_models(),
        ))
    }

    fn models(&self) -> Result<Vec<pi_core::ModelSpec>, PluginContextError> {
        Ok(self.session()?.runtime().models())
    }

    fn available_models(&self) -> Result<Vec<pi_core::ModelSpec>, PluginContextError> {
        Ok(self.session()?.runtime().available_models())
    }

    fn provider_display_name(&self, provider: &ProviderId) -> Result<String, PluginContextError> {
        Ok(self
            .session()?
            .runtime()
            .provider_name(provider)
            .unwrap_or_else(|| provider.to_string()))
    }

    fn thinking_level(&self) -> Result<Option<ThinkingLevel>, PluginContextError> {
        Ok(Some(self.session()?.runtime().agent().thinking_level()))
    }

    fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), PluginContextError> {
        self.session()?
            .set_thinking_level(level)
            .map_err(context_failed)
    }

    fn register_provider(&self, name: String, config: Value) -> Result<(), PluginContextError> {
        self.stage_provider_mutation(PluginProviderMutation::Register { name, config })
    }

    fn unregister_provider(&self, name: String) -> Result<(), PluginContextError> {
        self.stage_provider_mutation(PluginProviderMutation::Unregister { name })
    }

    async fn set_model(
        &self,
        scope: PluginContextScope,
        provider: ProviderId,
        model_id: ModelId,
    ) -> Result<bool, PluginContextError> {
        let _provider_reload = if scope == PluginContextScope::Command {
            Some(self.provider_reload_gate.lock().await)
        } else {
            None
        };
        if scope == PluginContextScope::Command {
            self.reload_pending_providers().await?;
        }
        let session = self.session()?;
        let runtime = session.runtime();
        if runtime.model(&provider, &model_id).is_none()
            || !runtime.provider_is_available(&provider)
        {
            return Ok(false);
        }
        session
            .set_model(provider, model_id)
            .map_err(context_failed)?;
        Ok(true)
    }
}

#[async_trait]
impl SessionContextAccess for PiPluginContext {
    fn execution_origin(&self) -> Result<pi_core::SessionExecutionOrigin, PluginContextError> {
        Ok(self.session()?.runtime().execution_origin())
    }

    async fn run_ephemeral(
        &self,
        _scope: PluginContextScope,
        request: pi_core::EphemeralSessionRequest,
        signal: AbortSignal,
    ) -> Result<pi_core::EphemeralSessionOutcome, PluginContextError> {
        self.session()?
            .runtime()
            .run_ephemeral(request, signal)
            .await
            .map_err(context_failed)
    }

    fn session_snapshot(&self) -> Result<SessionSnapshot, PluginContextError> {
        let session = self.session()?;
        let document = session
            .log()
            .load()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        let leaf_id = document
            .leaf_id(MAIN_LANE)
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .map(str::to_owned);
        let branch_ids = document
            .branch()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .into_iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let entries = document
            .entries
            .iter()
            .map(session_entry_view)
            .collect::<Result<Vec<_>, _>>()?;
        let entry_indices = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id(), index))
            .collect::<HashMap<_, _>>();
        let branch = branch_ids
            .iter()
            .map(|id| {
                entry_indices
                    .get(id.as_str())
                    .map(|&index| entries[index].clone())
                    .ok_or_else(|| {
                        PluginContextError::Failed(format!(
                            "session branch entry is missing from the snapshot: {id}"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let path = session.log().path();
        let directory = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .to_path_buf();
        let file = session.log().is_materialized().then(|| path.to_path_buf());
        let raw_header = value(&document.header)?;

        Ok(SessionSnapshot::new(
            document.header.id,
            document.header.cwd,
            directory,
            file,
            document.name,
            leaf_id,
            entries,
            branch,
            document.labels.into_iter().collect::<BTreeMap<_, _>>(),
            raw_header,
        ))
    }

    fn cwd(&self) -> Result<PathBuf, PluginContextError> {
        Ok(self.session()?.runtime().cwd().to_path_buf())
    }

    fn is_project_trusted(&self) -> Result<bool, PluginContextError> {
        let session = self.session()?;
        Ok(self
            .binding
            .project_trust(session.log().id())
            .unwrap_or(self.project_trusted))
    }

    fn is_idle(&self) -> Result<bool, PluginContextError> {
        Ok(self.session()?.is_idle())
    }

    fn has_pending_messages(&self) -> Result<bool, PluginContextError> {
        Ok(self.session()?.has_pending_messages())
    }

    fn context_usage(&self) -> Result<Option<pi_core::ContextUsage>, PluginContextError> {
        let session = self.session()?;
        let Some(context_window) = session.active_context_window() else {
            return Ok(None);
        };
        let document = session
            .log()
            .load()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        let branch = document
            .branch()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let context = document
            .context()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        let tokens =
            current_session_context_tokens(&branch, &context.messages).map(|usage| usage.tokens);
        let percent = tokens.map(|tokens| {
            if context_window == 0 {
                0.0
            } else {
                tokens as f64 / context_window as f64 * 100.0
            }
        });
        Ok(Some(pi_core::ContextUsage {
            tokens,
            context_window,
            percent,
        }))
    }

    fn system_prompt(&self) -> Result<String, PluginContextError> {
        Ok(self
            .session()?
            .runtime()
            .agent()
            .runtime()
            .system_prompt()
            .to_owned())
    }

    fn system_prompt_options(
        &self,
        scope: PluginContextScope,
    ) -> Result<Value, PluginContextError> {
        require_command(scope, "getSystemPromptOptions")?;
        let session = self.session()?;
        let runtime = session.runtime();
        runtime
            .prompt_options()
            .map_or_else(|| Ok(json!({ "cwd": runtime.cwd() })), value)
    }

    fn session_cwd(&self) -> Result<PathBuf, PluginContextError> {
        self.cwd()
    }

    fn session_dir(&self) -> Result<PathBuf, PluginContextError> {
        Ok(self
            .session()?
            .log()
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .to_path_buf())
    }

    fn session_id(&self) -> Result<String, PluginContextError> {
        Ok(self.session()?.log().id().to_owned())
    }

    fn session_file(&self) -> Result<Option<PathBuf>, PluginContextError> {
        let session = self.session()?;
        Ok(session
            .log()
            .is_materialized()
            .then(|| session.log().path().to_path_buf()))
    }

    fn session_leaf_id(&self) -> Result<Option<String>, PluginContextError> {
        Ok(self.session()?.log().leaf_id())
    }

    fn session_leaf_entry(&self) -> Result<Option<Value>, PluginContextError> {
        let session = self.session()?;
        session
            .log()
            .leaf_id()
            .and_then(|id| session.log().get_entry(&id))
            .as_ref()
            .map(value)
            .transpose()
    }

    fn session_entry(&self, id: &str) -> Result<Option<Value>, PluginContextError> {
        self.session()?
            .log()
            .get_entry(id)
            .as_ref()
            .map(value)
            .transpose()
    }

    fn session_label(&self, id: &str) -> Result<Option<String>, PluginContextError> {
        Ok(self.session()?.log().label(id))
    }

    fn session_branch(&self, from_id: Option<&str>) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        let branch = document
            .branch_at(from_id.or_else(|| document.leaf_id(MAIN_LANE).ok().flatten()))
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        values(branch)
    }

    fn session_context_entries(&self) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        let branch = document
            .branch()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        values(build_context_entries(
            &branch,
            &crate::SessionContextBuildOptions::default(),
        ))
    }

    fn session_header(&self) -> Result<Value, PluginContextError> {
        value(self.session()?.log().header())
    }

    fn session_entries(&self) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        values(document.entries)
    }

    fn session_tree(&self) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        values(session_tree(&document))
    }

    fn session_name(&self) -> Result<Option<String>, PluginContextError> {
        Ok(self.session()?.log().name())
    }

    fn active_tools(&self) -> Result<Vec<String>, PluginContextError> {
        Ok(self.session()?.runtime().active_tools())
    }

    fn all_tools(&self) -> Result<Vec<pi_core::ToolSpec>, PluginContextError> {
        Ok(self.session()?.runtime().tool_specs())
    }

    fn commands(&self) -> Result<Vec<pi_core::CommandSpec>, PluginContextError> {
        Ok(self.session()?.runtime().command_specs())
    }

    fn abort(&self) -> Result<(), PluginContextError> {
        self.session()?.abort();
        Ok(())
    }

    fn compact(&self, options: CompactOptions) -> Result<(), PluginContextError> {
        let session = self.session()?;
        self.runtime.spawn(async move {
            let _ = session.compact(options.custom_instructions).await;
        });
        Ok(())
    }

    fn shutdown(&self) -> Result<(), PluginContextError> {
        if let Ok(session) = self.pi_session() {
            session.abort();
        }
        self.binding.request_shutdown();
        Ok(())
    }

    fn send_message(
        &self,
        message: CustomMessageInput,
        options: SendMessageOptions,
    ) -> Result<(), PluginContextError> {
        let session = self.session()?;
        let running = session.runtime().agent().is_running();
        let plan = plan_custom_message(message, options, running);
        dispatch_message_plan(&self.runtime, session, plan)
    }

    fn send_user_message(
        &self,
        content: CustomMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), PluginContextError> {
        let session = self.session()?;
        let running = session.runtime().agent().is_running();
        match plan_user_message(content, options, running) {
            Ok(plan) => dispatch_message_plan(&self.runtime, session, plan),
            Err(message) => {
                session.notify_plugin(message, NoticeLevel::Error);
                Ok(())
            }
        }
    }

    fn append_entry(
        &self,
        custom_type: String,
        data: Option<Value>,
    ) -> Result<(), PluginContextError> {
        self.session()?
            .append_custom_entry(custom_type, data)
            .map_err(context_failed)?;
        Ok(())
    }

    fn record_usage(&self, usage: Usage, details: Option<Value>) -> Result<(), PluginContextError> {
        self.session()?
            .record_usage(usage, details)
            .map_err(context_failed)?;
        Ok(())
    }

    fn set_session_name(&self, name: String) -> Result<(), PluginContextError> {
        let session = self.session()?;
        let name = session
            .set_name_immediate(Some(name))
            .map_err(context_failed)?;
        self.runtime.spawn(async move {
            session
                .session_plugin_driver()
                .session_info_changed(&crate::SessionInfoChangedEvent { name })
                .await;
        });
        Ok(())
    }

    fn set_label(&self, entry_id: String, label: Option<String>) -> Result<(), PluginContextError> {
        self.session()?
            .set_label(&entry_id, label)
            .map_err(context_failed)
    }

    fn set_active_tools(&self, tool_names: Vec<String>) -> Result<(), PluginContextError> {
        let session = self.session()?;
        let available = session
            .runtime()
            .tool_specs()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<HashSet<_>>();
        session
            .set_active_tools(
                tool_names
                    .into_iter()
                    .filter(|tool| available.contains(tool)),
            )
            .map_err(context_failed)
    }

    async fn wait_for_idle(&self, scope: PluginContextScope) -> Result<(), PluginContextError> {
        require_command(scope, "waitForIdle")?;
        self.session()?.runtime().wait_for_idle().await;
        Ok(())
    }

    async fn send_message_and_wait(
        &self,
        scope: PluginContextScope,
        message: CustomMessageInput,
        options: SendMessageOptions,
    ) -> Result<(), PluginContextError> {
        require_command(scope, "sendMessage")?;
        let session = self.session()?;
        let running = session.runtime().agent().is_running();
        execute_message_plan(session, plan_custom_message(message, options, running)).await
    }

    async fn send_user_message_and_wait(
        &self,
        scope: PluginContextScope,
        content: CustomMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), PluginContextError> {
        require_command(scope, "sendUserMessage")?;
        let session = self.session()?;
        let running = session.runtime().agent().is_running();
        let plan =
            plan_user_message(content, options, running).map_err(PluginContextError::Invalid)?;
        execute_message_plan(session, plan).await
    }

    async fn complete(
        &self,
        _scope: PluginContextScope,
        request: DirectCompletionRequest,
        signal: AbortSignal,
    ) -> Result<AssistantMessage, PluginContextError> {
        let session = self.session()?;
        let runtime = session.runtime();
        let state = runtime.agent().state();
        runtime
            .complete(
                pi_runtime::RuntimeCompletionRequest {
                    system_prompt: request.system_prompt,
                    messages: request.messages,
                    model: request.model,
                    thinking_level: request.thinking_level.unwrap_or(state.thinking_level),
                    thinking_budgets: runtime.agent().thinking_budgets(),
                    max_output_tokens: request.max_output_tokens,
                },
                signal,
            )
            .await
            .map_err(context_failed)
    }

    async fn launch_isolated_session(
        &self,
        _scope: PluginContextScope,
        request: IsolatedSessionRequest,
    ) -> Result<IsolatedSessionId, PluginContextError> {
        self.pi_session()?
            .launch_isolated_session(request)
            .await
            .map_err(context_failed)
    }

    async fn wait_for_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
    ) -> Result<IsolatedSessionOutcome, PluginContextError> {
        self.pi_session()?.wait_for_isolated_session(&id).await
    }

    fn abort_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
    ) -> Result<(), PluginContextError> {
        self.pi_session()?.abort_isolated_session(&id)
    }

    async fn new_session(
        &self,
        scope: PluginContextScope,
        options: NewSessionOptions,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "newSession")?;
        let pi_session = self.pi_session()?;
        let current = pi_session.current();
        let directory = current
            .log()
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let path = directory.join(format!("{}.jsonl", uuid::Uuid::now_v7()));
        let replacement = match options.parent_session {
            Some(parent) => {
                pi_session
                    .new_session_with_parent(current.runtime().cwd(), path, parent)
                    .await
            }
            None => pi_session.new_session(current.runtime().cwd(), path).await,
        }
        .map_err(context_failed)?;
        Self::replacement(pi_session, scope, replacement)
    }

    async fn fork(
        &self,
        scope: PluginContextScope,
        entry_id: String,
        options: ForkOptions,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "fork")?;
        let pi_session = self.pi_session()?;
        let replacement = pi_session
            .fork_session(entry_id, options.position)
            .await
            .map_err(context_failed)?;
        Self::replacement(pi_session, scope, replacement)
    }

    async fn navigate_tree(
        &self,
        scope: PluginContextScope,
        target_id: String,
        options: NavigateTreeOptions,
    ) -> Result<bool, PluginContextError> {
        require_command(scope, "navigateTree")?;
        let session = self.session()?;
        if options.summarize {
            session
                .summarize_branch_and_checkout(
                    &target_id,
                    options.custom_instructions,
                    options.replace_instructions,
                    options.label,
                )
                .await
                .map_err(context_failed)?;
        } else {
            session
                .checkout(Some(&target_id))
                .await
                .map_err(context_failed)?;
            if let Some(label) = options.label {
                session
                    .set_label(&target_id, Some(label))
                    .map_err(context_failed)?;
            }
        }
        Ok(false)
    }

    async fn switch_session(
        &self,
        scope: PluginContextScope,
        session_path: PathBuf,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "switchSession")?;
        let pi_session = self.pi_session()?;
        let replacement = pi_session
            .resume_session(session_path)
            .await
            .map_err(context_failed)?;
        Self::replacement(pi_session, scope, replacement)
    }

    async fn reload(
        &self,
        scope: PluginContextScope,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "reload")?;
        let pi_session = self.pi_session()?;
        pi_session.reload().await.map_err(context_failed)?;
        Ok(PluginContextReplacement {
            cancelled: false,
            context: Some(pi_session.current().runtime().plugin_context_handle(scope)),
        })
    }
}

impl PiPluginContext {
    fn replacement(
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

    fn stage_provider_mutation(
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

    async fn reload_pending_providers(&self) -> Result<(), PluginContextError> {
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

fn require_command(
    scope: PluginContextScope,
    operation: &'static str,
) -> Result<(), PluginContextError> {
    if scope == PluginContextScope::Command {
        Ok(())
    } else {
        Err(PluginContextError::CommandOnly(operation))
    }
}

fn context_failed(error: impl std::fmt::Display) -> PluginContextError {
    PluginContextError::Failed(error.to_string())
}

fn value<T: Serialize>(value: T) -> Result<Value, PluginContextError> {
    serde_json::to_value(value).map_err(context_failed)
}

fn values<T: Serialize>(items: T) -> Result<Vec<Value>, PluginContextError> {
    match value(items)? {
        Value::Array(values) => Ok(values),
        _ => Err(PluginContextError::Failed(
            "context value did not serialize as an array".to_string(),
        )),
    }
}

fn session_entry_view(record: &SessionRecord) -> Result<SessionEntryView, PluginContextError> {
    let kind = match record.entry.entry_type() {
        SessionEntryType::Message => SessionEntryKind::Message,
        SessionEntryType::CustomMessage => SessionEntryKind::CustomMessage,
        SessionEntryType::ModelChange => SessionEntryKind::ModelChange,
        SessionEntryType::ThinkingLevelChange => SessionEntryKind::ThinkingLevelChange,
        SessionEntryType::ActiveToolsChange => SessionEntryKind::ActiveToolsChange,
        SessionEntryType::Compaction => SessionEntryKind::Compaction,
        SessionEntryType::BranchSummary => SessionEntryKind::BranchSummary,
        SessionEntryType::Custom => SessionEntryKind::Custom,
    };
    Ok(SessionEntryView::new(
        record.id.clone(),
        record.parent_id.clone(),
        record.timestamp_ms,
        kind,
        value(record)?,
    ))
}

fn unix_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

const BUSY_USER_MESSAGE: &str =
    "Agent is already processing. Specify deliverAs ('steer' or 'followUp') to queue the message.";

enum PluginMessagePlan {
    Enqueue {
        message: Message,
        kind: QueueKind,
    },
    Prompt(Message),
    Submit(String),
    AppendCustom {
        message: CustomMessage,
        wait_for_idle: bool,
    },
}

fn plan_custom_message(
    message: CustomMessageInput,
    options: SendMessageOptions,
    running: bool,
) -> PluginMessagePlan {
    let message = message.into_message(unix_timestamp_ms());
    if options.deliver_as == Some(MessageDelivery::NextTurn) {
        PluginMessagePlan::Enqueue {
            message: Message::custom(message),
            kind: QueueKind::NextRun,
        }
    } else if running && options.trigger_turn != Some(false) {
        PluginMessagePlan::Enqueue {
            message: Message::custom(message),
            kind: delivery_queue(options.deliver_as),
        }
    } else if options.trigger_turn == Some(true) {
        PluginMessagePlan::Prompt(Message::custom(message))
    } else {
        PluginMessagePlan::AppendCustom {
            message,
            wait_for_idle: running,
        }
    }
}

fn plan_user_message(
    content: CustomMessageContent,
    options: SendUserMessageOptions,
    running: bool,
) -> Result<PluginMessagePlan, String> {
    if running && options.deliver_as.is_none() {
        return Err(BUSY_USER_MESSAGE.to_string());
    }
    let (text, blocks) = plugin_user_message(content);
    if running {
        Ok(PluginMessagePlan::Enqueue {
            message: Message::User(UserMessage {
                content: blocks,
                timestamp_ms: unix_timestamp_ms(),
            }),
            kind: delivery_queue(options.deliver_as),
        })
    } else if options.expand_prompt_templates {
        Ok(PluginMessagePlan::Submit(text))
    } else {
        Ok(PluginMessagePlan::Prompt(Message::User(UserMessage {
            content: blocks,
            timestamp_ms: unix_timestamp_ms(),
        })))
    }
}

fn delivery_queue(delivery: Option<MessageDelivery>) -> QueueKind {
    match delivery {
        Some(MessageDelivery::FollowUp) => QueueKind::FollowUp,
        Some(MessageDelivery::NextTurn) => QueueKind::NextRun,
        Some(MessageDelivery::Steer) | None => QueueKind::Steer,
    }
}

fn dispatch_message_plan(
    runtime: &tokio::runtime::Handle,
    session: Arc<AgentSession>,
    plan: PluginMessagePlan,
) -> Result<(), PluginContextError> {
    match plan {
        PluginMessagePlan::Enqueue { message, kind } => session
            .enqueue_message(message, kind)
            .map(|_| ())
            .map_err(context_failed),
        PluginMessagePlan::AppendCustom {
            message,
            wait_for_idle: false,
        } => session
            .append_custom_message(message)
            .map(|_| ())
            .map_err(context_failed),
        plan => {
            runtime.spawn(async move {
                let _ = execute_message_plan(session, plan).await;
            });
            Ok(())
        }
    }
}

async fn execute_message_plan(
    session: Arc<AgentSession>,
    plan: PluginMessagePlan,
) -> Result<(), PluginContextError> {
    match plan {
        PluginMessagePlan::Enqueue { message, kind } => session
            .enqueue_message(message, kind)
            .map(|_| ())
            .map_err(context_failed),
        PluginMessagePlan::Prompt(message) => session
            .prompt(vec![message])
            .await
            .map(|_| ())
            .map_err(context_failed),
        PluginMessagePlan::Submit(text) => session
            .submit(text)
            .await
            .map(|_| ())
            .map_err(context_failed),
        PluginMessagePlan::AppendCustom {
            message,
            wait_for_idle,
        } => {
            if wait_for_idle {
                session.runtime().wait_for_idle().await;
            }
            session
                .append_custom_message(message)
                .map(|_| ())
                .map_err(context_failed)
        }
    }
}

fn plugin_user_message(content: CustomMessageContent) -> (String, Vec<ContentBlock>) {
    match content {
        CustomMessageContent::Text(text) => {
            let blocks = vec![ContentBlock::Text(pi_core::TextContent::new(&text))];
            (text, blocks)
        }
        CustomMessageContent::Blocks(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut normalized = vec![ContentBlock::Text(pi_core::TextContent::new(&text))];
            normalized.extend(
                blocks
                    .into_iter()
                    .filter(|block| matches!(block, ContentBlock::Image(_))),
            );
            (text, normalized)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTreeWireNode {
    entry: SessionRecord,
    children: Vec<SessionTreeWireNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn session_tree(document: &SessionDocument) -> Vec<SessionTreeWireNode> {
    let ids = document
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<HashSet<_>>();
    let mut children = HashMap::<Option<String>, Vec<SessionRecord>>::new();
    for entry in &document.entries {
        let parent = entry
            .parent_id
            .as_ref()
            .filter(|parent| parent.as_str() != entry.id && ids.contains(parent.as_str()))
            .cloned();
        children.entry(parent).or_default().push(entry.clone());
    }
    for entries in children.values_mut() {
        entries.sort_by_key(|entry| entry.timestamp_ms);
    }

    fn build(
        entry: SessionRecord,
        children: &mut HashMap<Option<String>, Vec<SessionRecord>>,
        labels: &HashMap<String, String>,
    ) -> SessionTreeWireNode {
        let descendants = children
            .remove(&Some(entry.id.clone()))
            .unwrap_or_default()
            .into_iter()
            .map(|child| build(child, children, labels))
            .collect();
        let label = labels.get(&entry.id).cloned();
        SessionTreeWireNode {
            entry,
            children: descendants,
            label,
        }
    }

    children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| build(entry, &mut children, &document.labels))
        .collect()
}
