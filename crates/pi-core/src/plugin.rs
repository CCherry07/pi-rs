use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
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

#[derive(Clone)]
pub struct PluginContext {
    pub plugin_id: PluginId,
    pub run_id: RunId,
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
}

#[derive(Clone)]
pub struct InputContext {
    pub plugin_id: PluginId,
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEvent {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPatch {
    Continue,
    Transform(String),
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnStartEvent;

#[derive(Debug, Clone, PartialEq)]
pub struct TurnEndEvent {
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
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResultPatch {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<Value>,
    pub usage: Option<Usage>,
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
    ) -> std::result::Result<(), PluginError> {
        Ok(())
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

pub struct PluginDriver {
    plugins: Vec<RegisteredPlugin>,
}

fn plugin_context(
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
    }
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
        Ok(Self {
            plugins: registered,
        })
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.plugins
            .iter()
            .map(|plugin| plugin.id.clone())
            .collect()
    }

    /// Chains input transformations in plugin registration order. A handled
    /// result short-circuits the remaining plugins.
    pub async fn input(
        &self,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut event: InputEvent,
    ) -> std::result::Result<InputPatch, PluginError> {
        let original = event.text.clone();
        for registered in &self.plugins {
            let patch = registered
                .plugin
                .input(
                    InputContext {
                        plugin_id: registered.id.clone(),
                        cwd: cwd.to_path_buf(),
                        abort_signal: signal.clone(),
                    },
                    event.clone(),
                )
                .await
                .map_err(|error| PluginError::Hook {
                    plugin_id: registered.id.clone(),
                    hook: "input",
                    message: error.to_string(),
                })?;
            match patch {
                InputPatch::Continue => {}
                InputPatch::Transform(text) => event.text = text,
                InputPatch::Handled => return Ok(InputPatch::Handled),
            }
        }
        if event.text == original {
            Ok(InputPatch::Continue)
        } else {
            Ok(InputPatch::Transform(event.text))
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
            let context = PluginContext {
                plugin_id: registered.id.clone(),
                run_id: run_id.clone(),
                cwd: cwd.to_path_buf(),
                abort_signal: signal.clone(),
            };
            let patch = registered
                .plugin
                .before_agent_start(context, event.clone())
                .await
                .map_err(|error| PluginError::Hook {
                    plugin_id: registered.id.clone(),
                    hook: "before_agent_start",
                    message: error.to_string(),
                })?;
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
            let _ = registered
                .plugin
                .agent_start(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .agent_end(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .turn_start(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .turn_end(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .message_start(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .message_update(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
        }
    }

    pub async fn message_end(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: MessageEndEvent,
    ) {
        for registered in &self.plugins {
            let _ = registered
                .plugin
                .message_end(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
        }
    }

    pub async fn tool_execution_start(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        event: ToolExecutionStartEvent,
    ) {
        for registered in &self.plugins {
            let _ = registered
                .plugin
                .tool_execution_start(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .tool_execution_update(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let _ = registered
                .plugin
                .tool_execution_end(
                    plugin_context(registered, run_id, cwd, signal),
                    event.clone(),
                )
                .await;
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
            let context = PluginContext {
                plugin_id: registered.id.clone(),
                run_id: run_id.clone(),
                cwd: cwd.to_path_buf(),
                abort_signal: signal.clone(),
            };
            let patch = registered
                .plugin
                .context(
                    context,
                    ContextEvent {
                        messages: messages.clone(),
                    },
                )
                .await
                .map_err(|error| PluginError::Hook {
                    plugin_id: registered.id.clone(),
                    hook: "context",
                    message: error.to_string(),
                })?;
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
            let context = PluginContext {
                plugin_id: registered.id.clone(),
                run_id: run_id.clone(),
                cwd: cwd.to_path_buf(),
                abort_signal: signal.clone(),
            };
            let patch = registered
                .plugin
                .tool_call(
                    context,
                    ToolCallEvent {
                        assistant_message: event.assistant_message.clone(),
                        tool_call: event.tool_call.clone(),
                        validated_args: arguments.clone(),
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
            let context = PluginContext {
                plugin_id: registered.id.clone(),
                run_id: run_id.clone(),
                cwd: cwd.to_path_buf(),
                abort_signal: signal.clone(),
            };
            let current_event = ToolResultEvent {
                assistant_message: event.assistant_message.clone(),
                tool_call: event.tool_call.clone(),
                validated_args: event.validated_args.clone(),
                result: result.clone(),
            };
            match registered.plugin.tool_result(context, current_event).await {
                Ok(patch) => patch.apply(&mut result),
                Err(error) => {
                    return ToolResult::error(format!(
                        "plugin {} failed in tool_result: {error}",
                        registered.id
                    ));
                }
            }
        }
        result
    }
}
