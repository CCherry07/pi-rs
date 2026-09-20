use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::diagnostics::{PluginDiagnostic, PluginDiagnosticSink, PluginError};
use super::session::*;

use super::capabilities::{
    CommandContextParts, ContextParts, ModelsContext, PluginContextEpoch, PluginContextHandle,
    SessionContext, UiContext,
};
use crate::{
    AbortSignal, AssistantMessage, AssistantStream, ContentBlock, CoreError, Message, ModelId,
    PluginId, ProviderId, RegistriesBuilder, Result, RunId, StreamEvent, ToolCall, ToolCallId,
    ToolResult, ToolResultMessage, Usage,
};

#[derive(Clone)]
pub struct AgentPluginContext {
    plugin_id: PluginId,
    run_id: RunId,
    workspace: crate::WorkspaceSnapshot,
    abort_signal: AbortSignal,
    pub session: SessionContext,
    pub models: ModelsContext,
    pub ui: UiContext,
    diagnostics: PluginDiagnosticSink,
}

impl AgentPluginContext {
    #[doc(hidden)]
    pub fn unavailable_for_testing(
        plugin_id: PluginId,
        run_id: RunId,
        cwd: PathBuf,
        abort_signal: AbortSignal,
    ) -> Self {
        let context = ContextParts::unavailable();
        Self {
            plugin_id,
            run_id,
            workspace: context.workspace_or_cwd(&cwd),
            abort_signal,
            session: context.session,
            models: context.models,
            ui: context.ui,
            diagnostics: PluginDiagnosticSink::default(),
        }
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn workspace(&self) -> &crate::WorkspaceSnapshot {
        &self.workspace
    }

    pub fn cwd(&self) -> &std::path::Path {
        self.workspace.cwd()
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.abort_signal
    }

    pub fn report_hook_error(&self, hook: &'static str, message: impl Into<String>) {
        self.diagnostics
            .record(self.plugin_id.clone(), hook, None, message);
    }

    #[doc(hidden)]
    pub fn plugin_context_handle(&self) -> PluginContextHandle {
        self.session.handle_for_adapter()
    }

    /// Rebinds adapter-owned metadata while retaining the generation-scoped
    /// capabilities and diagnostic sink.
    #[doc(hidden)]
    pub fn for_adapter_plugin(&self, plugin_id: PluginId) -> Self {
        let mut context = self.clone();
        context.plugin_id = plugin_id;
        context
    }
}

#[derive(Clone)]
pub struct InputContext {
    plugin_id: PluginId,
    workspace: crate::WorkspaceSnapshot,
    abort_signal: AbortSignal,
    pub session: SessionContext,
    pub models: ModelsContext,
    pub ui: UiContext,
    diagnostics: PluginDiagnosticSink,
}

impl InputContext {
    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn workspace(&self) -> &crate::WorkspaceSnapshot {
        &self.workspace
    }

    pub fn cwd(&self) -> &std::path::Path {
        self.workspace.cwd()
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.abort_signal
    }

    pub fn report_hook_error(&self, hook: &'static str, message: impl Into<String>) {
        self.diagnostics
            .record(self.plugin_id.clone(), hook, None, message);
    }

    #[doc(hidden)]
    pub fn plugin_context_handle(&self) -> PluginContextHandle {
        self.session.handle_for_adapter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputSource {
    Interactive,
    Rpc,
    Extension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputStreamingBehavior {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEvent {
    pub text: String,
    pub images: Option<Vec<crate::ImageContent>>,
    pub source: InputSource,
    pub streaming_behavior: Option<InputStreamingBehavior>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPatch {
    Continue,
    Transform {
        text: String,
        images: Option<Vec<crate::ImageContent>>,
    },
    Handled,
}

pub struct RegisterContext<'a> {
    owner: PluginId,
    registries: &'a mut RegistriesBuilder,
}

impl<'a> RegisterContext<'a> {
    fn new(owner: PluginId, registries: &'a mut RegistriesBuilder) -> Self {
        Self { owner, registries }
    }

    pub fn register_tool(&mut self, tool: Arc<dyn crate::Tool>) -> Result<()> {
        self.registries.register_tool(self.owner.clone(), tool)
    }

    pub fn register_command(&mut self, command: Arc<dyn crate::Command>) -> Result<()> {
        self.registries
            .register_command(self.owner.clone(), command)
    }
}

#[derive(Debug, Clone)]
pub struct BeforeAgentStartEvent {
    pub system_prompt: String,
    pub input_messages: Vec<Message>,
    pub active_tools: Vec<String>,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeAgentStartPatch {
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentStartEvent;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentEndEvent {
    pub messages: Vec<Message>,
}

/// Fired by product/session orchestration after the low-level Agent is idle
/// and no automatic retry, compaction, or queued continuation remains.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSettledEvent;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnStartEvent {
    pub turn_index: u64,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnEndEvent {
    pub turn_index: u64,
    pub message: Arc<AssistantMessage>,
    pub tool_results: Vec<ToolResultMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageStartEvent {
    pub message: Message,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageUpdateEvent {
    /// O(1) handle to the live cumulative partial. Materialize only when a
    /// hook genuinely needs the complete message.
    pub stream: AssistantStream,
    /// Shared current delta. Cloning the hook event never clones delta bytes.
    pub update: Arc<StreamEvent>,
}

impl MessageUpdateEvent {
    pub fn update(&self) -> &StreamEvent {
        self.update.as_ref()
    }

    pub fn snapshot(&self) -> Option<AssistantMessage> {
        self.stream.snapshot()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageEndEvent {
    pub message: Message,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageEndPatch {
    pub message: Option<Message>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionStartEvent {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionUpdateEvent {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub args: Value,
    pub partial_result: ToolResult,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionEndEvent {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub result: ToolResult,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct ContextEvent {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextPatch {
    pub messages: Option<Vec<Message>>,
}

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub validated_args: Value,
    pub context: Arc<crate::AgentContext>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCallPatch {
    pub arguments: Option<Value>,
    pub block: Option<ToolCallBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallBlock {
    pub reason: String,
    pub terminate: bool,
}

#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub validated_args: Value,
    pub result: ToolResult,
    pub context: Arc<crate::AgentContext>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResultPatch {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<Value>,
    pub usage: Option<Usage>,
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: Option<bool>,
    pub terminate: Option<bool>,
}

impl ToolResultPatch {
    pub fn apply(self, result: &mut ToolResult) {
        if let Some(content) = self.content {
            result.content = content;
        }
        if let Some(details) = self.details {
            result.details = Some(details);
        }
        if let Some(usage) = self.usage {
            result.usage = Some(usage);
        }
        if let Some(added_tool_names) = self.added_tool_names {
            result.added_tool_names = Some(added_tool_names);
        }
        if let Some(is_error) = self.is_error {
            result.is_error = is_error;
        }
        if let Some(terminate) = self.terminate {
            result.terminate = terminate;
        }
    }
}

define_hooks! {
    /// A `Plugin` callback that can be routed independently.
    AgentHook {
        Input => "input",
        BeforeAgentStart => "before_agent_start",
        AgentStart => "agent_start",
        AgentEnd => "agent_end",
        AgentSettled => "agent_settled",
        TurnStart => "turn_start",
        TurnEnd => "turn_end",
        MessageStart => "message_start",
        MessageUpdate => "message_update",
        MessageEnd => "message_end",
        ToolExecutionStart => "tool_execution_start",
        ToolExecutionUpdate => "tool_execution_update",
        ToolExecutionEnd => "tool_execution_end",
        Context => "context",
        ToolCall => "tool_call",
        ToolResult => "tool_result",
    }
}

const AGENT_HOOKS: [AgentHook; 16] = [
    AgentHook::Input,
    AgentHook::BeforeAgentStart,
    AgentHook::AgentStart,
    AgentHook::AgentEnd,
    AgentHook::AgentSettled,
    AgentHook::TurnStart,
    AgentHook::TurnEnd,
    AgentHook::MessageStart,
    AgentHook::MessageUpdate,
    AgentHook::MessageEnd,
    AgentHook::ToolExecutionStart,
    AgentHook::ToolExecutionUpdate,
    AgentHook::ToolExecutionEnd,
    AgentHook::Context,
    AgentHook::ToolCall,
    AgentHook::ToolResult,
];

impl AgentHook {
    const COUNT: usize = AGENT_HOOKS.len();

    const fn index(self) -> usize {
        self as usize
    }
}

/// The exact runtime callbacks implemented by an `Plugin`.
///
/// Plugin authors normally do not construct this value themselves. The
/// `#[pi_plugin::plugin]` and `#[pi_plugin::native_plugin]` attributes derive
/// it from the callback methods present on the trait implementation and also
/// expand async callbacks without a companion `#[async_trait]` attribute.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentHookInterests(u32);

impl AgentHookInterests {
    pub const fn from_hooks(hooks: &[AgentHook]) -> Self {
        let mut bits = 0_u32;
        let mut index = 0;
        while index < hooks.len() {
            bits |= 1_u32 << hooks[index].index();
            index += 1;
        }
        Self(bits)
    }

    pub const fn contains(self, hook: AgentHook) -> bool {
        self.0 & (1_u32 << hook.index()) != 0
    }
}

/// Implement with `#[pi_plugin::plugin]`; the attribute derives hook
/// interests and supplies the async-trait expansion.
#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> PluginId;

    /// Declares the Agent callbacks this plugin implements. Use `#[pi_plugin::plugin]`
    /// so this stays synchronized with the implementation.
    fn hook_interests(&self) -> AgentHookInterests;

    fn session_hook_interests(&self) -> SessionHookInterests;

    fn register(&self, _context: &mut RegisterContext<'_>) -> Result<()> {
        Ok(())
    }

    async fn input(
        &self,
        _context: InputContext,
        _event: InputEvent,
    ) -> std::result::Result<InputPatch, PluginError> {
        Ok(InputPatch::Continue)
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        _event: BeforeAgentStartEvent,
    ) -> std::result::Result<BeforeAgentStartPatch, PluginError> {
        Ok(BeforeAgentStartPatch::default())
    }

    async fn agent_start(
        &self,
        _context: AgentPluginContext,
        _event: AgentStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn agent_end(
        &self,
        _context: AgentPluginContext,
        _event: AgentEndEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn agent_settled(
        &self,
        _context: AgentPluginContext,
        _event: AgentSettledEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn turn_start(
        &self,
        _context: AgentPluginContext,
        _event: TurnStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn turn_end(
        &self,
        _context: AgentPluginContext,
        _event: TurnEndEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn message_start(
        &self,
        _context: AgentPluginContext,
        _event: MessageStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn message_update(
        &self,
        _context: AgentPluginContext,
        _event: MessageUpdateEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn message_end(
        &self,
        _context: AgentPluginContext,
        _event: MessageEndEvent,
    ) -> std::result::Result<MessageEndPatch, PluginError> {
        Ok(MessageEndPatch::default())
    }
    async fn tool_execution_start(
        &self,
        _context: AgentPluginContext,
        _event: ToolExecutionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn tool_execution_update(
        &self,
        _context: AgentPluginContext,
        _event: ToolExecutionUpdateEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn tool_execution_end(
        &self,
        _context: AgentPluginContext,
        _event: ToolExecutionEndEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn context(
        &self,
        _context: AgentPluginContext,
        _event: ContextEvent,
    ) -> std::result::Result<ContextPatch, PluginError> {
        Ok(ContextPatch::default())
    }

    async fn tool_call(
        &self,
        _context: AgentPluginContext,
        _event: ToolCallEvent,
    ) -> std::result::Result<ToolCallPatch, PluginError> {
        Ok(ToolCallPatch::default())
    }

    async fn tool_result(
        &self,
        _context: AgentPluginContext,
        _event: ToolResultEvent,
    ) -> std::result::Result<ToolResultPatch, PluginError> {
        Ok(ToolResultPatch::default())
    }
    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn session_info_changed(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionInfoChangedEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn session_before_switch(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeSwitchEvent,
    ) -> std::result::Result<Option<SessionBeforeSwitchResult>, PluginError> {
        Ok(None)
    }

    async fn session_before_fork(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeForkEvent,
    ) -> std::result::Result<Option<SessionBeforeForkResult>, PluginError> {
        Ok(None)
    }

    async fn session_before_compact(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeCompactEvent,
    ) -> std::result::Result<Option<SessionBeforeCompactResult>, PluginError> {
        Ok(None)
    }

    async fn session_compact(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionCompactEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn session_compact_failed(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionCompactFailedEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn session_shutdown(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn session_before_tree(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionBeforeTreeEvent,
    ) -> std::result::Result<Option<SessionBeforeTreeResult>, PluginError> {
        Ok(None)
    }

    async fn session_tree(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionTreeEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
}

pub(super) struct RegisteredPlugin {
    pub(super) id: PluginId,
    pub(super) plugin: Arc<dyn Plugin>,
}

struct AgentHookRoutes {
    plugin_indices: [Vec<usize>; AgentHook::COUNT],
}

impl Default for AgentHookRoutes {
    fn default() -> Self {
        Self {
            plugin_indices: std::array::from_fn(|_| Vec::new()),
        }
    }
}

impl AgentHookRoutes {
    fn add(&mut self, hook: AgentHook, plugin_index: usize) {
        self.plugin_indices[hook.index()].push(plugin_index);
    }

    fn get(&self, hook: AgentHook) -> &[usize] {
        &self.plugin_indices[hook.index()]
    }
}

fn same_message_role(left: &Message, right: &Message) -> bool {
    matches!(
        (left, right),
        (Message::User(_), Message::User(_))
            | (Message::Assistant(_), Message::Assistant(_))
            | (Message::ToolResult(_), Message::ToolResult(_))
            | (Message::Custom(_), Message::Custom(_))
    )
}

pub struct PluginDriver {
    pub(super) plugins: Vec<RegisteredPlugin>,
    routes: AgentHookRoutes,
    session_routes: [Vec<usize>; 10],
    pub(super) diagnostics: PluginDiagnosticSink,
    context_epoch: PluginContextEpoch,
}

macro_rules! agent_observers {
    ($($method:ident: $hook:ident => $event:ty),* $(,)?) => {
        $(pub async fn $method(
            &self,
            run_id: &RunId,
            cwd: &std::path::Path,
            signal: &AbortSignal,
            event: $event,
        ) {
            for registered in self.interested_plugins(AgentHook::$hook) {
                if let Err(error) = registered.plugin.$method(
                    self.agent_plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                ).await {
                    self.record_error(registered, AgentHook::$hook, error);
                }
            }
        })*
    };
}

impl PluginDriver {
    agent_observers! {
        agent_start: AgentStart => AgentStartEvent,
        agent_end: AgentEnd => AgentEndEvent,
        agent_settled: AgentSettled => AgentSettledEvent,
        turn_start: TurnStart => TurnStartEvent,
        turn_end: TurnEnd => TurnEndEvent,
        message_start: MessageStart => MessageStartEvent,
        message_update: MessageUpdate => MessageUpdateEvent,
        tool_execution_start: ToolExecutionStart => ToolExecutionStartEvent,
        tool_execution_update: ToolExecutionUpdate => ToolExecutionUpdateEvent,
        tool_execution_end: ToolExecutionEnd => ToolExecutionEndEvent,
    }

    pub fn new(plugins: Vec<Arc<dyn Plugin>>) -> Result<Self> {
        Self::new_with_context(plugins, PluginContextEpoch::unavailable())
    }

    pub fn new_with_context(
        plugins: Vec<Arc<dyn Plugin>>,
        context_epoch: PluginContextEpoch,
    ) -> Result<Self> {
        let mut seen = std::collections::HashSet::new();
        let mut registered = Vec::with_capacity(plugins.len());
        let mut routes = AgentHookRoutes::default();
        let mut session_routes: [Vec<usize>; 10] = std::array::from_fn(|_| Vec::new());
        for plugin in plugins {
            let id = plugin.id();
            if !seen.insert(id.clone()) {
                return Err(CoreError::DuplicatePlugin(id.to_string()));
            }
            let interests = plugin.hook_interests();
            let plugin_index = registered.len();
            for hook in AGENT_HOOKS {
                if interests.contains(hook) {
                    routes.add(hook, plugin_index);
                }
            }
            for hook in SESSION_HOOKS {
                if plugin.session_hook_interests().contains(hook) {
                    session_routes[hook as usize].push(plugin_index);
                }
            }
            registered.push(RegisteredPlugin { id, plugin });
        }
        let diagnostics = PluginDiagnosticSink::default();
        Ok(Self {
            plugins: registered,
            routes,
            session_routes,
            diagnostics,
            context_epoch,
        })
    }

    pub fn context_parts(&self) -> ContextParts {
        self.context_epoch.context()
    }

    pub fn command_context_parts(&self) -> CommandContextParts {
        self.context_epoch.command_context()
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.plugins
            .iter()
            .map(|plugin| plugin.id.clone())
            .collect()
    }

    pub fn diagnostics(&self) -> Vec<PluginDiagnostic> {
        self.diagnostics.snapshot()
    }

    pub fn take_diagnostics(&self) -> Vec<PluginDiagnostic> {
        self.diagnostics.take()
    }

    pub(super) fn session_plugins(
        &self,
        hook: SessionHook,
    ) -> impl Iterator<Item = &RegisteredPlugin> {
        self.session_routes[hook as usize]
            .iter()
            .map(|&index| &self.plugins[index])
    }

    fn interested_plugins(&self, hook: AgentHook) -> impl Iterator<Item = &RegisteredPlugin> {
        self.routes
            .get(hook)
            .iter()
            .map(|&index| &self.plugins[index])
    }

    fn agent_plugin_context(
        &self,
        registered: &RegisteredPlugin,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
    ) -> AgentPluginContext {
        let context = self.context_parts();
        AgentPluginContext {
            plugin_id: registered.id.clone(),
            run_id: run_id.clone(),
            workspace: context.workspace_or_cwd(cwd),
            abort_signal: signal.clone(),
            session: context.session,
            models: context.models,
            ui: context.ui,
            diagnostics: self.diagnostics.clone(),
        }
    }

    fn record_error(
        &self,
        registered: &RegisteredPlugin,
        hook: AgentHook,
        error: impl std::fmt::Display,
    ) {
        self.diagnostics
            .record(registered.id.clone(), hook, None, error.to_string());
    }

    /// Chains input transformations in plugin registration order. A handled
    /// result short-circuits the remaining plugins.
    pub async fn input(
        &self,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut event: InputEvent,
    ) -> std::result::Result<InputPatch, PluginError> {
        let original = event.clone();
        for registered in self.interested_plugins(AgentHook::Input) {
            let context = self.context_parts();
            let context = InputContext {
                plugin_id: registered.id.clone(),
                workspace: context.workspace_or_cwd(cwd),
                abort_signal: signal.clone(),
                session: context.session,
                models: context.models,
                ui: context.ui,
                diagnostics: self.diagnostics.clone(),
            };
            let patch = match registered.plugin.input(context, event.clone()).await {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, AgentHook::Input, error);
                    continue;
                }
            };
            match patch {
                InputPatch::Continue => {}
                InputPatch::Transform { text, images } => {
                    event.text = text;
                    if images.is_some() {
                        event.images = images;
                    }
                }
                InputPatch::Handled => return Ok(InputPatch::Handled),
            }
        }
        if event == original {
            Ok(InputPatch::Continue)
        } else {
            Ok(InputPatch::Transform {
                text: event.text,
                images: event.images,
            })
        }
    }

    pub fn register_all(&self, registries: &mut RegistriesBuilder) -> Result<()> {
        for registered in &self.plugins {
            let mut context = RegisterContext::new(registered.id.clone(), registries);
            registered.plugin.register(&mut context)?;
        }
        Ok(())
    }

    pub async fn before_agent_start(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut event: BeforeAgentStartEvent,
    ) -> std::result::Result<BeforeAgentStartPatch, PluginError> {
        let mut messages = Vec::new();
        for registered in self.interested_plugins(AgentHook::BeforeAgentStart) {
            let patch = match registered
                .plugin
                .before_agent_start(
                    self.agent_plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, AgentHook::BeforeAgentStart, error);
                    continue;
                }
            };
            if let Some(system_prompt) = patch.system_prompt {
                event.system_prompt = system_prompt;
            }
            messages.extend(patch.messages);
        }
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(event.system_prompt),
            messages,
        })
    }

    pub async fn message_end(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut event: MessageEndEvent,
    ) -> Message {
        for registered in self.interested_plugins(AgentHook::MessageEnd) {
            let patch = match registered
                .plugin
                .message_end(
                    self.agent_plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, AgentHook::MessageEnd, error);
                    continue;
                }
            };
            if let Some(replacement) = patch.message {
                if same_message_role(&event.message, &replacement) {
                    event.message = replacement;
                } else {
                    self.diagnostics.record(
                        registered.id.clone(),
                        AgentHook::MessageEnd,
                        None,
                        "message_end handlers must return a message with the same role",
                    );
                }
            }
        }
        event.message
    }

    pub async fn context(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut messages: Vec<Message>,
    ) -> std::result::Result<Vec<Message>, PluginError> {
        for registered in self.interested_plugins(AgentHook::Context) {
            let patch = match registered
                .plugin
                .context(
                    self.agent_plugin_context(registered, run_id, cwd, signal),
                    ContextEvent {
                        messages: messages.clone(),
                    },
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, AgentHook::Context, error);
                    continue;
                }
            };
            if let Some(replacement) = patch.messages {
                messages = replacement;
            }
        }
        Ok(messages)
    }

    pub async fn tool_call(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: ToolCallEvent,
    ) -> std::result::Result<ToolCallPatch, PluginError> {
        let mut arguments = event.validated_args;
        for registered in self.interested_plugins(AgentHook::ToolCall) {
            let patch = registered
                .plugin
                .tool_call(
                    self.agent_plugin_context(registered, run_id, cwd, signal),
                    ToolCallEvent {
                        assistant_message: event.assistant_message.clone(),
                        tool_call: event.tool_call.clone(),
                        validated_args: arguments.clone(),
                        context: Arc::clone(&event.context),
                    },
                )
                .await
                .map_err(|error| PluginError::Hook {
                    plugin_id: registered.id.clone(),
                    hook: "tool_call",
                    message: error.to_string(),
                })?;
            if let Some(replacement) = patch.arguments {
                arguments = replacement;
            }
            if let Some(block) = patch.block {
                return Ok(ToolCallPatch {
                    arguments: Some(arguments),
                    block: Some(block),
                });
            }
        }
        Ok(ToolCallPatch {
            arguments: Some(arguments),
            block: None,
        })
    }

    pub async fn tool_result(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: ToolResultEvent,
    ) -> ToolResult {
        let mut result = event.result;
        for registered in self.interested_plugins(AgentHook::ToolResult) {
            let current_event = ToolResultEvent {
                assistant_message: event.assistant_message.clone(),
                tool_call: event.tool_call.clone(),
                validated_args: event.validated_args.clone(),
                result: result.clone(),
                context: Arc::clone(&event.context),
            };
            match registered
                .plugin
                .tool_result(
                    self.agent_plugin_context(registered, run_id, cwd, signal),
                    current_event,
                )
                .await
            {
                Ok(patch) => patch.apply(&mut result),
                Err(error) => self.record_error(registered, AgentHook::ToolResult, error),
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{AbortHandle, AssistantStreamId, AssistantStreamView};

    struct RegistrationOnlyPlugin {
        registrations: Arc<AtomicUsize>,
    }

    #[pi_plugin::plugin]
    impl Plugin for RegistrationOnlyPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("registration-only")
        }

        fn register(&self, _context: &mut RegisterContext<'_>) -> Result<()> {
            self.registrations.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct InputPlugin {
        id: &'static str,
        suffix: &'static str,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    struct MessageUpdateProbe {
        id: &'static str,
        stream_ids: Arc<Mutex<Vec<String>>>,
    }

    struct StaticAssistantStream(AssistantMessage);

    impl AssistantStreamView for StaticAssistantStream {
        fn snapshot(&self) -> Option<AssistantMessage> {
            Some(self.0.clone())
        }
    }

    #[pi_plugin::plugin]
    impl Plugin for MessageUpdateProbe {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn message_update(
            &self,
            _context: AgentPluginContext,
            event: MessageUpdateEvent,
        ) -> std::result::Result<(), PluginError> {
            self.stream_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event.stream.id().to_string());
            Ok(())
        }
    }

    #[pi_plugin::plugin]
    impl Plugin for InputPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn input(
            &self,
            _context: InputContext,
            event: InputEvent,
        ) -> std::result::Result<InputPatch, PluginError> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(self.id);
            Ok(InputPatch::Transform {
                text: format!("{}{}", event.text, self.suffix),
                images: event.images,
            })
        }
    }

    #[tokio::test]
    async fn derives_hook_routes_without_skipping_registration_or_ordered_chaining() {
        let registrations = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(RegistrationOnlyPlugin {
                registrations: Arc::clone(&registrations),
            }),
            Arc::new(InputPlugin {
                id: "first-input",
                suffix: "-first",
                calls: Arc::clone(&calls),
            }),
            Arc::new(InputPlugin {
                id: "second-input",
                suffix: "-second",
                calls: Arc::clone(&calls),
            }),
        ];
        let driver = PluginDriver::new(plugins).unwrap();

        assert_eq!(driver.routes.get(AgentHook::Input), &[1, 2]);
        assert!(driver.routes.get(AgentHook::Context).is_empty());

        driver.register_all(&mut RegistriesBuilder::new()).unwrap();
        assert_eq!(registrations.load(Ordering::Relaxed), 1);

        let (_abort, signal) = AbortHandle::new();
        let patch = driver
            .input(
                std::path::Path::new("."),
                &signal,
                InputEvent {
                    text: "seed".to_string(),
                    images: None,
                    source: InputSource::Interactive,
                    streaming_behavior: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            patch,
            InputPatch::Transform {
                text: "seed-first-second".to_string(),
                images: None,
            }
        );
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["first-input", "second-input"]
        );
    }

    #[tokio::test]
    async fn message_update_hooks_share_one_live_stream_handle() {
        let stream_ids = Arc::new(Mutex::new(Vec::new()));
        let driver = PluginDriver::new(vec![
            Arc::new(MessageUpdateProbe {
                id: "first-update-probe",
                stream_ids: Arc::clone(&stream_ids),
            }),
            Arc::new(MessageUpdateProbe {
                id: "second-update-probe",
                stream_ids: Arc::clone(&stream_ids),
            }),
        ])
        .unwrap();
        let message = Arc::new(AssistantMessage {
            content: Vec::new(),
            api: "test".to_string(),
            provider: ProviderId::new("scripted"),
            model: ModelId::new("test"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: crate::StopReason::Pending,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 0,
        });
        let stream = AssistantStream::new(
            AssistantStreamId::new("stream-1"),
            Arc::new(StaticAssistantStream((*message).clone())),
        );
        let (_abort, signal) = AbortHandle::new();

        driver
            .message_update(
                &RunId::next(),
                std::path::Path::new("."),
                &signal,
                MessageUpdateEvent {
                    stream,
                    update: Arc::new(StreamEvent::TextDelta {
                        content_index: 0,
                        delta: "x".to_string(),
                    }),
                },
            )
            .await;

        assert_eq!(
            *stream_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["stream-1", "stream-1"]
        );
    }

    #[test]
    fn hook_names_do_not_have_a_wildcard_fallback() {
        for hook in AGENT_HOOKS {
            let name = match hook {
                AgentHook::Input => "input",
                AgentHook::BeforeAgentStart => "before_agent_start",
                AgentHook::AgentStart => "agent_start",
                AgentHook::AgentEnd => "agent_end",
                AgentHook::AgentSettled => "agent_settled",
                AgentHook::TurnStart => "turn_start",
                AgentHook::TurnEnd => "turn_end",
                AgentHook::MessageStart => "message_start",
                AgentHook::MessageUpdate => "message_update",
                AgentHook::MessageEnd => "message_end",
                AgentHook::ToolExecutionStart => "tool_execution_start",
                AgentHook::ToolExecutionUpdate => "tool_execution_update",
                AgentHook::ToolExecutionEnd => "tool_execution_end",
                AgentHook::Context => "context",
                AgentHook::ToolCall => "tool_call",
                AgentHook::ToolResult => "tool_result",
            };
            assert_eq!(AgentHook::from_name(name), Some(hook));
        }
        assert_eq!(AgentHook::from_name("all"), None);
        assert_eq!(AgentHook::from_name("unknown"), None);
    }
}
