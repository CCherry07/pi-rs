use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use pi_core::{
    ContentBlock, CustomMessage, CustomMessageContent, Message, ModelId, ProviderId, ThinkingLevel,
    UserMessage,
};
use pi_session::{
    AgentSession, AgentSessionReplacement, ExtensionNoticeLevel, ForkPosition, MAIN_LANE,
    PiSession, QueueKind, SessionDocument, SessionRecord, WeakPiSession, build_context_entries,
    current_session_context_tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

/// The capability set attached to one JavaScript callback invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionContextScope {
    Base,
    Command,
}

/// Synchronous, side-effect-free reads exposed by `NativeExtensionContext.query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextQuery {
    Cwd,
    IsProjectTrusted,
    Model,
    ScopedModels,
    Models,
    AvailableModels,
    ProviderDisplayName { provider: String },
    ThinkingLevel,
    IsIdle,
    HasPendingMessages,
    ContextUsage,
    SystemPrompt,
    SystemPromptOptions,
    SessionCwd,
    SessionDir,
    SessionId,
    SessionFile,
    SessionLeafId,
    SessionLeafEntry,
    SessionEntry { id: String },
    SessionLabel { id: String },
    SessionBranch { from_id: Option<String> },
    SessionContextEntries,
    SessionHeader,
    SessionEntries,
    SessionTree,
    SessionName,
    ActiveTools,
    AllTools,
    Commands,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCustomMessage {
    pub custom_type: String,
    #[serde(default)]
    pub content: CustomMessageContent,
    #[serde(default)]
    pub display: bool,
    #[serde(default)]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionMessageDelivery {
    #[default]
    Steer,
    FollowUp,
    NextTurn,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSendMessageOptions {
    #[serde(default)]
    pub trigger_turn: Option<bool>,
    #[serde(default)]
    pub deliver_as: Option<ExtensionMessageDelivery>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionUserMessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSendUserMessageOptions {
    #[serde(default)]
    pub deliver_as: Option<ExtensionMessageDelivery>,
    #[serde(default)]
    pub expand_prompt_templates: bool,
}

/// Fire-and-forget commands exposed by `NativeExtensionContext.notify`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextNotification {
    Abort,
    Compact {
        #[serde(default)]
        custom_instructions: Option<String>,
    },
    Shutdown,
    UiNotify {
        message: String,
        level: ExtensionNoticeLevel,
    },
    SendMessage {
        message: ExtensionCustomMessage,
        #[serde(default)]
        options: ExtensionSendMessageOptions,
    },
    SendUserMessage {
        content: ExtensionUserMessageContent,
        #[serde(default)]
        options: ExtensionSendUserMessageOptions,
    },
    AppendEntry {
        custom_type: String,
        #[serde(default)]
        data: Option<Value>,
    },
    SetSessionName {
        name: String,
    },
    SetLabel {
        entry_id: String,
        #[serde(default)]
        label: Option<String>,
    },
    SetActiveTools {
        tool_names: Vec<String>,
    },
    SetThinkingLevel {
        level: String,
    },
    RegisterProvider {
        name: String,
        config: Value,
    },
    UnregisterProvider {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionProviderMutation {
    Register { name: String, config: Value },
    Unregister { name: String },
}

/// Generation-external staging seam for JavaScript provider mutations.
/// Implementations keep published registries immutable and expose pending
/// mutations to the next product generation transaction.
pub trait ExtensionProviderMutationAccess: Send + Sync {
    fn stage(&self, mutation: ExtensionProviderMutation) -> Result<(), String>;
    fn has_pending(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionForkPosition {
    Before,
    At,
}

/// Awaited commands exposed by `NativeExtensionContext.request`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextRequest {
    WaitForIdle,
    SendMessage {
        message: ExtensionCustomMessage,
        #[serde(default)]
        options: ExtensionSendMessageOptions,
    },
    SendUserMessage {
        content: ExtensionUserMessageContent,
        #[serde(default)]
        options: ExtensionSendUserMessageOptions,
    },
    NewSession {
        #[serde(default)]
        parent_session: Option<String>,
    },
    Fork {
        entry_id: String,
        #[serde(default = "default_fork_position")]
        position: ExtensionForkPosition,
    },
    NavigateTree {
        target_id: String,
        #[serde(default)]
        summarize: bool,
        #[serde(default)]
        custom_instructions: Option<String>,
        #[serde(default)]
        replace_instructions: bool,
        #[serde(default)]
        label: Option<String>,
    },
    SwitchSession {
        session_path: PathBuf,
    },
    Reload,
    SetModel {
        provider: String,
        model_id: String,
    },
}

fn default_fork_position() -> ExtensionForkPosition {
    ExtensionForkPosition::Before
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionContextError {
    #[error("JavaScript extension context has retired")]
    Retired,
    #[error("JavaScript extension context is not bound to a session")]
    Unbound,
    #[error("{0} is only available in an extension command context")]
    CommandOnly(&'static str),
    #[error("JavaScript extension context capability is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid JavaScript extension context operation: {0}")]
    Invalid(String),
    #[error("JavaScript extension context operation failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait ExtensionContextAccess: Send + Sync {
    fn query(
        &self,
        scope: ExtensionContextScope,
        query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError>;

    fn notify(
        &self,
        scope: ExtensionContextScope,
        notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError>;

    async fn request(
        &self,
        scope: ExtensionContextScope,
        request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError>;

    fn retire(&self) {}
}

struct ExtensionContextEpochInner {
    active: AtomicBool,
    access: Arc<dyn ExtensionContextAccess>,
}

/// Generation-owned lifetime guard for native extension contexts.
#[derive(Clone)]
pub struct ExtensionContextEpoch {
    inner: Arc<ExtensionContextEpochInner>,
}

impl ExtensionContextEpoch {
    pub fn new(access: Arc<dyn ExtensionContextAccess>) -> Self {
        Self {
            inner: Arc::new(ExtensionContextEpochInner {
                active: AtomicBool::new(true),
                access,
            }),
        }
    }

    pub fn unavailable() -> Self {
        Self::new(Arc::new(UnavailableExtensionContextAccess))
    }

    pub fn handle(&self, scope: ExtensionContextScope) -> ExtensionContextHandle {
        ExtensionContextHandle {
            epoch: self.clone(),
            scope,
        }
    }

    pub fn retire(&self) {
        if self.inner.active.swap(false, Ordering::AcqRel) {
            self.inner.access.retire();
        }
    }

    pub(crate) fn query_for_adapter(
        &self,
        query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError> {
        if !self.inner.active.load(Ordering::Acquire) {
            return Err(ExtensionContextError::Retired);
        }
        self.inner
            .access
            .query(ExtensionContextScope::Command, query)
    }
}

/// Cloneable native handle passed directly to one Node callback.
#[derive(Clone)]
pub struct ExtensionContextHandle {
    epoch: ExtensionContextEpoch,
    scope: ExtensionContextScope,
}

impl ExtensionContextHandle {
    pub fn query(&self, query: ExtensionContextQuery) -> Result<Value, ExtensionContextError> {
        self.ensure_active()?;
        self.epoch.inner.access.query(self.scope, query)
    }

    pub fn notify(
        &self,
        notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError> {
        self.ensure_active()?;
        self.epoch.inner.access.notify(self.scope, notification)
    }

    pub async fn request(
        &self,
        request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError> {
        self.ensure_active()?;
        let result = self.epoch.inner.access.request(self.scope, request).await;
        self.ensure_active()?;
        result
    }

    pub fn scope(&self) -> ExtensionContextScope {
        self.scope
    }

    fn ensure_active(&self) -> Result<(), ExtensionContextError> {
        self.epoch
            .inner
            .active
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or(ExtensionContextError::Retired)
    }
}

pub(crate) struct UnavailableExtensionContextAccess;

#[async_trait]
impl ExtensionContextAccess for UnavailableExtensionContextAccess {
    fn query(
        &self,
        _scope: ExtensionContextScope,
        _query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError> {
        Err(ExtensionContextError::Unbound)
    }

    fn notify(
        &self,
        _scope: ExtensionContextScope,
        _notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError> {
        Err(ExtensionContextError::Unbound)
    }

    async fn request(
        &self,
        _scope: ExtensionContextScope,
        _request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError> {
        Err(ExtensionContextError::Unbound)
    }
}

#[derive(Default)]
struct ExtensionSessionBindingState {
    session: Option<WeakPiSession>,
    project_trust_by_session_id: HashMap<String, bool>,
}

/// Stable outer-session capability shared by every JavaScript generation.
#[derive(Clone)]
pub struct ExtensionSessionBinding {
    state: Arc<Mutex<ExtensionSessionBindingState>>,
    shutdown: watch::Sender<bool>,
}

impl Default for ExtensionSessionBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionSessionBinding {
    pub fn new() -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            state: Arc::new(Mutex::new(ExtensionSessionBindingState::default())),
            shutdown,
        }
    }

    pub fn bind(&self, session: PiSession) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session = Some(session.downgrade());
    }

    pub async fn wait_for_shutdown(&self) {
        let mut receiver = self.shutdown.subscribe();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }

    fn session(&self) -> Option<PiSession> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .as_ref()
            .and_then(WeakPiSession::upgrade)
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

/// Production context implementation backed by one prepared generation and
/// the stable outer [`PiSession`] handle once application startup completes.
pub struct SessionExtensionContextAccess {
    project_trusted: bool,
    generation_session: RwLock<Option<Weak<AgentSession>>>,
    binding: ExtensionSessionBinding,
    runtime: tokio::runtime::Handle,
    provider_mutations: Option<Arc<dyn ExtensionProviderMutationAccess>>,
    provider_reload_gate: Arc<tokio::sync::Mutex<()>>,
}

impl SessionExtensionContextAccess {
    pub fn new(project_trusted: bool, binding: ExtensionSessionBinding) -> Self {
        Self {
            project_trusted,
            generation_session: RwLock::new(None),
            binding,
            runtime: tokio::runtime::Handle::current(),
            provider_mutations: None,
            provider_reload_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn with_provider_mutations(
        mut self,
        access: Arc<dyn ExtensionProviderMutationAccess>,
    ) -> Self {
        self.provider_mutations = Some(access);
        self
    }

    pub fn bind_generation_session(&self, session: Arc<AgentSession>) {
        self.binding
            .register_project_trust(session.log().header().id, self.project_trusted);
        *self
            .generation_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(&session));
    }

    fn session(&self) -> Result<Arc<AgentSession>, ExtensionContextError> {
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
            .session()
            .map(|session| session.current())
            .or(generation)
            .ok_or(ExtensionContextError::Unbound)
    }

    fn pi_session(&self) -> Result<PiSession, ExtensionContextError> {
        self.binding.session().ok_or(ExtensionContextError::Unbound)
    }

    fn document(&self) -> Result<(Arc<AgentSession>, SessionDocument), ExtensionContextError> {
        let session = self.session()?;
        let document = session
            .log()
            .load()
            .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
        Ok((session, document))
    }
}

#[async_trait]
impl ExtensionContextAccess for SessionExtensionContextAccess {
    fn query(
        &self,
        scope: ExtensionContextScope,
        query: ExtensionContextQuery,
    ) -> Result<Value, ExtensionContextError> {
        let session = self.session()?;
        let runtime = session.runtime();
        let snapshot = session.snapshot();
        match query {
            ExtensionContextQuery::Cwd => value(runtime.cwd()),
            ExtensionContextQuery::IsProjectTrusted => {
                let id = session.log().header().id;
                Ok(json!(
                    self.binding
                        .project_trust(&id)
                        .unwrap_or(self.project_trusted)
                ))
            }
            ExtensionContextQuery::Model => {
                let state = snapshot.agent;
                value(runtime.model(&state.provider_id, &state.model_id))
            }
            ExtensionContextQuery::ScopedModels => Ok(json!([])),
            ExtensionContextQuery::Models => value(runtime.models()),
            ExtensionContextQuery::AvailableModels => value(runtime.available_models()),
            ExtensionContextQuery::ProviderDisplayName { provider } => Ok(json!(
                runtime
                    .provider_name(&pi_core::ProviderId::new(&provider))
                    .unwrap_or(provider)
            )),
            ExtensionContextQuery::ThinkingLevel => value(snapshot.agent.thinking_level),
            ExtensionContextQuery::IsIdle => Ok(json!(
                !runtime.agent().state().is_running
                    && snapshot.compaction.is_none()
                    && snapshot.bash.is_none()
            )),
            ExtensionContextQuery::HasPendingMessages => Ok(json!(
                !snapshot.queue.steering.is_empty() || !snapshot.queue.follow_up.is_empty()
            )),
            ExtensionContextQuery::ContextUsage => {
                let Some(context_window) = session.active_context_window() else {
                    return Ok(Value::Null);
                };
                let document = session
                    .log()
                    .load()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                let branch = document
                    .branch()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                let context = document
                    .context()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                let usage = current_session_context_tokens(&branch, &context.messages);
                let tokens = usage.map(|usage| usage.tokens);
                let percent = tokens.map(|tokens| {
                    if context_window == 0 {
                        0.0
                    } else {
                        tokens as f64 / context_window as f64 * 100.0
                    }
                });
                Ok(json!({
                    "tokens": tokens,
                    "contextWindow": context_window,
                    "percent": percent,
                }))
            }
            ExtensionContextQuery::SystemPrompt => Ok(json!(snapshot.agent.system_prompt)),
            ExtensionContextQuery::SystemPromptOptions => {
                require_command(scope, "getSystemPromptOptions")?;
                runtime
                    .prompt_options()
                    .map_or_else(|| Ok(json!({ "cwd": runtime.cwd() })), value)
            }
            ExtensionContextQuery::SessionCwd => value(runtime.cwd()),
            ExtensionContextQuery::SessionDir => value(
                session
                    .log()
                    .path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("")),
            ),
            ExtensionContextQuery::SessionId => Ok(json!(session.log().header().id)),
            ExtensionContextQuery::SessionFile => {
                if session.log().is_materialized() {
                    value(session.log().path())
                } else {
                    Ok(Value::Null)
                }
            }
            ExtensionContextQuery::SessionLeafId => Ok(json!(session.log().leaf_id())),
            ExtensionContextQuery::SessionLeafEntry => {
                let (_, document) = self.document()?;
                let entry = document
                    .leaf_id(MAIN_LANE)
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?
                    .and_then(|id| document.entries.iter().find(|entry| entry.id == id));
                value(entry)
            }
            ExtensionContextQuery::SessionEntry { id } => {
                let (_, document) = self.document()?;
                value(document.entries.iter().find(|entry| entry.id == id))
            }
            ExtensionContextQuery::SessionLabel { id } => {
                let (_, document) = self.document()?;
                value(document.labels.get(&id))
            }
            ExtensionContextQuery::SessionBranch { from_id } => {
                let (_, document) = self.document()?;
                let branch = document
                    .branch_at(
                        from_id
                            .as_deref()
                            .or_else(|| document.leaf_id(MAIN_LANE).ok().flatten()),
                    )
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                value(branch)
            }
            ExtensionContextQuery::SessionContextEntries => {
                let (_, document) = self.document()?;
                let branch = document
                    .branch()
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                value(build_context_entries(
                    &branch,
                    &pi_session::SessionContextBuildOptions::default(),
                ))
            }
            ExtensionContextQuery::SessionHeader => {
                let (_, document) = self.document()?;
                value(document.header)
            }
            ExtensionContextQuery::SessionEntries => {
                let (_, document) = self.document()?;
                value(document.entries)
            }
            ExtensionContextQuery::SessionTree => {
                let (_, document) = self.document()?;
                value(session_tree(&document))
            }
            ExtensionContextQuery::SessionName => {
                let (_, document) = self.document()?;
                value(document.name)
            }
            ExtensionContextQuery::ActiveTools => value(runtime.active_tools()),
            ExtensionContextQuery::AllTools => Ok(Value::Array(
                runtime
                    .tool_specs()
                    .into_iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "label": tool.label,
                            "description": tool.description,
                            "parameters": tool.parameters,
                            "promptSnippet": tool.prompt_snippet,
                            "promptGuidelines": tool.prompt_guidelines,
                        })
                    })
                    .collect(),
            )),
            ExtensionContextQuery::Commands => Ok(Value::Array(
                runtime
                    .command_specs()
                    .into_iter()
                    .map(|command| {
                        json!({
                            "name": command.name,
                            "description": command.description,
                            "argumentHint": command.argument_hint,
                        })
                    })
                    .collect(),
            )),
        }
    }

    fn notify(
        &self,
        _scope: ExtensionContextScope,
        notification: ExtensionContextNotification,
    ) -> Result<(), ExtensionContextError> {
        match notification {
            ExtensionContextNotification::Abort => self.session()?.abort(),
            ExtensionContextNotification::Compact {
                custom_instructions,
            } => {
                let session = self.session()?;
                self.runtime.spawn(async move {
                    let _ = session.compact(custom_instructions).await;
                });
            }
            ExtensionContextNotification::Shutdown => {
                if let Ok(session) = self.pi_session() {
                    session.abort();
                }
                self.binding.request_shutdown();
            }
            ExtensionContextNotification::UiNotify { message, level } => {
                self.session()?.notify_extension(message, level);
            }
            ExtensionContextNotification::SendMessage { message, options } => {
                let session = self.session()?;
                let message = Message::custom(CustomMessage {
                    custom_type: message.custom_type,
                    content: message.content,
                    display: message.display,
                    details: message.details,
                    timestamp_ms: unix_timestamp_ms(),
                });
                if options.deliver_as == Some(ExtensionMessageDelivery::NextTurn) {
                    session
                        .enqueue_extension_message(message, QueueKind::NextRun)
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else if session.snapshot().agent.is_running && options.trigger_turn != Some(false)
                {
                    let kind = match options.deliver_as {
                        Some(ExtensionMessageDelivery::FollowUp) => QueueKind::FollowUp,
                        _ => QueueKind::Steer,
                    };
                    session
                        .enqueue_extension_message(message, kind)
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else if options.trigger_turn == Some(true) {
                    self.runtime.spawn(async move {
                        let _ = session.prompt(vec![message]).await;
                    });
                } else if session.snapshot().agent.is_running {
                    self.runtime.spawn(async move {
                        session.runtime().wait_for_idle().await;
                        let Message::Custom(message) = message else {
                            return;
                        };
                        let _ = session.append_custom_message((*message).clone());
                    });
                } else {
                    let Message::Custom(message) = message else {
                        unreachable!("extension custom messages always use the custom role");
                    };
                    session
                        .append_custom_message((*message).clone())
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                }
            }
            ExtensionContextNotification::SendUserMessage { content, options } => {
                let session = self.session()?;
                if session.snapshot().agent.is_running && options.deliver_as.is_none() {
                    session.notify_extension(
                        "Agent is already processing. Specify deliverAs ('steer' or 'followUp') to queue the message."
                            .to_string(),
                        ExtensionNoticeLevel::Error,
                    );
                    return Ok(());
                }
                let runtime = self.runtime.clone();
                runtime.spawn(async move {
                    let (text, blocks) = extension_user_message(content);
                    if session.snapshot().agent.is_running {
                        let kind = match options.deliver_as {
                            Some(ExtensionMessageDelivery::FollowUp) => QueueKind::FollowUp,
                            _ => QueueKind::Steer,
                        };
                        let message = Message::User(UserMessage {
                            content: blocks,
                            timestamp_ms: unix_timestamp_ms(),
                        });
                        let _ = session.enqueue_extension_message(message, kind);
                    } else if options.expand_prompt_templates {
                        let _ = session.submit(text).await;
                    } else {
                        let message = Message::User(UserMessage {
                            content: blocks,
                            timestamp_ms: unix_timestamp_ms(),
                        });
                        let _ = session.prompt(vec![message]).await;
                    }
                });
            }
            ExtensionContextNotification::AppendEntry { custom_type, data } => {
                self.session()?
                    .append_custom_entry(custom_type, data)
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
            }
            ExtensionContextNotification::SetSessionName { name } => {
                let session = self.session()?;
                let name = session
                    .set_name_immediate(Some(name))
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                self.runtime.spawn(async move {
                    session
                        .session_plugin_driver()
                        .session_info_changed(&pi_session::SessionInfoChangedEvent { name })
                        .await;
                });
            }
            ExtensionContextNotification::SetLabel { entry_id, label } => {
                self.session()?
                    .set_label(&entry_id, label)
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
            }
            ExtensionContextNotification::SetActiveTools { tool_names } => {
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
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
            }
            ExtensionContextNotification::SetThinkingLevel { level } => {
                let level = level
                    .parse::<ThinkingLevel>()
                    .map_err(ExtensionContextError::Invalid)?;
                self.session()?
                    .set_thinking_level(level)
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
            }
            ExtensionContextNotification::RegisterProvider { name, config } => {
                self.stage_provider_mutation(ExtensionProviderMutation::Register { name, config })?;
            }
            ExtensionContextNotification::UnregisterProvider { name } => {
                self.stage_provider_mutation(ExtensionProviderMutation::Unregister { name })?;
            }
        }
        Ok(())
    }

    async fn request(
        &self,
        scope: ExtensionContextScope,
        request: ExtensionContextRequest,
    ) -> Result<Value, ExtensionContextError> {
        match request {
            ExtensionContextRequest::WaitForIdle => {
                require_command(scope, "waitForIdle")?;
                self.session()?.runtime().wait_for_idle().await;
                Ok(Value::Null)
            }
            ExtensionContextRequest::SendMessage { message, options } => {
                require_command(scope, "sendMessage")?;
                let session = self.session()?;
                let message = Message::custom(CustomMessage {
                    custom_type: message.custom_type,
                    content: message.content,
                    display: message.display,
                    details: message.details,
                    timestamp_ms: unix_timestamp_ms(),
                });
                if options.deliver_as == Some(ExtensionMessageDelivery::NextTurn) {
                    session
                        .enqueue_extension_message(message, QueueKind::NextRun)
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else if session.snapshot().agent.is_running && options.trigger_turn != Some(false)
                {
                    let kind = match options.deliver_as {
                        Some(ExtensionMessageDelivery::FollowUp) => QueueKind::FollowUp,
                        _ => QueueKind::Steer,
                    };
                    session
                        .enqueue_extension_message(message, kind)
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else if options.trigger_turn == Some(true) {
                    session
                        .prompt(vec![message])
                        .await
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else {
                    if session.snapshot().agent.is_running {
                        session.runtime().wait_for_idle().await;
                    }
                    let Message::Custom(message) = message else {
                        unreachable!("extension custom messages always use the custom role");
                    };
                    session
                        .append_custom_message((*message).clone())
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                }
                Ok(Value::Null)
            }
            ExtensionContextRequest::SendUserMessage { content, options } => {
                require_command(scope, "sendUserMessage")?;
                let session = self.session()?;
                if session.snapshot().agent.is_running && options.deliver_as.is_none() {
                    return Err(ExtensionContextError::Invalid(
                        "Agent is already processing. Specify deliverAs ('steer' or 'followUp') to queue the message."
                            .to_string(),
                    ));
                }
                let (text, blocks) = extension_user_message(content);
                if session.snapshot().agent.is_running {
                    let kind = match options.deliver_as {
                        Some(ExtensionMessageDelivery::FollowUp) => QueueKind::FollowUp,
                        _ => QueueKind::Steer,
                    };
                    session
                        .enqueue_extension_message(
                            Message::User(UserMessage {
                                content: blocks,
                                timestamp_ms: unix_timestamp_ms(),
                            }),
                            kind,
                        )
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else if options.expand_prompt_templates {
                    session
                        .submit(text)
                        .await
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else {
                    session
                        .prompt(vec![Message::User(UserMessage {
                            content: blocks,
                            timestamp_ms: unix_timestamp_ms(),
                        })])
                        .await
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                }
                Ok(Value::Null)
            }
            ExtensionContextRequest::NewSession { parent_session } => {
                require_command(scope, "newSession")?;
                let pi_session = self.pi_session()?;
                let current = pi_session.current();
                let directory = current
                    .log()
                    .path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let path = directory.join(format!("{}.jsonl", uuid::Uuid::now_v7()));
                let replacement = match parent_session {
                    Some(parent) => {
                        pi_session
                            .new_session_with_parent(current.runtime().cwd(), path, parent)
                            .await
                    }
                    None => pi_session.new_session(current.runtime().cwd(), path).await,
                }
                .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                replacement_value(replacement)
            }
            ExtensionContextRequest::Fork { entry_id, position } => {
                require_command(scope, "fork")?;
                let position = match position {
                    ExtensionForkPosition::Before => ForkPosition::Before,
                    ExtensionForkPosition::At => ForkPosition::At,
                };
                let replacement = self
                    .pi_session()?
                    .fork_session(entry_id, position)
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                replacement_value(replacement)
            }
            ExtensionContextRequest::NavigateTree {
                target_id,
                summarize,
                custom_instructions,
                replace_instructions,
                label,
            } => {
                require_command(scope, "navigateTree")?;
                let session = self.session()?;
                if summarize {
                    session
                        .summarize_branch_and_checkout(
                            &target_id,
                            custom_instructions,
                            replace_instructions,
                            label,
                        )
                        .await
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                } else {
                    session
                        .checkout(Some(&target_id))
                        .await
                        .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                    if let Some(label) = label {
                        session
                            .set_label(&target_id, Some(label))
                            .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                    }
                }
                Ok(json!({ "cancelled": false }))
            }
            ExtensionContextRequest::SwitchSession { session_path } => {
                require_command(scope, "switchSession")?;
                let replacement = self
                    .pi_session()?
                    .resume_session(session_path)
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                replacement_value(replacement)
            }
            ExtensionContextRequest::Reload => {
                require_command(scope, "reload")?;
                self.pi_session()?
                    .reload()
                    .await
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                Ok(Value::Null)
            }
            ExtensionContextRequest::SetModel { provider, model_id } => {
                let _provider_reload = if scope == ExtensionContextScope::Command {
                    Some(self.provider_reload_gate.lock().await)
                } else {
                    None
                };
                if scope == ExtensionContextScope::Command {
                    self.reload_pending_providers().await?;
                }
                let provider = ProviderId::new(provider);
                let model_id = ModelId::new(model_id);
                let session = self.session()?;
                let runtime = session.runtime();
                if runtime.model(&provider, &model_id).is_none()
                    || !runtime.provider_is_available(&provider)
                {
                    return Ok(json!(false));
                }
                session
                    .set_model(provider, model_id)
                    .map_err(|error| ExtensionContextError::Failed(error.to_string()))?;
                Ok(json!(true))
            }
        }
    }

    fn retire(&self) {
        self.generation_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

impl SessionExtensionContextAccess {
    fn stage_provider_mutation(
        &self,
        mutation: ExtensionProviderMutation,
    ) -> Result<(), ExtensionContextError> {
        let access = self.provider_mutations.as_ref().ok_or_else(|| {
            ExtensionContextError::Unavailable(
                "dynamic provider registration is not configured".to_string(),
            )
        })?;
        access
            .stage(mutation)
            .map_err(ExtensionContextError::Invalid)?;
        let session = self.pi_session()?;
        let access = Arc::clone(access);
        let gate = Arc::clone(&self.provider_reload_gate);
        self.runtime.spawn(async move {
            let _reload = gate.lock().await;
            if !access.has_pending() {
                return;
            }
            if let Err(error) = session.reload().await {
                session.current().notify_extension(
                    format!("JavaScript provider update failed: {error}"),
                    ExtensionNoticeLevel::Error,
                );
            }
        });
        Ok(())
    }

    async fn reload_pending_providers(&self) -> Result<(), ExtensionContextError> {
        if !self
            .provider_mutations
            .as_ref()
            .is_some_and(|mutations| mutations.has_pending())
        {
            return Ok(());
        }
        self.pi_session()?
            .reload()
            .await
            .map_err(|error| ExtensionContextError::Failed(error.to_string()))
    }
}

fn require_command(
    scope: ExtensionContextScope,
    operation: &'static str,
) -> Result<(), ExtensionContextError> {
    if scope == ExtensionContextScope::Command {
        Ok(())
    } else {
        Err(ExtensionContextError::CommandOnly(operation))
    }
}

fn replacement_value(replacement: AgentSessionReplacement) -> Result<Value, ExtensionContextError> {
    Ok(json!({
        "cancelled": replacement == AgentSessionReplacement::Cancelled,
    }))
}

fn value<T: Serialize>(value: T) -> Result<Value, ExtensionContextError> {
    serde_json::to_value(value).map_err(|error| ExtensionContextError::Failed(error.to_string()))
}

fn unix_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

fn extension_user_message(content: ExtensionUserMessageContent) -> (String, Vec<ContentBlock>) {
    match content {
        ExtensionUserMessageContent::Text(text) => {
            let blocks = vec![ContentBlock::Text(pi_core::TextContent::new(&text))];
            (text, blocks)
        }
        ExtensionUserMessageContent::Blocks(blocks) => {
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
struct ExtensionSessionTreeNode {
    entry: SessionRecord,
    children: Vec<ExtensionSessionTreeNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn session_tree(document: &SessionDocument) -> Vec<ExtensionSessionTreeNode> {
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
    ) -> ExtensionSessionTreeNode {
        let descendants = children
            .remove(&Some(entry.id.clone()))
            .unwrap_or_default()
            .into_iter()
            .map(|child| build(child, children, labels))
            .collect();
        let label = labels.get(&entry.id).cloned();
        ExtensionSessionTreeNode {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticAccess;

    #[async_trait]
    impl ExtensionContextAccess for StaticAccess {
        fn query(
            &self,
            _scope: ExtensionContextScope,
            _query: ExtensionContextQuery,
        ) -> Result<Value, ExtensionContextError> {
            Ok(json!("ok"))
        }

        fn notify(
            &self,
            _scope: ExtensionContextScope,
            _notification: ExtensionContextNotification,
        ) -> Result<(), ExtensionContextError> {
            Ok(())
        }

        async fn request(
            &self,
            _scope: ExtensionContextScope,
            _request: ExtensionContextRequest,
        ) -> Result<Value, ExtensionContextError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn retired_epoch_rejects_stored_handles() {
        let epoch = ExtensionContextEpoch::new(Arc::new(StaticAccess));
        let handle = epoch.handle(ExtensionContextScope::Base);
        assert_eq!(
            handle.query(ExtensionContextQuery::Cwd).unwrap(),
            json!("ok")
        );

        epoch.retire();
        assert!(matches!(
            handle.query(ExtensionContextQuery::Cwd),
            Err(ExtensionContextError::Retired)
        ));
    }

    #[test]
    fn context_operations_use_camel_case_tags() {
        assert_eq!(
            serde_json::to_value(ExtensionContextQuery::SessionEntry {
                id: "entry-1".to_string(),
            })
            .unwrap(),
            json!({ "type": "sessionEntry", "id": "entry-1" })
        );
        assert_eq!(
            serde_json::to_value(ExtensionContextRequest::SwitchSession {
                session_path: PathBuf::from("session.jsonl"),
            })
            .unwrap(),
            json!({ "type": "switchSession", "sessionPath": "session.jsonl" })
        );
        assert_eq!(
            serde_json::to_value(ExtensionContextNotification::UiNotify {
                message: "Extension notice".to_string(),
                level: ExtensionNoticeLevel::Warning,
            })
            .unwrap(),
            json!({
                "type": "uiNotify",
                "message": "Extension notice",
                "level": "warning",
            })
        );
    }
}
