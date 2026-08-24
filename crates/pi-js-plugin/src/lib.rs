use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pi_core::{
    AbortSignal, AgentEndEvent, AgentPlugin, AgentStartEvent, BeforeAgentStartEvent,
    BeforeAgentStartPatch, BeforeProviderRequestEvent, Command, CommandContext, CommandError,
    CommandOutcome, CommandSpec, ContentBlock, ContextEvent, ContextPatch, InputContext,
    InputEvent, InputPatch, Message, MessageEndEvent, MessageStartEvent, MessageUpdateEvent,
    PluginContext, PluginError, PluginId, ProviderPlugin, ProviderPluginContext, RegisterContext,
    Tool, ToolCallEvent, ToolCallId, ToolCallPatch, ToolContext, ToolError, ToolExecutionEndEvent,
    ToolExecutionMode, ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResult,
    ToolResultEvent, ToolResultPatch, ToolSpec, ToolUpdateSink, TurnEndEvent, TurnStartEvent,
    Usage,
};
use pi_session::{
    CompactionEntry, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCompactEvent,
    SessionCompactFailedEvent, SessionForkPosition, SessionInfoChangedEvent, SessionPlugin,
    SessionPluginContext, SessionPluginError, SessionShutdownEvent, SessionShutdownReason,
    SessionStartEvent, SessionStartReason, SessionSwitchReason, SessionTreeEvent,
    SessionTreeSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsGenerationManifest {
    pub generation_id: String,
    #[serde(default)]
    pub agent_plugins: Vec<JsAgentPluginManifest>,
    #[serde(default)]
    pub provider_plugins: Vec<JsProviderPluginManifest>,
    #[serde(default)]
    pub session_plugins: Vec<JsSessionPluginManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsGenerationRequest {
    pub project_trusted: bool,
    #[serde(default)]
    pub extension_paths: Vec<String>,
    pub mode: JsHostMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JsHostMode {
    Tui,
    Print,
    Json,
    Rpc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum JsHostOperation {
    PrepareGeneration { request: JsGenerationRequest },
    Invoke { invocation: JsInvocation },
    Cancel { invocation_id: String },
    RetireGeneration { generation_id: String },
}

#[cfg(test)]
mod wire_tests {
    use super::{JsGenerationRequest, JsHostMode, JsHostOperation};

    #[test]
    fn host_notification_fields_use_camel_case() {
        let cancel = serde_json::to_value(JsHostOperation::Cancel {
            invocation_id: "invocation-1".to_string(),
        })
        .unwrap();
        assert_eq!(cancel["invocationId"], "invocation-1");
        assert!(cancel.get("invocation_id").is_none());

        let retire = serde_json::to_value(JsHostOperation::RetireGeneration {
            generation_id: "generation-1".to_string(),
        })
        .unwrap();
        assert_eq!(retire["generationId"], "generation-1");
        assert!(retire.get("generation_id").is_none());
    }

    #[test]
    fn generation_request_contains_only_resolved_extension_paths() {
        let prepare = serde_json::to_value(JsHostOperation::PrepareGeneration {
            request: JsGenerationRequest {
                project_trusted: true,
                extension_paths: vec!["/extensions/example.ts".to_string()],
                mode: JsHostMode::Print,
            },
        })
        .unwrap();
        let request = &prepare["request"];

        assert_eq!(request["extensionPaths"][0], "/extensions/example.ts");
        assert_eq!(request["projectTrusted"], true);
        assert!(request.get("cwd").is_none());
        assert!(request.get("agentDir").is_none());
        assert!(request.get("explicitPaths").is_none());
        assert!(request.get("discoverExtensions").is_none());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsAgentPluginManifest {
    pub id: String,
    #[serde(default)]
    pub tools: Vec<JsToolManifest>,
    #[serde(default)]
    pub commands: Vec<JsCommandManifest>,
    #[serde(default)]
    pub hooks: Vec<JsHookManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsProviderPluginManifest {
    pub id: String,
    #[serde(default)]
    pub hooks: Vec<JsHookManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsSessionPluginManifest {
    pub id: String,
    #[serde(default)]
    pub hooks: Vec<JsHookManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsHookManifest {
    pub name: String,
    pub callback_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsCommandManifest {
    pub callback_id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub argument_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsToolManifest {
    pub callback_id: String,
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default)]
    pub prompt_snippet: Option<String>,
    #[serde(default)]
    pub prompt_guidelines: Vec<String>,
    #[serde(default)]
    pub execution_mode: JsToolExecutionMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JsToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsInvocation {
    pub invocation_id: String,
    pub generation_id: String,
    pub callback_id: String,
    pub kind: JsInvocationKind,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JsInvocationKind {
    Tool,
    Command,
    AgentHook,
    ProviderHook,
    SessionHook,
}

#[derive(Debug, thiserror::Error)]
#[error("JavaScript callback failed: {message}")]
pub struct JsCallbackError {
    pub message: String,
}

impl JsCallbackError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait JsCallbackDispatcher: Send + Sync {
    async fn invoke(&self, invocation: JsInvocation) -> Result<Value, JsCallbackError>;

    fn cancel(&self, _invocation_id: &str) {}

    fn retire_generation(&self, _generation_id: &str) {}
}

#[async_trait]
pub trait JsPluginHost: JsCallbackDispatcher {
    async fn prepare_generation(
        &self,
        request: JsGenerationRequest,
    ) -> Result<JsGenerationManifest, JsCallbackError>;
}

#[derive(Debug, thiserror::Error)]
pub enum JsPluginError {
    #[error("invalid JavaScript plugin manifest: {0}")]
    InvalidManifest(String),
}

pub struct JsPluginGeneration {
    agent_plugins: Vec<Arc<dyn AgentPlugin>>,
    provider_plugins: Vec<Arc<dyn ProviderPlugin>>,
    session_plugins: Vec<Arc<dyn SessionPlugin>>,
}

impl JsPluginGeneration {
    pub fn prepare_with_host(
        manifest: JsGenerationManifest,
        host: Arc<dyn JsPluginHost>,
    ) -> Result<Self, JsPluginError> {
        Self::prepare(manifest, Arc::new(HostCallbackDispatcher { host }))
    }

    pub fn prepare(
        manifest: JsGenerationManifest,
        dispatcher: Arc<dyn JsCallbackDispatcher>,
    ) -> Result<Self, JsPluginError> {
        let JsGenerationManifest {
            generation_id,
            agent_plugins,
            provider_plugins,
            session_plugins,
        } = manifest;
        let lease = Arc::new(JsGenerationLease {
            generation_id,
            dispatcher,
        });
        validate_manifest(
            &lease.generation_id,
            &agent_plugins,
            &provider_plugins,
            &session_plugins,
        )?;
        let agent_plugins = agent_plugins
            .into_iter()
            .map(|plugin| {
                let tools = plugin
                    .tools
                    .into_iter()
                    .map(|tool| Arc::new(JsTool::new(tool, Arc::clone(&lease))) as Arc<dyn Tool>)
                    .collect();
                let commands = plugin
                    .commands
                    .into_iter()
                    .map(|command| {
                        Arc::new(JsCommand::new(command, Arc::clone(&lease))) as Arc<dyn Command>
                    })
                    .collect();
                Arc::new(JsAgentPlugin {
                    id: PluginId::new(plugin.id),
                    tools,
                    commands,
                    hooks: plugin.hooks,
                    lease: Arc::clone(&lease),
                }) as Arc<dyn AgentPlugin>
            })
            .collect();
        let provider_plugins = provider_plugins
            .into_iter()
            .map(|plugin| {
                Arc::new(JsProviderPlugin {
                    id: PluginId::new(plugin.id),
                    hooks: plugin.hooks,
                    lease: Arc::clone(&lease),
                }) as Arc<dyn ProviderPlugin>
            })
            .collect();
        let session_plugins = session_plugins
            .into_iter()
            .map(|plugin| {
                Arc::new(JsSessionPlugin {
                    id: PluginId::new(plugin.id),
                    hooks: plugin.hooks,
                    lease: Arc::clone(&lease),
                }) as Arc<dyn SessionPlugin>
            })
            .collect();

        Ok(Self {
            agent_plugins,
            provider_plugins,
            session_plugins,
        })
    }

    pub fn agent_plugins(&self) -> Vec<Arc<dyn AgentPlugin>> {
        self.agent_plugins.clone()
    }

    pub fn provider_plugins(&self) -> Vec<Arc<dyn ProviderPlugin>> {
        self.provider_plugins.clone()
    }

    pub fn session_plugins(&self) -> Vec<Arc<dyn SessionPlugin>> {
        self.session_plugins.clone()
    }
}

struct HostCallbackDispatcher {
    host: Arc<dyn JsPluginHost>,
}

#[async_trait]
impl JsCallbackDispatcher for HostCallbackDispatcher {
    async fn invoke(&self, invocation: JsInvocation) -> Result<Value, JsCallbackError> {
        self.host.invoke(invocation).await
    }

    fn cancel(&self, invocation_id: &str) {
        self.host.cancel(invocation_id);
    }

    fn retire_generation(&self, generation_id: &str) {
        self.host.retire_generation(generation_id);
    }
}

const AGENT_HOOKS: &[&str] = &[
    "input",
    "before_agent_start",
    "agent_start",
    "agent_end",
    "turn_start",
    "turn_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "context",
    "tool_call",
    "tool_result",
];
const PROVIDER_HOOKS: &[&str] = &["before_provider_request"];
const SESSION_HOOKS: &[&str] = &[
    "session_start",
    "session_info_changed",
    "session_before_switch",
    "session_before_fork",
    "session_before_compact",
    "session_compact",
    "session_compact_failed",
    "session_shutdown",
    "session_before_tree",
    "session_tree",
];

fn validate_manifest(
    generation_id: &str,
    agent_plugins: &[JsAgentPluginManifest],
    provider_plugins: &[JsProviderPluginManifest],
    session_plugins: &[JsSessionPluginManifest],
) -> Result<(), JsPluginError> {
    if generation_id.trim().is_empty() {
        return Err(JsPluginError::InvalidManifest(
            "generationId must not be empty".to_string(),
        ));
    }
    let mut callback_ids = HashSet::new();
    let mut agent_ids = HashSet::new();
    for plugin in agent_plugins {
        if !agent_ids.insert(plugin.id.as_str()) {
            return Err(JsPluginError::InvalidManifest(format!(
                "duplicate agent plugin id: {}",
                plugin.id
            )));
        }
        validate_agent_plugin(plugin, &mut callback_ids)?;
    }
    let mut provider_ids = HashSet::new();
    for plugin in provider_plugins {
        if !provider_ids.insert(plugin.id.as_str()) {
            return Err(JsPluginError::InvalidManifest(format!(
                "duplicate provider plugin id: {}",
                plugin.id
            )));
        }
        validate_hooks(
            "provider",
            &plugin.id,
            &plugin.hooks,
            PROVIDER_HOOKS,
            &mut callback_ids,
        )?;
    }
    let mut session_ids = HashSet::new();
    for plugin in session_plugins {
        if !session_ids.insert(plugin.id.as_str()) {
            return Err(JsPluginError::InvalidManifest(format!(
                "duplicate session plugin id: {}",
                plugin.id
            )));
        }
        validate_hooks(
            "session",
            &plugin.id,
            &plugin.hooks,
            SESSION_HOOKS,
            &mut callback_ids,
        )?;
    }
    Ok(())
}

fn validate_agent_plugin(
    plugin: &JsAgentPluginManifest,
    callback_ids: &mut HashSet<String>,
) -> Result<(), JsPluginError> {
    if plugin.id.trim().is_empty() {
        return Err(JsPluginError::InvalidManifest(
            "agent plugin id must not be empty".to_string(),
        ));
    }
    for tool in &plugin.tools {
        if tool.callback_id.trim().is_empty() {
            return Err(JsPluginError::InvalidManifest(format!(
                "tool {} callbackId must not be empty",
                tool.name
            )));
        }
        if tool.name.trim().is_empty() {
            return Err(JsPluginError::InvalidManifest(format!(
                "agent plugin {} has a tool with an empty name",
                plugin.id
            )));
        }
        if !tool.parameters.is_object() {
            return Err(JsPluginError::InvalidManifest(format!(
                "tool {} parameters must be a JSON schema object",
                tool.name
            )));
        }
        validate_callback_id(&tool.callback_id, callback_ids)?;
    }
    for command in &plugin.commands {
        if command.callback_id.trim().is_empty() || command.name.trim().is_empty() {
            return Err(JsPluginError::InvalidManifest(format!(
                "agent plugin {} has an invalid command",
                plugin.id
            )));
        }
        validate_callback_id(&command.callback_id, callback_ids)?;
    }
    validate_hooks(
        "agent",
        &plugin.id,
        &plugin.hooks,
        AGENT_HOOKS,
        callback_ids,
    )?;
    Ok(())
}

fn validate_hooks(
    kind: &str,
    id: &str,
    hooks: &[JsHookManifest],
    supported: &[&str],
    callback_ids: &mut HashSet<String>,
) -> Result<(), JsPluginError> {
    if id.trim().is_empty() {
        return Err(JsPluginError::InvalidManifest(format!(
            "{kind} plugin id must not be empty"
        )));
    }
    if hooks
        .iter()
        .any(|hook| hook.name.trim().is_empty() || hook.callback_id.trim().is_empty())
    {
        return Err(JsPluginError::InvalidManifest(format!(
            "{kind} plugin {id} has an invalid hook"
        )));
    }
    for hook in hooks {
        if !supported.contains(&hook.name.as_str()) {
            return Err(JsPluginError::InvalidManifest(format!(
                "unsupported {kind} hook for plugin {id}: {}",
                hook.name
            )));
        }
        validate_callback_id(&hook.callback_id, callback_ids)?;
    }
    Ok(())
}

fn validate_callback_id(
    callback_id: &str,
    callback_ids: &mut HashSet<String>,
) -> Result<(), JsPluginError> {
    if !callback_ids.insert(callback_id.to_string()) {
        return Err(JsPluginError::InvalidManifest(format!(
            "duplicate callback id: {callback_id}"
        )));
    }
    Ok(())
}

struct JsGenerationLease {
    generation_id: String,
    dispatcher: Arc<dyn JsCallbackDispatcher>,
}

enum JsInvokeError {
    Aborted,
    Callback(JsCallbackError),
}

impl JsGenerationLease {
    async fn invoke(
        &self,
        callback_id: &str,
        kind: JsInvocationKind,
        payload: Value,
        abort_signal: Option<&AbortSignal>,
    ) -> Result<Value, JsInvokeError> {
        static NEXT_INVOCATION_ID: AtomicU64 = AtomicU64::new(1);

        let invocation_id = format!(
            "js-invocation-{}",
            NEXT_INVOCATION_ID.fetch_add(1, Ordering::Relaxed)
        );
        let invocation = JsInvocation {
            invocation_id: invocation_id.clone(),
            generation_id: self.generation_id.clone(),
            callback_id: callback_id.to_string(),
            kind,
            payload,
        };
        if let Some(signal) = abort_signal {
            tokio::select! {
                response = self.dispatcher.invoke(invocation) => {
                    response.map_err(JsInvokeError::Callback)
                }
                () = signal.wait() => {
                    self.dispatcher.cancel(&invocation_id);
                    Err(JsInvokeError::Aborted)
                }
            }
        } else {
            self.dispatcher
                .invoke(invocation)
                .await
                .map_err(JsInvokeError::Callback)
        }
    }
}

impl Drop for JsGenerationLease {
    fn drop(&mut self) {
        self.dispatcher.retire_generation(&self.generation_id);
    }
}

struct JsAgentPlugin {
    id: PluginId,
    tools: Vec<Arc<dyn Tool>>,
    commands: Vec<Arc<dyn Command>>,
    hooks: Vec<JsHookManifest>,
    lease: Arc<JsGenerationLease>,
}

impl JsAgentPlugin {
    fn hook_callbacks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.hooks
            .iter()
            .filter(move |hook| hook.name == name)
            .map(|hook| hook.callback_id.as_str())
    }

    async fn invoke_hook(
        &self,
        name: &'static str,
        callback_id: &str,
        context: Value,
        event: Value,
        signal: &AbortSignal,
    ) -> Result<Value, PluginError> {
        self.lease
            .invoke(
                callback_id,
                JsInvocationKind::AgentHook,
                json!({ "hook": name, "context": context, "event": event }),
                Some(signal),
            )
            .await
            .map_err(|error| PluginError::Hook {
                plugin_id: self.id.clone(),
                hook: name,
                message: match error {
                    JsInvokeError::Aborted => "JavaScript hook was aborted".to_string(),
                    JsInvokeError::Callback(error) => error.to_string(),
                },
            })
    }

    async fn notify_hooks(
        &self,
        name: &'static str,
        context: &PluginContext,
        event: Value,
    ) -> Result<(), PluginError> {
        let context_value = plugin_context_value(context);
        for callback_id in self.hook_callbacks(name) {
            self.invoke_hook(
                name,
                callback_id,
                context_value.clone(),
                event.clone(),
                &context.abort_signal,
            )
            .await?;
        }
        Ok(())
    }
}

fn plugin_context_value(context: &PluginContext) -> Value {
    json!({
        "pluginId": context.plugin_id.as_str(),
        "runId": context.run_id.as_str(),
        "cwd": context.cwd.to_string_lossy(),
    })
}

#[async_trait]
impl AgentPlugin for JsAgentPlugin {
    fn id(&self) -> PluginId {
        self.id.clone()
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for tool in &self.tools {
            context.register_tool(Arc::clone(tool))?;
        }
        for command in &self.commands {
            context.register_command(Arc::clone(command))?;
        }
        Ok(())
    }

    async fn input(
        &self,
        context: InputContext,
        event: InputEvent,
    ) -> Result<InputPatch, PluginError> {
        let original = event.text;
        let mut text = original.clone();
        let context_value = json!({
            "pluginId": context.plugin_id.as_str(),
            "cwd": context.cwd.to_string_lossy(),
        });
        for callback_id in self.hook_callbacks("input") {
            let result = self
                .invoke_hook(
                    "input",
                    callback_id,
                    context_value.clone(),
                    json!({ "type": "input", "text": text }),
                    &context.abort_signal,
                )
                .await?;
            match result.get("action").and_then(Value::as_str) {
                None | Some("continue") => {}
                Some("handled") => return Ok(InputPatch::Handled),
                Some("transform") => {
                    text = result
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| PluginError::Hook {
                            plugin_id: self.id.clone(),
                            hook: "input",
                            message: "transform result is missing text".to_string(),
                        })?
                        .to_string();
                }
                Some(action) => {
                    return Err(PluginError::Hook {
                        plugin_id: self.id.clone(),
                        hook: "input",
                        message: format!("invalid input action: {action}"),
                    });
                }
            }
        }
        Ok(if text == original {
            InputPatch::Continue
        } else {
            InputPatch::Transform(text)
        })
    }

    async fn before_agent_start(
        &self,
        context: PluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        let original_system_prompt = event.system_prompt;
        let mut system_prompt = original_system_prompt.clone();
        let context_value = plugin_context_value(&context);
        for callback_id in self.hook_callbacks("before_agent_start") {
            let result = self
                .invoke_hook(
                    "before_agent_start",
                    callback_id,
                    context_value.clone(),
                    json!({
                        "type": "before_agent_start",
                        "systemPrompt": system_prompt,
                        "inputMessages": event.input_messages,
                        "activeTools": event.active_tools,
                        "providerId": event.provider_id.as_str(),
                        "modelId": event.model_id.as_str(),
                    }),
                    &context.abort_signal,
                )
                .await?;
            if result
                .get("message")
                .is_some_and(|message| !message.is_null())
            {
                return Err(PluginError::Hook {
                    plugin_id: self.id.clone(),
                    hook: "before_agent_start",
                    message: "custom message injection is not supported by the Rust message model"
                        .to_string(),
                });
            }
            if let Some(replacement) = result.get("systemPrompt").and_then(Value::as_str) {
                system_prompt = replacement.to_string();
            }
        }
        Ok(BeforeAgentStartPatch {
            system_prompt: (system_prompt != original_system_prompt).then_some(system_prompt),
            messages: Vec::new(),
        })
    }

    async fn agent_start(
        &self,
        context: PluginContext,
        _event: AgentStartEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks("agent_start", &context, json!({ "type": "agent_start" }))
            .await
    }

    async fn agent_end(
        &self,
        context: PluginContext,
        event: AgentEndEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "agent_end",
            &context,
            json!({ "type": "agent_end", "messages": event.messages }),
        )
        .await
    }

    async fn turn_start(
        &self,
        context: PluginContext,
        _event: TurnStartEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks("turn_start", &context, json!({ "type": "turn_start" }))
            .await
    }

    async fn turn_end(
        &self,
        context: PluginContext,
        event: TurnEndEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "turn_end",
            &context,
            json!({
                "type": "turn_end",
                "message": event.message,
                "toolResults": event.tool_results,
            }),
        )
        .await
    }

    async fn message_start(
        &self,
        context: PluginContext,
        event: MessageStartEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "message_start",
            &context,
            json!({ "type": "message_start", "message": event.message }),
        )
        .await
    }

    async fn message_update(
        &self,
        context: PluginContext,
        event: MessageUpdateEvent,
    ) -> Result<(), PluginError> {
        let assistant_message_event = assistant_message_event_value(&event);
        self.notify_hooks(
            "message_update",
            &context,
            json!({
                "type": "message_update",
                "message": event.message,
                "assistantMessageEvent": assistant_message_event,
            }),
        )
        .await
    }

    async fn message_end(
        &self,
        context: PluginContext,
        event: MessageEndEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "message_end",
            &context,
            json!({ "type": "message_end", "message": event.message }),
        )
        .await
    }

    async fn tool_execution_start(
        &self,
        context: PluginContext,
        event: ToolExecutionStartEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "tool_execution_start",
            &context,
            json!({
                "type": "tool_execution_start",
                "toolCallId": event.tool_call_id.as_str(),
                "toolName": event.tool_name,
                "args": event.args,
            }),
        )
        .await
    }

    async fn tool_execution_update(
        &self,
        context: PluginContext,
        event: ToolExecutionUpdateEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "tool_execution_update",
            &context,
            json!({
                "type": "tool_execution_update",
                "toolCallId": event.tool_call_id.as_str(),
                "toolName": event.tool_name,
                "args": event.args,
                "partialResult": tool_result_value(&event.partial_result),
            }),
        )
        .await
    }

    async fn tool_execution_end(
        &self,
        context: PluginContext,
        event: ToolExecutionEndEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "tool_execution_end",
            &context,
            json!({
                "type": "tool_execution_end",
                "toolCallId": event.tool_call_id.as_str(),
                "toolName": event.tool_name,
                "result": tool_result_value(&event.result),
                "isError": event.is_error,
            }),
        )
        .await
    }

    async fn context(
        &self,
        context: PluginContext,
        event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        let original = event.messages;
        let mut messages = original.clone();
        let context_value = plugin_context_value(&context);
        for callback_id in self.hook_callbacks("context") {
            let result = self
                .invoke_hook(
                    "context",
                    callback_id,
                    context_value.clone(),
                    json!({ "type": "context", "messages": messages }),
                    &context.abort_signal,
                )
                .await?;
            if let Some(replacement) = result.get("messages") {
                messages = serde_json::from_value::<Vec<Message>>(replacement.clone()).map_err(
                    |error| PluginError::Hook {
                        plugin_id: self.id.clone(),
                        hook: "context",
                        message: format!("invalid messages result: {error}"),
                    },
                )?;
            }
        }
        Ok(ContextPatch {
            messages: (messages != original).then_some(messages),
        })
    }

    async fn tool_call(
        &self,
        context: PluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        let original_arguments = event.validated_args;
        let mut arguments = original_arguments.clone();
        let context_value = plugin_context_value(&context);
        for callback_id in self.hook_callbacks("tool_call") {
            let result = self
                .invoke_hook(
                    "tool_call",
                    callback_id,
                    context_value.clone(),
                    json!({
                        "type": "tool_call",
                        "toolCallId": event.tool_call.id.as_str(),
                        "toolName": event.tool_call.name,
                        "input": arguments,
                    }),
                    &context.abort_signal,
                )
                .await?;
            if let Some(input) = result.get("input") {
                arguments = input.clone();
            }
            if result.get("block").and_then(Value::as_bool) == Some(true) {
                return Ok(ToolCallPatch {
                    arguments: Some(arguments),
                    block: Some(pi_core::ToolCallBlock {
                        reason: result
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("blocked by JavaScript extension")
                            .to_string(),
                        terminate: result
                            .get("terminate")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }),
                });
            }
        }
        Ok(ToolCallPatch {
            arguments: (arguments != original_arguments).then_some(arguments),
            block: None,
        })
    }

    async fn tool_result(
        &self,
        context: PluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        let mut result = event.result;
        let mut aggregate = JsToolResultPatch::default();
        let context_value = plugin_context_value(&context);
        for callback_id in self.hook_callbacks("tool_result") {
            let patch = self
                .invoke_hook(
                    "tool_result",
                    callback_id,
                    context_value.clone(),
                    json!({
                        "type": "tool_result",
                        "toolCallId": event.tool_call.id.as_str(),
                        "toolName": event.tool_call.name,
                        "input": event.validated_args,
                        "content": result.content,
                        "details": result.details,
                        "usage": result.usage,
                        "isError": result.is_error,
                    }),
                    &context.abort_signal,
                )
                .await?;
            let parsed = serde_json::from_value::<JsToolResultPatch>(patch).map_err(|error| {
                PluginError::Hook {
                    plugin_id: self.id.clone(),
                    hook: "tool_result",
                    message: format!("invalid tool result patch: {error}"),
                }
            })?;
            parsed.clone().into_core().apply(&mut result);
            aggregate.merge(&parsed);
        }
        Ok(aggregate.into_core())
    }
}

fn assistant_message_event_value(event: &MessageUpdateEvent) -> Value {
    use pi_core::AssistantMessageEvent;

    let partial = &event.message;
    match &event.event {
        AssistantMessageEvent::Start => json!({ "type": "start", "partial": partial }),
        AssistantMessageEvent::TextStart { content_index } => json!({
            "type": "text_start",
            "contentIndex": content_index,
            "partial": partial,
        }),
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => json!({
            "type": "text_delta",
            "contentIndex": content_index,
            "delta": delta,
            "partial": partial,
        }),
        AssistantMessageEvent::TextEnd { content_index } => json!({
            "type": "text_end",
            "contentIndex": content_index,
            "content": content_block_text(partial.content.get(*content_index)),
            "partial": partial,
        }),
        AssistantMessageEvent::ThinkingStart { content_index } => json!({
            "type": "thinking_start",
            "contentIndex": content_index,
            "partial": partial,
        }),
        AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => json!({
            "type": "thinking_delta",
            "contentIndex": content_index,
            "delta": delta,
            "partial": partial,
        }),
        AssistantMessageEvent::ThinkingEnd { content_index } => json!({
            "type": "thinking_end",
            "contentIndex": content_index,
            "content": content_block_thinking(partial.content.get(*content_index)),
            "partial": partial,
        }),
        AssistantMessageEvent::ToolCallStart { content_index } => json!({
            "type": "toolcall_start",
            "contentIndex": content_index,
            "partial": partial,
        }),
        AssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => json!({
            "type": "toolcall_delta",
            "contentIndex": content_index,
            "delta": delta,
            "partial": partial,
        }),
        AssistantMessageEvent::ToolCallEnd { content_index } => json!({
            "type": "toolcall_end",
            "contentIndex": content_index,
            "toolCall": content_block_tool_call(partial.content.get(*content_index)),
            "partial": partial,
        }),
        AssistantMessageEvent::Done => {
            if matches!(
                partial.stop_reason,
                pi_core::StopReason::Error | pi_core::StopReason::Aborted
            ) {
                json!({
                    "type": "error",
                    "reason": partial.stop_reason,
                    "error": partial,
                })
            } else {
                json!({
                    "type": "done",
                    "reason": partial.stop_reason,
                    "message": partial,
                })
            }
        }
    }
}

fn content_block_text(block: Option<&ContentBlock>) -> &str {
    match block {
        Some(ContentBlock::Text(content)) => &content.text,
        _ => "",
    }
}

fn content_block_thinking(block: Option<&ContentBlock>) -> &str {
    match block {
        Some(ContentBlock::Thinking(content)) => &content.thinking,
        _ => "",
    }
}

fn content_block_tool_call(block: Option<&ContentBlock>) -> Option<&pi_core::ToolCall> {
    match block {
        Some(ContentBlock::ToolCall(tool_call)) => Some(tool_call),
        _ => None,
    }
}

fn tool_result_value(result: &ToolResult) -> Value {
    json!({
        "content": result.content,
        "details": result.details,
        "usage": result.usage,
        "isError": result.is_error,
        "terminate": result.terminate,
    })
}

struct JsCommand {
    callback_id: String,
    spec: CommandSpec,
    lease: Arc<JsGenerationLease>,
}

impl JsCommand {
    fn new(manifest: JsCommandManifest, lease: Arc<JsGenerationLease>) -> Self {
        Self {
            callback_id: manifest.callback_id,
            spec: CommandSpec {
                name: manifest.name,
                description: manifest.description,
                argument_hint: manifest.argument_hint,
            },
            lease,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
enum JsCommandOutcome {
    Handled,
    Transform { text: String },
}

#[async_trait]
impl Command for JsCommand {
    fn spec(&self) -> CommandSpec {
        self.spec.clone()
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        let result = self
            .lease
            .invoke(
                &self.callback_id,
                JsInvocationKind::Command,
                json!({
                    "context": { "cwd": context.cwd.to_string_lossy() },
                    "arguments": arguments,
                }),
                Some(&context.abort_signal),
            )
            .await
            .map_err(|error| match error {
                JsInvokeError::Aborted => CommandError::Aborted,
                JsInvokeError::Callback(error) => CommandError::Execution(error.to_string()),
            })?;
        match serde_json::from_value::<JsCommandOutcome>(result)
            .map_err(|error| CommandError::Execution(format!("invalid command result: {error}")))?
        {
            JsCommandOutcome::Handled => Ok(CommandOutcome::Handled),
            JsCommandOutcome::Transform { text } => Ok(CommandOutcome::TransformInput(text)),
        }
    }
}

struct JsProviderPlugin {
    id: PluginId,
    hooks: Vec<JsHookManifest>,
    lease: Arc<JsGenerationLease>,
}

#[async_trait]
impl ProviderPlugin for JsProviderPlugin {
    fn id(&self) -> PluginId {
        self.id.clone()
    }

    async fn before_provider_request(
        &self,
        context: ProviderPluginContext,
        event: BeforeProviderRequestEvent,
    ) -> Result<Option<Value>, PluginError> {
        let original = event.payload;
        let mut payload = original.clone();
        for hook in self
            .hooks
            .iter()
            .filter(|hook| hook.name == "before_provider_request")
        {
            let result = self
                .lease
                .invoke(
                    &hook.callback_id,
                    JsInvocationKind::ProviderHook,
                    json!({
                        "hook": "before_provider_request",
                        "context": {
                            "pluginId": context.plugin_id.as_str(),
                            "generation": context.generation,
                            "providerId": context.provider_id.as_str(),
                            "modelId": context.model_id.as_str(),
                            "cwd": context.cwd.to_string_lossy(),
                        },
                        "event": {
                            "type": "before_provider_request",
                            "payload": payload,
                        },
                    }),
                    Some(&context.abort_signal),
                )
                .await
                .map_err(|error| PluginError::Hook {
                    plugin_id: self.id.clone(),
                    hook: "before_provider_request",
                    message: match error {
                        JsInvokeError::Aborted => "JavaScript hook was aborted".to_string(),
                        JsInvokeError::Callback(error) => error.to_string(),
                    },
                })?;
            if !result.is_null() {
                payload = result;
            }
        }
        Ok((payload != original).then_some(payload))
    }
}

struct JsSessionPlugin {
    id: PluginId,
    hooks: Vec<JsHookManifest>,
    lease: Arc<JsGenerationLease>,
}

impl JsSessionPlugin {
    async fn invoke(
        &self,
        name: &'static str,
        context: &SessionPluginContext,
        event: Value,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, SessionPluginError> {
        let mut last = Value::Null;
        for hook in self.hooks.iter().filter(|hook| hook.name == name) {
            let result = self
                .lease
                .invoke(
                    &hook.callback_id,
                    JsInvocationKind::SessionHook,
                    json!({
                        "hook": name,
                        "context": session_context_value(context),
                        "event": event,
                    }),
                    signal,
                )
                .await
                .map_err(|error| {
                    SessionPluginError::Failure(match error {
                        JsInvokeError::Aborted => "JavaScript session hook was aborted".to_string(),
                        JsInvokeError::Callback(error) => error.to_string(),
                    })
                })?;
            if !result.is_null() {
                let cancelled = result.get("cancel").and_then(Value::as_bool) == Some(true);
                last = result;
                if cancelled {
                    break;
                }
            }
        }
        Ok(last)
    }

    async fn notify(
        &self,
        name: &'static str,
        context: &SessionPluginContext,
        event: Value,
    ) -> Result<(), SessionPluginError> {
        self.invoke(name, context, event, None).await.map(drop)
    }
}

fn session_context_value(context: &SessionPluginContext) -> Value {
    json!({
        "pluginId": context.plugin_id.as_str(),
        "generation": context.generation,
        "session": {
            "id": context.session.id,
            "path": context.session.path,
            "cwd": context.session.cwd,
            "parentSessionId": context.session.parent_session_id,
        },
    })
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSessionBeforeSwitchResult {
    #[serde(default)]
    cancel: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSessionBeforeForkResult {
    #[serde(default)]
    cancel: bool,
    #[serde(default)]
    skip_conversation_restore: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsCompactionResult {
    summary: String,
    tokens_before: u64,
    #[serde(default)]
    details: Option<Value>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSessionBeforeCompactResult {
    #[serde(default)]
    cancel: bool,
    #[serde(default)]
    compaction: Option<JsCompactionResult>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSessionBeforeTreeResult {
    #[serde(default)]
    cancel: bool,
    #[serde(default)]
    summary: Option<JsSessionTreeSummary>,
    #[serde(default)]
    custom_instructions: Option<String>,
    #[serde(default)]
    replace_instructions: Option<bool>,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSessionTreeSummary {
    summary: String,
    #[serde(default)]
    details: Option<Value>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[async_trait]
impl SessionPlugin for JsSessionPlugin {
    fn id(&self) -> PluginId {
        self.id.clone()
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        event: &SessionStartEvent,
    ) -> Result<(), SessionPluginError> {
        self.notify(
            "session_start",
            context,
            json!({
                "type": "session_start",
                "reason": session_start_reason(event.reason),
                "previousSessionFile": event.previous_session_file,
            }),
        )
        .await
    }

    async fn session_info_changed(
        &self,
        context: &SessionPluginContext,
        event: &SessionInfoChangedEvent,
    ) -> Result<(), SessionPluginError> {
        self.notify(
            "session_info_changed",
            context,
            json!({ "type": "session_info_changed", "name": event.name }),
        )
        .await
    }

    async fn session_before_switch(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeSwitchEvent,
    ) -> Result<Option<SessionBeforeSwitchResult>, SessionPluginError> {
        let result = self
            .invoke(
                "session_before_switch",
                context,
                json!({
                    "type": "session_before_switch",
                    "reason": session_switch_reason(event.reason),
                    "targetSessionFile": event.target_session_file,
                }),
                None,
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let result = serde_json::from_value::<JsSessionBeforeSwitchResult>(result)
            .map_err(invalid_session_result)?;
        Ok(Some(SessionBeforeSwitchResult {
            cancel: result.cancel,
        }))
    }

    async fn session_before_fork(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeForkEvent,
    ) -> Result<Option<SessionBeforeForkResult>, SessionPluginError> {
        let result = self
            .invoke(
                "session_before_fork",
                context,
                json!({
                    "type": "session_before_fork",
                    "entryId": event.entry_id,
                    "position": session_fork_position(event.position),
                }),
                None,
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let result = serde_json::from_value::<JsSessionBeforeForkResult>(result)
            .map_err(invalid_session_result)?;
        Ok(Some(SessionBeforeForkResult {
            cancel: result.cancel,
            skip_conversation_restore: result.skip_conversation_restore,
        }))
    }

    async fn session_before_compact(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, SessionPluginError> {
        let preparation = &event.preparation;
        let result = self
            .invoke(
                "session_before_compact",
                context,
                json!({
                    "type": "session_before_compact",
                    "preparation": {
                        "messagesToSummarize": preparation.messages_to_summarize,
                        "turnPrefixMessages": preparation.turn_prefix_messages,
                        "retainedTail": preparation.retained_tail,
                        "isSplitTurn": preparation.is_split_turn,
                        "tokensBefore": preparation.tokens_before,
                        "previousSummary": preparation.previous_summary,
                        "fileOps": {
                            "read": sorted_strings(&preparation.file_ops.read),
                            "written": sorted_strings(&preparation.file_ops.written),
                            "edited": sorted_strings(&preparation.file_ops.edited),
                        },
                        "settings": preparation.settings,
                    },
                    "branchEntries": event.branch_entries,
                    "customInstructions": event.custom_instructions,
                    "reason": event.reason,
                    "willRetry": event.will_retry,
                }),
                Some(&event.signal),
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let result = serde_json::from_value::<JsSessionBeforeCompactResult>(result)
            .map_err(invalid_session_result)?;
        Ok(Some(SessionBeforeCompactResult {
            cancel: result.cancel,
            compaction: result.compaction.map(|compaction| CompactionEntry {
                summary: compaction.summary,
                retained_tail: preparation.retained_tail.clone(),
                tokens_before: compaction.tokens_before,
                details: compaction.details,
                usage: compaction.usage,
            }),
        }))
    }

    async fn session_compact(
        &self,
        context: &SessionPluginContext,
        event: &SessionCompactEvent,
    ) -> Result<(), SessionPluginError> {
        self.notify(
            "session_compact",
            context,
            json!({
                "type": "session_compact",
                "compactionEntry": event.compaction_entry,
                "fromExtension": event.from_extension,
                "reason": event.reason,
                "willRetry": event.will_retry,
            }),
        )
        .await
    }

    async fn session_compact_failed(
        &self,
        context: &SessionPluginContext,
        event: &SessionCompactFailedEvent,
    ) -> Result<(), SessionPluginError> {
        self.notify(
            "session_compact_failed",
            context,
            json!({
                "type": "session_compact_failed",
                "reason": event.reason,
                "errorMessage": event.error_message,
                "aborted": event.aborted,
                "willRetry": event.will_retry,
                "fromExtension": event.from_extension,
            }),
        )
        .await
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        event: &SessionShutdownEvent,
    ) -> Result<(), SessionPluginError> {
        self.notify(
            "session_shutdown",
            context,
            json!({
                "type": "session_shutdown",
                "reason": session_shutdown_reason(event.reason),
                "targetSessionFile": event.target_session_file,
            }),
        )
        .await
    }

    async fn session_before_tree(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeTreeEvent,
    ) -> Result<Option<SessionBeforeTreeResult>, SessionPluginError> {
        let result = self
            .invoke(
                "session_before_tree",
                context,
                json!({
                    "type": "session_before_tree",
                    "preparation": {
                        "targetId": event.preparation.target_id,
                        "oldLeafId": event.preparation.old_leaf_id,
                        "commonAncestorId": event.preparation.common_ancestor_id,
                        "entriesToSummarize": event.preparation.entries_to_summarize,
                        "userWantsSummary": event.preparation.user_wants_summary,
                        "customInstructions": event.preparation.custom_instructions,
                        "replaceInstructions": event.preparation.replace_instructions,
                        "label": event.preparation.label,
                    },
                }),
                Some(&event.signal),
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let result = serde_json::from_value::<JsSessionBeforeTreeResult>(result)
            .map_err(invalid_session_result)?;
        Ok(Some(SessionBeforeTreeResult {
            cancel: result.cancel,
            summary: result.summary.map(|summary| SessionTreeSummary {
                summary: summary.summary,
                details: summary.details,
                usage: summary.usage,
            }),
            custom_instructions: result.custom_instructions,
            replace_instructions: result.replace_instructions,
            label: result.label,
        }))
    }

    async fn session_tree(
        &self,
        context: &SessionPluginContext,
        event: &SessionTreeEvent,
    ) -> Result<(), SessionPluginError> {
        self.notify(
            "session_tree",
            context,
            json!({
                "type": "session_tree",
                "newLeafId": event.new_leaf_id,
                "oldLeafId": event.old_leaf_id,
                "summaryEntry": event.summary_entry,
                "fromExtension": event.from_extension,
            }),
        )
        .await
    }
}

fn invalid_session_result(error: serde_json::Error) -> SessionPluginError {
    SessionPluginError::Failure(format!("invalid JavaScript session hook result: {error}"))
}

fn sorted_strings(values: &HashSet<String>) -> Vec<&str> {
    let mut values = values.iter().map(String::as_str).collect::<Vec<_>>();
    values.sort_unstable();
    values
}

fn session_start_reason(reason: SessionStartReason) -> &'static str {
    match reason {
        SessionStartReason::Startup => "startup",
        SessionStartReason::Reload => "reload",
        SessionStartReason::New => "new",
        SessionStartReason::Resume => "resume",
        SessionStartReason::Fork => "fork",
    }
}

fn session_shutdown_reason(reason: SessionShutdownReason) -> &'static str {
    match reason {
        SessionShutdownReason::Quit => "quit",
        SessionShutdownReason::Reload => "reload",
        SessionShutdownReason::New => "new",
        SessionShutdownReason::Resume => "resume",
        SessionShutdownReason::Fork => "fork",
    }
}

fn session_switch_reason(reason: SessionSwitchReason) -> &'static str {
    match reason {
        SessionSwitchReason::New => "new",
        SessionSwitchReason::Resume => "resume",
    }
}

fn session_fork_position(position: SessionForkPosition) -> &'static str {
    match position {
        SessionForkPosition::Before => "before",
        SessionForkPosition::At => "at",
    }
}

struct JsTool {
    callback_id: String,
    spec: ToolSpec,
    lease: Arc<JsGenerationLease>,
}

impl JsTool {
    fn new(manifest: JsToolManifest, lease: Arc<JsGenerationLease>) -> Self {
        let execution_mode = match manifest.execution_mode {
            JsToolExecutionMode::Sequential => ToolExecutionMode::Sequential,
            JsToolExecutionMode::Parallel => ToolExecutionMode::Parallel,
        };
        Self {
            callback_id: manifest.callback_id,
            spec: ToolSpec {
                name: manifest.name,
                label: manifest.label,
                description: manifest.description,
                parameters: manifest.parameters,
                execution_mode,
                prompt_snippet: manifest.prompt_snippet,
                prompt_guidelines: manifest.prompt_guidelines,
            },
            lease,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsToolResult {
    content: Vec<ContentBlock>,
    #[serde(default)]
    details: Option<Value>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    terminate: bool,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsToolResultPatch {
    #[serde(default)]
    content: Option<Vec<ContentBlock>>,
    #[serde(default)]
    details: Option<Value>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    terminate: Option<bool>,
}

impl JsToolResultPatch {
    fn into_core(self) -> ToolResultPatch {
        ToolResultPatch {
            content: self.content,
            details: self.details,
            usage: self.usage,
            is_error: self.is_error,
            terminate: self.terminate,
        }
    }

    fn merge(&mut self, next: &Self) {
        if next.content.is_some() {
            self.content.clone_from(&next.content);
        }
        if next.details.is_some() {
            self.details.clone_from(&next.details);
        }
        if next.usage.is_some() {
            self.usage.clone_from(&next.usage);
        }
        if next.is_error.is_some() {
            self.is_error = next.is_error;
        }
        if next.terminate.is_some() {
            self.terminate = next.terminate;
        }
    }
}

impl From<JsToolResult> for ToolResult {
    fn from(result: JsToolResult) -> Self {
        Self {
            content: result.content,
            details: result.details,
            usage: result.usage,
            is_error: result.is_error,
            terminate: result.terminate,
        }
    }
}

#[async_trait]
impl Tool for JsTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn execute(
        &self,
        context: ToolContext,
        tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let response = self
            .lease
            .invoke(
                &self.callback_id,
                JsInvocationKind::Tool,
                json!({
                "context": {
                    "cwd": context.cwd.to_string_lossy(),
                    "toolCallId": tool_call_id.as_str(),
                },
                "input": input,
                }),
                Some(&context.abort_signal),
            )
            .await
            .map_err(|error| match error {
                JsInvokeError::Aborted => ToolError::Aborted,
                JsInvokeError::Callback(error) => ToolError::Execution(error.to_string()),
            })?;

        serde_json::from_value::<JsToolResult>(response)
            .map(ToolResult::from)
            .map_err(|error| {
                ToolError::Execution(format!("invalid JavaScript tool result: {error}"))
            })
    }
}
