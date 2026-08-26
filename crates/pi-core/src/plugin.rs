use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AbortSignal, AssistantMessage, AssistantMessageEvent, ContentBlock, CoreError, Message,
    ModelId, PluginId, ProviderId, RegistriesBuilder, Result, RunId, ToolCall, ToolCallId,
    ToolResult, ToolResultMessage, Usage,
};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {plugin_id} failed in {hook}: {message}")]
    Hook {
        plugin_id: PluginId,
        hook: &'static str,
        message: String,
    },
    #[error("plugin registration failed: {0}")]
    Registration(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDiagnostic {
    pub plugin_id: PluginId,
    pub hook: String,
    pub message: String,
}

#[derive(Clone, Default)]
pub(crate) struct PluginDiagnosticSink {
    diagnostics: Arc<Mutex<Vec<PluginDiagnostic>>>,
}

impl PluginDiagnosticSink {
    pub(crate) fn record(
        &self,
        plugin_id: PluginId,
        hook: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PluginDiagnostic {
                plugin_id,
                hook: hook.into(),
                message: message.into(),
            });
    }

    pub(crate) fn snapshot(&self) -> Vec<PluginDiagnostic> {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn take(&self) -> Vec<PluginDiagnostic> {
        std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

#[derive(Clone)]
pub struct PluginContext {
    pub plugin_id: PluginId,
    pub run_id: RunId,
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
    diagnostics: PluginDiagnosticSink,
}

impl PluginContext {
    pub fn new(
        plugin_id: PluginId,
        run_id: RunId,
        cwd: PathBuf,
        abort_signal: AbortSignal,
    ) -> Self {
        Self {
            plugin_id,
            run_id,
            cwd,
            abort_signal,
            diagnostics: PluginDiagnosticSink::default(),
        }
    }

    pub fn report_hook_error(&self, hook: &'static str, message: impl Into<String>) {
        self.diagnostics
            .record(self.plugin_id.clone(), hook, message);
    }
}

#[derive(Clone)]
pub struct InputContext {
    pub plugin_id: PluginId,
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
    diagnostics: PluginDiagnosticSink,
}

impl InputContext {
    pub fn report_hook_error(&self, hook: &'static str, message: impl Into<String>) {
        self.diagnostics
            .record(self.plugin_id.clone(), hook, message);
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
    pub message: AssistantMessage,
    pub tool_results: Vec<ToolResultMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageStartEvent {
    pub message: Message,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageUpdateEvent {
    pub message: AssistantMessage,
    pub event: AssistantMessageEvent,
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

#[async_trait]
pub trait AgentPlugin: Send + Sync {
    fn id(&self) -> PluginId;

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
        _context: PluginContext,
        _event: BeforeAgentStartEvent,
    ) -> std::result::Result<BeforeAgentStartPatch, PluginError> {
        Ok(BeforeAgentStartPatch::default())
    }

    async fn agent_start(
        &self,
        _context: PluginContext,
        _event: AgentStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn agent_end(
        &self,
        _context: PluginContext,
        _event: AgentEndEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn agent_settled(
        &self,
        _context: PluginContext,
        _event: AgentSettledEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn turn_start(
        &self,
        _context: PluginContext,
        _event: TurnStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn turn_end(
        &self,
        _context: PluginContext,
        _event: TurnEndEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn message_start(
        &self,
        _context: PluginContext,
        _event: MessageStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn message_update(
        &self,
        _context: PluginContext,
        _event: MessageUpdateEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn message_end(
        &self,
        _context: PluginContext,
        _event: MessageEndEvent,
    ) -> std::result::Result<MessageEndPatch, PluginError> {
        Ok(MessageEndPatch::default())
    }
    async fn tool_execution_start(
        &self,
        _context: PluginContext,
        _event: ToolExecutionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn tool_execution_update(
        &self,
        _context: PluginContext,
        _event: ToolExecutionUpdateEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
    async fn tool_execution_end(
        &self,
        _context: PluginContext,
        _event: ToolExecutionEndEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }

    async fn context(
        &self,
        _context: PluginContext,
        _event: ContextEvent,
    ) -> std::result::Result<ContextPatch, PluginError> {
        Ok(ContextPatch::default())
    }

    async fn tool_call(
        &self,
        _context: PluginContext,
        _event: ToolCallEvent,
    ) -> std::result::Result<ToolCallPatch, PluginError> {
        Ok(ToolCallPatch::default())
    }

    async fn tool_result(
        &self,
        _context: PluginContext,
        _event: ToolResultEvent,
    ) -> std::result::Result<ToolResultPatch, PluginError> {
        Ok(ToolResultPatch::default())
    }
}

struct RegisteredPlugin {
    id: PluginId,
    plugin: Arc<dyn AgentPlugin>,
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
    plugins: Vec<RegisteredPlugin>,
    diagnostics: PluginDiagnosticSink,
}

impl PluginDriver {
    pub fn new(plugins: Vec<Arc<dyn AgentPlugin>>) -> Result<Self> {
        let mut seen = std::collections::HashSet::new();
        let mut registered = Vec::with_capacity(plugins.len());
        for plugin in plugins {
            let id = plugin.id();
            if !seen.insert(id.clone()) {
                return Err(CoreError::DuplicatePlugin(id.to_string()));
            }
            registered.push(RegisteredPlugin { id, plugin });
        }
        let diagnostics = PluginDiagnosticSink::default();
        Ok(Self {
            plugins: registered,
            diagnostics,
        })
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

    fn plugin_context(
        &self,
        registered: &RegisteredPlugin,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
    ) -> PluginContext {
        PluginContext {
            plugin_id: registered.id.clone(),
            run_id: run_id.clone(),
            cwd: cwd.to_path_buf(),
            abort_signal: signal.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    fn record_error(
        &self,
        registered: &RegisteredPlugin,
        hook: &'static str,
        error: impl std::fmt::Display,
    ) {
        self.diagnostics
            .record(registered.id.clone(), hook, error.to_string());
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
        for registered in &self.plugins {
            let context = InputContext {
                plugin_id: registered.id.clone(),
                cwd: cwd.to_path_buf(),
                abort_signal: signal.clone(),
                diagnostics: self.diagnostics.clone(),
            };
            let patch = match registered.plugin.input(context, event.clone()).await {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, "input", error);
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
        for registered in &self.plugins {
            let patch = match registered
                .plugin
                .before_agent_start(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, "before_agent_start", error);
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

    pub async fn agent_start(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: AgentStartEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .agent_start(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "agent_start", error);
            }
        }
    }

    pub async fn agent_end(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: AgentEndEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .agent_end(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "agent_end", error);
            }
        }
    }

    pub async fn agent_settled(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: AgentSettledEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .agent_settled(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "agent_settled", error);
            }
        }
    }

    pub async fn turn_start(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: TurnStartEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .turn_start(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "turn_start", error);
            }
        }
    }

    pub async fn turn_end(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: TurnEndEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .turn_end(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "turn_end", error);
            }
        }
    }

    pub async fn message_start(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: MessageStartEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .message_start(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "message_start", error);
            }
        }
    }

    pub async fn message_update(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: MessageUpdateEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .message_update(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "message_update", error);
            }
        }
    }

    pub async fn message_end(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut event: MessageEndEvent,
    ) -> Message {
        for registered in &self.plugins {
            let patch = match registered
                .plugin
                .message_end(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, "message_end", error);
                    continue;
                }
            };
            if let Some(replacement) = patch.message {
                if same_message_role(&event.message, &replacement) {
                    event.message = replacement;
                } else {
                    self.diagnostics.record(
                        registered.id.clone(),
                        "message_end",
                        "message_end handlers must return a message with the same role",
                    );
                }
            }
        }
        event.message
    }

    pub async fn tool_execution_start(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: ToolExecutionStartEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .tool_execution_start(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "tool_execution_start", error);
            }
        }
    }

    pub async fn tool_execution_update(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: ToolExecutionUpdateEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .tool_execution_update(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "tool_execution_update", error);
            }
        }
    }

    pub async fn tool_execution_end(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: ToolExecutionEndEvent,
    ) {
        for registered in &self.plugins {
            if let Err(error) = registered
                .plugin
                .tool_execution_end(
                    self.plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await
            {
                self.record_error(registered, "tool_execution_end", error);
            }
        }
    }

    pub async fn context(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut messages: Vec<Message>,
    ) -> std::result::Result<Vec<Message>, PluginError> {
        for registered in &self.plugins {
            let patch = match registered
                .plugin
                .context(
                    self.plugin_context(registered, run_id, cwd, signal),
                    ContextEvent {
                        messages: messages.clone(),
                    },
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    self.record_error(registered, "context", error);
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
        for registered in &self.plugins {
            let patch = registered
                .plugin
                .tool_call(
                    self.plugin_context(registered, run_id, cwd, signal),
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
        for registered in &self.plugins {
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
                    self.plugin_context(registered, run_id, cwd, signal),
                    current_event,
                )
                .await
            {
                Ok(patch) => patch.apply(&mut result),
                Err(error) => self.record_error(registered, "tool_result", error),
            }
        }
        result
    }
}
