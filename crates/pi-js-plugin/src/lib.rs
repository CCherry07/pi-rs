mod context;

pub use context::*;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    AbortSignal, AfterProviderResponseEvent, AgentEndEvent, AgentHook, AgentHookInterests,
    AgentPlugin, AgentPluginContext, AgentSettledEvent, AgentStartEvent, AssistantMessage,
    AssistantStreamId, BeforeAgentStartEvent, BeforeAgentStartPatch, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, Command, CommandContext, CommandError, CommandOutcome, CommandSpec,
    ContentBlock, ContextEvent, ContextPatch, CustomMessage, CustomMessageContent, ImageContent,
    InputContext, InputEvent, InputPatch, InputSource, InputStreamingBehavior, Message,
    MessageEndEvent, MessageEndPatch, MessageStartEvent, MessageUpdateEvent, PluginError, PluginId,
    ProviderPlugin, ProviderPluginContext, RegisterContext, RunId, StreamEvent, Tool,
    ToolCallEvent, ToolCallId, ToolCallPatch, ToolContext, ToolError, ToolExecutionEndEvent,
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

static NEXT_JS_INVOCATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsGenerationManifest {
    pub generation_id: String,
    #[serde(default)]
    pub agent_plugins: Vec<JsAgentPluginManifest>,
    #[serde(default)]
    pub provider_plugins: Vec<JsProviderPluginManifest>,
    #[serde(default)]
    pub provider_registrations: Vec<JsProviderRegistration>,
    #[serde(default)]
    pub session_plugins: Vec<JsSessionPluginManifest>,
    #[serde(default)]
    pub diagnostics: Vec<JsExtensionDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsProviderRegistration {
    pub plugin_id: String,
    pub path: String,
    pub name: String,
    pub config: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsExtensionDiagnostic {
    pub plugin_id: String,
    pub path: String,
    pub feature: String,
    pub status: JsExtensionSupportStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JsExtensionSupportStatus {
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsGenerationRequest {
    pub project_trusted: bool,
    #[serde(default)]
    pub extension_paths: Vec<String>,
    pub mode: PresentationMode,
    pub cwd: String,
    #[serde(default)]
    pub flag_values: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum JsHostOperation {
    PrepareGeneration {
        request: JsGenerationRequest,
    },
    Invoke {
        invocation: JsInvocation,
    },
    InvokeHookBatch {
        invocation: JsHookBatchInvocation,
    },
    InvokeStreamHookBatch {
        invocation: JsStreamHookBatchInvocation,
    },
    ReleaseStream {
        generation_id: String,
        stream_id: String,
    },
    Cancel {
        invocation_id: String,
    },
    RetireGeneration {
        generation_id: String,
    },
}

#[cfg(test)]
mod wire_tests {
    use std::sync::Arc;

    use pi_core::{AgentHook, PresentationMode, StreamEvent, ToolExecutionMode};
    use serde_json::json;

    use super::{
        AGENT_HOOKS, JsGenerationRequest, JsHookBatchCallback, JsHookBatchInvocation,
        JsHostOperation, JsStreamHookBatchInvocation,
    };

    #[test]
    fn every_validated_agent_hook_has_a_core_route() {
        for name in AGENT_HOOKS {
            assert!(
                AgentHook::from_name(name).is_some(),
                "missing route for {name}"
            );
        }
    }

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

        let batch = serde_json::to_value(JsHostOperation::InvokeHookBatch {
            invocation: JsHookBatchInvocation {
                invocation_id: "invocation-2".to_string(),
                generation_id: "generation-1".to_string(),
                hook: "message_update".to_string(),
                callbacks: vec![JsHookBatchCallback {
                    callback_id: "callback-1".to_string(),
                    context: json!({ "pluginId": "extension" }),
                }],
                event: json!({ "type": "message_update" }),
            },
        })
        .unwrap();
        assert_eq!(batch["type"], "invokeHookBatch");
        assert_eq!(batch["invocation"]["invocationId"], "invocation-2");
        assert_eq!(
            batch["invocation"]["callbacks"][0]["callbackId"],
            "callback-1"
        );

        let stream = serde_json::to_value(JsHostOperation::InvokeStreamHookBatch {
            invocation: JsStreamHookBatchInvocation {
                invocation_id: "invocation-3".to_string(),
                generation_id: "generation-1".to_string(),
                callbacks: Vec::new().into(),
                stream_id: "stream-1".to_string(),
                initial_message: None,
                update: Arc::new(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: "delta".to_string(),
                }),
            },
        })
        .unwrap();
        assert_eq!(stream["type"], "invokeStreamHookBatch");
        assert_eq!(stream["invocation"]["streamId"], "stream-1");
        assert!(stream["invocation"].get("initialMessage").is_none());
        assert_eq!(stream["invocation"]["update"]["type"], "textDelta");
    }

    #[test]
    fn generation_request_contains_only_resolved_extension_paths() {
        let prepare = serde_json::to_value(JsHostOperation::PrepareGeneration {
            request: JsGenerationRequest {
                project_trusted: true,
                extension_paths: vec!["/extensions/example.ts".to_string()],
                mode: PresentationMode::Print,
                cwd: "/workspace".to_string(),
                flag_values: std::collections::BTreeMap::from([(
                    "fixture".to_string(),
                    json!(true),
                )]),
            },
        })
        .unwrap();
        let request = &prepare["request"];

        assert_eq!(request["extensionPaths"][0], "/extensions/example.ts");
        assert_eq!(request["projectTrusted"], true);
        assert_eq!(request["mode"], "print");
        assert_eq!(request["cwd"], "/workspace");
        assert_eq!(request["flagValues"]["fixture"], true);
        assert!(request.get("agentDir").is_none());
        assert!(request.get("explicitPaths").is_none());
        assert!(request.get("discoverExtensions").is_none());
    }

    #[test]
    fn canonical_contract_types_keep_javascript_wire_shape() {
        assert_eq!(
            serde_json::to_value(ToolExecutionMode::Sequential).unwrap(),
            json!("sequential")
        );
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
    #[serde(default)]
    pub prepare_callback_id: Option<String>,
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default)]
    pub prompt_snippet: Option<String>,
    #[serde(default)]
    pub prompt_guidelines: Vec<String>,
    #[serde(default)]
    pub execution_mode: ToolExecutionMode,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsHookBatchInvocation {
    pub invocation_id: String,
    pub generation_id: String,
    pub hook: String,
    pub callbacks: Vec<JsHookBatchCallback>,
    pub event: Value,
}

/// Compact wire payload for high-frequency assistant stream hooks.
///
/// The cumulative message is sent only when a stream is first observed. Each
/// subsequent invocation carries the provider delta directly, allowing the JS
/// host to retain chunked state and materialize a snapshot only when a callback
/// actually reads one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsStreamHookBatchInvocation {
    pub invocation_id: String,
    pub generation_id: String,
    pub callbacks: Arc<[JsHookBatchCallback]>,
    pub stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<Box<AssistantMessage>>,
    pub update: Arc<StreamEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsHookBatchCallback {
    pub callback_id: String,
    pub context: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsHookBatchResult {
    #[serde(default)]
    pub errors: Vec<JsHookBatchError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsHookBatchError {
    pub callback_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JsInvocationKind {
    Tool,
    ToolPrepareArguments,
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
    async fn invoke(
        &self,
        invocation: JsInvocation,
        context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError>;

    async fn invoke_with_tool_updates(
        &self,
        invocation: JsInvocation,
        context: PluginContextHandle,
        _updates: ToolUpdateSink,
    ) -> Result<Value, JsCallbackError> {
        self.invoke(invocation, context).await
    }

    /// Executes observer hooks in registration order. Dispatchers that cross
    /// a process or language seam can override this to encode the shared event
    /// once; the default keeps lightweight in-process adapters compatible.
    async fn invoke_hook_batch(
        &self,
        invocation: JsHookBatchInvocation,
        context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        let mut errors = Vec::new();
        for callback in invocation.callbacks {
            let callback_id = callback.callback_id;
            let result = self
                .invoke(
                    JsInvocation {
                        invocation_id: invocation.invocation_id.clone(),
                        generation_id: invocation.generation_id.clone(),
                        callback_id: callback_id.clone(),
                        kind: JsInvocationKind::AgentHook,
                        payload: json!({
                            "hook": invocation.hook.clone(),
                            "context": callback.context,
                            "event": invocation.event.clone(),
                        }),
                    },
                    context.clone(),
                )
                .await;
            if let Err(error) = result {
                errors.push(JsHookBatchError {
                    callback_id,
                    message: error.message,
                });
            }
        }
        Ok(JsHookBatchResult { errors })
    }

    async fn invoke_stream_hook_batch(
        &self,
        invocation: JsStreamHookBatchInvocation,
        context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        let event = json!({
            "type": "message_update",
            "streamId": invocation.stream_id,
            "initialMessage": invocation.initial_message,
            "update": invocation.update,
        });
        self.invoke_hook_batch(
            JsHookBatchInvocation {
                invocation_id: invocation.invocation_id,
                generation_id: invocation.generation_id,
                hook: "message_update".to_string(),
                callbacks: invocation.callbacks.to_vec(),
                event,
            },
            context,
        )
        .await
    }

    fn cancel(&self, _invocation_id: &str) {}

    fn release_stream(&self, _generation_id: &str, _stream_id: &str) {}

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
    provider_registrations: Vec<JsProviderRegistration>,
    session_plugins: Vec<Arc<dyn SessionPlugin>>,
    diagnostics: Vec<JsExtensionDiagnostic>,
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
            provider_registrations,
            session_plugins,
            diagnostics,
        } = manifest;
        let lease = Arc::new(JsGenerationLease {
            generation_id,
            dispatcher,
        });
        validate_manifest(
            &lease.generation_id,
            &agent_plugins,
            &provider_plugins,
            &provider_registrations,
            &session_plugins,
        )?;
        // JavaScript extensions are one contiguous generation contribution.
        // Route only observer hooks through the first adapter so callbacks can
        // share one encoded event and one host crossing. Chained/mutating hooks
        // remain on their owning adapter and retain the typed driver semantics.
        let mut generation_observer_hooks = Some(
            agent_plugins
                .iter()
                .flat_map(|plugin| {
                    let plugin_id = PluginId::new(plugin.id.clone());
                    plugin
                        .hooks
                        .iter()
                        .filter(|hook| is_batched_agent_observer(&hook.name))
                        .map(move |hook| JsAgentHook {
                            plugin_id: plugin_id.clone(),
                            name: hook.name.clone(),
                            callback_id: hook.callback_id.clone(),
                        })
                })
                .collect::<Vec<_>>(),
        );
        let agent_plugins = agent_plugins
            .into_iter()
            .enumerate()
            .map(|(index, plugin)| {
                let JsAgentPluginManifest {
                    id,
                    tools,
                    commands,
                    hooks,
                } = plugin;
                let plugin_id = PluginId::new(id);
                let tools = tools
                    .into_iter()
                    .map(|tool| Arc::new(JsTool::new(tool, Arc::clone(&lease))) as Arc<dyn Tool>)
                    .collect();
                let commands = commands
                    .into_iter()
                    .map(|command| {
                        Arc::new(JsCommand::new(command, Arc::clone(&lease))) as Arc<dyn Command>
                    })
                    .collect();
                let mut hooks = hooks
                    .into_iter()
                    .filter(|hook| !is_batched_agent_observer(&hook.name))
                    .map(|hook| JsAgentHook {
                        plugin_id: plugin_id.clone(),
                        name: hook.name,
                        callback_id: hook.callback_id,
                    })
                    .collect::<Vec<_>>();
                if index == 0 {
                    hooks.extend(generation_observer_hooks.take().unwrap_or_default());
                }
                Arc::new(JsAgentPlugin {
                    id: plugin_id,
                    tools,
                    commands,
                    hooks,
                    lease: Arc::clone(&lease),
                    active_streams: Mutex::new(HashMap::new()),
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
            provider_registrations,
            session_plugins,
            diagnostics,
        })
    }

    pub fn agent_plugins(&self) -> Vec<Arc<dyn AgentPlugin>> {
        self.agent_plugins.clone()
    }

    pub fn provider_plugins(&self) -> Vec<Arc<dyn ProviderPlugin>> {
        self.provider_plugins.clone()
    }

    pub fn provider_registrations(&self) -> &[JsProviderRegistration] {
        &self.provider_registrations
    }

    pub fn session_plugins(&self) -> Vec<Arc<dyn SessionPlugin>> {
        self.session_plugins.clone()
    }

    pub fn diagnostics(&self) -> &[JsExtensionDiagnostic] {
        &self.diagnostics
    }
}

struct HostCallbackDispatcher {
    host: Arc<dyn JsPluginHost>,
}

#[async_trait]
impl JsCallbackDispatcher for HostCallbackDispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.host.invoke(invocation, context).await
    }

    async fn invoke_hook_batch(
        &self,
        invocation: JsHookBatchInvocation,
        context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        self.host.invoke_hook_batch(invocation, context).await
    }

    async fn invoke_stream_hook_batch(
        &self,
        invocation: JsStreamHookBatchInvocation,
        context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        self.host
            .invoke_stream_hook_batch(invocation, context)
            .await
    }

    fn cancel(&self, invocation_id: &str) {
        self.host.cancel(invocation_id);
    }

    fn release_stream(&self, generation_id: &str, stream_id: &str) {
        self.host.release_stream(generation_id, stream_id);
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
    "agent_settled",
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

fn is_batched_agent_observer(name: &str) -> bool {
    matches!(
        name,
        "agent_start"
            | "agent_end"
            | "agent_settled"
            | "turn_start"
            | "turn_end"
            | "message_start"
            | "message_update"
            | "tool_execution_start"
            | "tool_execution_update"
            | "tool_execution_end"
    )
}

const PROVIDER_HOOKS: &[&str] = &[
    "before_provider_request",
    "before_provider_headers",
    "after_provider_response",
];
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
    provider_registrations: &[JsProviderRegistration],
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
    for registration in provider_registrations {
        if registration.plugin_id.trim().is_empty() {
            return Err(JsPluginError::InvalidManifest(
                "provider registration pluginId must not be empty".to_string(),
            ));
        }
        if registration.path.trim().is_empty() {
            return Err(JsPluginError::InvalidManifest(format!(
                "provider registration {} path must not be empty",
                registration.plugin_id
            )));
        }
        if registration.name.trim().is_empty() {
            return Err(JsPluginError::InvalidManifest(format!(
                "provider registration {} name must not be empty",
                registration.plugin_id
            )));
        }
        if !registration.config.is_object() {
            return Err(JsPluginError::InvalidManifest(format!(
                "provider registration {}/{} config must be an object",
                registration.plugin_id, registration.name
            )));
        }
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
        if let Some(callback_id) = &tool.prepare_callback_id {
            if callback_id.trim().is_empty() {
                return Err(JsPluginError::InvalidManifest(format!(
                    "tool {} prepareCallbackId must not be empty",
                    tool.name
                )));
            }
            validate_callback_id(callback_id, callback_ids)?;
        }
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
        context: PluginContextHandle,
        abort_signal: Option<&AbortSignal>,
    ) -> Result<Value, JsInvokeError> {
        self.invoke_inner(callback_id, kind, payload, context, abort_signal, None)
            .await
    }

    async fn invoke_tool(
        &self,
        callback_id: &str,
        payload: Value,
        context: PluginContextHandle,
        abort_signal: &AbortSignal,
        updates: ToolUpdateSink,
    ) -> Result<Value, JsInvokeError> {
        self.invoke_inner(
            callback_id,
            JsInvocationKind::Tool,
            payload,
            context,
            Some(abort_signal),
            Some(updates),
        )
        .await
    }

    async fn invoke_hook_batch(
        &self,
        hook: &'static str,
        callbacks: Vec<JsHookBatchCallback>,
        event: Value,
        context: PluginContextHandle,
        abort_signal: &AbortSignal,
    ) -> Result<JsHookBatchResult, JsInvokeError> {
        let invocation_id = format!(
            "js-invocation-{}",
            NEXT_JS_INVOCATION_ID.fetch_add(1, Ordering::Relaxed)
        );
        let invocation = JsHookBatchInvocation {
            invocation_id: invocation_id.clone(),
            generation_id: self.generation_id.clone(),
            hook: hook.to_string(),
            callbacks,
            event,
        };
        tokio::select! {
            response = self.dispatcher.invoke_hook_batch(invocation, context) => {
                response.map_err(JsInvokeError::Callback)
            }
            () = abort_signal.wait() => {
                self.dispatcher.cancel(&invocation_id);
                Err(JsInvokeError::Aborted)
            }
        }
    }

    async fn invoke_stream_hook_batch(
        &self,
        callbacks: Arc<[JsHookBatchCallback]>,
        stream_id: String,
        initial_message: Option<AssistantMessage>,
        update: Arc<StreamEvent>,
        context: PluginContextHandle,
        abort_signal: &AbortSignal,
    ) -> Result<JsHookBatchResult, JsInvokeError> {
        let invocation_id = format!(
            "js-invocation-{}",
            NEXT_JS_INVOCATION_ID.fetch_add(1, Ordering::Relaxed)
        );
        let invocation = JsStreamHookBatchInvocation {
            invocation_id: invocation_id.clone(),
            generation_id: self.generation_id.clone(),
            callbacks,
            stream_id,
            initial_message: initial_message.map(Box::new),
            update,
        };
        tokio::select! {
            response = self.dispatcher.invoke_stream_hook_batch(invocation, context) => {
                response.map_err(JsInvokeError::Callback)
            }
            () = abort_signal.wait() => {
                self.dispatcher.cancel(&invocation_id);
                Err(JsInvokeError::Aborted)
            }
        }
    }

    fn release_stream(&self, stream_id: &AssistantStreamId) {
        self.dispatcher
            .release_stream(&self.generation_id, stream_id.as_str());
    }

    async fn invoke_inner(
        &self,
        callback_id: &str,
        kind: JsInvocationKind,
        payload: Value,
        context: PluginContextHandle,
        abort_signal: Option<&AbortSignal>,
        updates: Option<ToolUpdateSink>,
    ) -> Result<Value, JsInvokeError> {
        let invocation_id = format!(
            "js-invocation-{}",
            NEXT_JS_INVOCATION_ID.fetch_add(1, Ordering::Relaxed)
        );
        let invocation = JsInvocation {
            invocation_id: invocation_id.clone(),
            generation_id: self.generation_id.clone(),
            callback_id: callback_id.to_string(),
            kind,
            payload,
        };
        let response = async {
            match updates {
                Some(updates) => {
                    self.dispatcher
                        .invoke_with_tool_updates(invocation, context, updates)
                        .await
                }
                None => self.dispatcher.invoke(invocation, context).await,
            }
        };
        if let Some(signal) = abort_signal {
            tokio::select! {
                response = response => {
                    response.map_err(JsInvokeError::Callback)
                }
                () = signal.wait() => {
                    self.dispatcher.cancel(&invocation_id);
                    Err(JsInvokeError::Aborted)
                }
            }
        } else {
            response.await.map_err(JsInvokeError::Callback)
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
    hooks: Vec<JsAgentHook>,
    lease: Arc<JsGenerationLease>,
    active_streams: Mutex<HashMap<RunId, ActiveJsStream>>,
}

struct ActiveJsStream {
    id: AssistantStreamId,
    callbacks: Arc<[JsHookBatchCallback]>,
}

#[derive(Clone)]
struct JsAgentHook {
    plugin_id: PluginId,
    name: String,
    callback_id: String,
}

impl JsAgentPlugin {
    fn hook_callbacks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a JsAgentHook> {
        self.hooks.iter().filter(move |hook| hook.name == name)
    }

    async fn invoke_hook(
        &self,
        name: &'static str,
        hook: &JsAgentHook,
        native_context: PluginContextHandle,
        context: Value,
        event: Value,
        signal: &AbortSignal,
    ) -> Result<Value, PluginError> {
        self.lease
            .invoke(
                &hook.callback_id,
                JsInvocationKind::AgentHook,
                json!({ "hook": name, "context": context, "event": event }),
                native_context,
                Some(signal),
            )
            .await
            .map_err(|error| PluginError::Hook {
                plugin_id: hook.plugin_id.clone(),
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
        context: &AgentPluginContext,
        event: Value,
    ) -> Result<(), PluginError> {
        let hooks = self.hook_callbacks(name).cloned().collect::<Vec<_>>();
        if hooks.is_empty() {
            return Ok(());
        }
        let callbacks = hooks
            .iter()
            .map(|hook| {
                let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
                JsHookBatchCallback {
                    callback_id: hook.callback_id.clone(),
                    context: agent_plugin_context_value(&callback_context),
                }
            })
            .collect();
        let result = self
            .lease
            .invoke_hook_batch(
                name,
                callbacks,
                event,
                context.plugin_context_handle(),
                context.signal(),
            )
            .await;
        match result {
            Ok(result) => {
                for error in result.errors {
                    let Some(hook) = hooks
                        .iter()
                        .find(|hook| hook.callback_id == error.callback_id)
                    else {
                        continue;
                    };
                    report_agent_hook_error(context, name, hook, error.message);
                }
            }
            Err(error) => {
                let message = match error {
                    JsInvokeError::Aborted => "JavaScript hook batch was aborted".to_string(),
                    JsInvokeError::Callback(error) => error.to_string(),
                };
                for hook in &hooks {
                    report_agent_hook_error(context, name, hook, message.clone());
                }
            }
        }
        Ok(())
    }

    async fn notify_stream_hooks(
        &self,
        context: &AgentPluginContext,
        event: MessageUpdateEvent,
    ) -> Result<(), PluginError> {
        const HOOK: &str = "message_update";
        let hooks = self.hook_callbacks(HOOK).collect::<Vec<_>>();
        if hooks.is_empty() {
            return Ok(());
        }
        let stream_id = event.stream.id().clone();
        let (initial_message, callbacks) = {
            let mut active = self
                .active_streams
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(active_stream) = active
                .get(context.run_id())
                .filter(|active_stream| active_stream.id == stream_id)
            {
                (None, Arc::clone(&active_stream.callbacks))
            } else {
                let callbacks = hooks
                    .iter()
                    .map(|hook| {
                        let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
                        JsHookBatchCallback {
                            callback_id: hook.callback_id.clone(),
                            context: agent_plugin_context_value(&callback_context),
                        }
                    })
                    .collect::<Vec<_>>()
                    .into();
                active.insert(
                    context.run_id().clone(),
                    ActiveJsStream {
                        id: stream_id.clone(),
                        callbacks: Arc::clone(&callbacks),
                    },
                );
                (event.stream.snapshot(), callbacks)
            }
        };
        let is_done = matches!(event.update.as_ref(), StreamEvent::Done { .. });
        let result = self
            .lease
            .invoke_stream_hook_batch(
                callbacks,
                stream_id.clone().to_string(),
                initial_message,
                event.update,
                context.plugin_context_handle(),
                context.signal(),
            )
            .await;
        let reset_stream = result.is_err();
        if is_done || reset_stream {
            let mut active = self
                .active_streams
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active
                .get(context.run_id())
                .is_some_and(|active_stream| active_stream.id == stream_id)
            {
                active.remove(context.run_id());
            }
        }
        if reset_stream {
            self.lease.release_stream(&stream_id);
        }
        match result {
            Ok(result) => {
                for error in result.errors {
                    let Some(hook) = hooks
                        .iter()
                        .find(|hook| hook.callback_id == error.callback_id)
                    else {
                        continue;
                    };
                    report_agent_hook_error(context, HOOK, hook, error.message);
                }
            }
            Err(error) => {
                let message = match error {
                    JsInvokeError::Aborted => {
                        "JavaScript stream hook batch was aborted".to_string()
                    }
                    JsInvokeError::Callback(error) => error.to_string(),
                };
                for hook in &hooks {
                    report_agent_hook_error(context, HOOK, hook, message.clone());
                }
            }
        }
        Ok(())
    }
}

fn report_agent_hook_error(
    context: &AgentPluginContext,
    name: &'static str,
    hook: &JsAgentHook,
    message: impl Into<String>,
) {
    let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
    callback_context.report_hook_error(
        name,
        PluginError::Hook {
            plugin_id: hook.plugin_id.clone(),
            hook: name,
            message: message.into(),
        }
        .to_string(),
    );
}

fn agent_plugin_context_value(context: &AgentPluginContext) -> Value {
    json!({
        "pluginId": context.plugin_id().as_str(),
        "runId": context.run_id().as_str(),
        "cwd": context.cwd().to_string_lossy(),
    })
}

fn input_source(source: InputSource) -> &'static str {
    match source {
        InputSource::Interactive => "interactive",
        InputSource::Rpc => "rpc",
        InputSource::Extension => "extension",
    }
}

fn input_streaming_behavior(behavior: InputStreamingBehavior) -> &'static str {
    match behavior {
        InputStreamingBehavior::Steer => "steer",
        InputStreamingBehavior::FollowUp => "followUp",
    }
}

fn input_images_value(images: &[ImageContent]) -> Value {
    Value::Array(
        images
            .iter()
            .map(|image| {
                json!({
                    "type": "image",
                    "data": image.data,
                    "mimeType": image.mime_type,
                })
            })
            .collect(),
    )
}

fn parse_input_images(value: &Value) -> Result<Vec<ImageContent>, String> {
    let blocks = serde_json::from_value::<Vec<ContentBlock>>(value.clone())
        .map_err(|error| format!("invalid input images result: {error}"))?;
    blocks
        .into_iter()
        .map(|block| match block {
            ContentBlock::Image(image) => Ok(image),
            _ => Err("input images result may only contain image blocks".to_string()),
        })
        .collect()
}

fn pi_prompt_input(messages: &[Message]) -> (String, Vec<ContentBlock>) {
    let Some(user) = messages.iter().rev().find_map(|message| match message {
        Message::User(user) => Some(user),
        Message::Assistant(_) | Message::ToolResult(_) | Message::Custom(_) => None,
    }) else {
        return (String::new(), Vec::new());
    };

    let mut prompt = String::new();
    let mut images = Vec::new();
    for block in &user.content {
        match block {
            ContentBlock::Text(text) => prompt.push_str(&text.text),
            ContentBlock::Image(_) => images.push(block.clone()),
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => {}
        }
    }
    (prompt, images)
}

fn before_agent_start_message(value: &Value) -> Result<Message, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "message must be an object".to_string())?;
    let custom_type = object
        .get("customType")
        .and_then(Value::as_str)
        .ok_or_else(|| "message.customType must be a string".to_string())?
        .to_string();
    let content = match object.get("content") {
        None | Some(Value::Null) => CustomMessageContent::default(),
        Some(Value::String(text)) => CustomMessageContent::Text(text.clone()),
        Some(Value::Array(_)) => {
            let blocks: Vec<ContentBlock> = serde_json::from_value(
                object
                    .get("content")
                    .expect("matched message content")
                    .clone(),
            )
            .map_err(|error| format!("message.content is invalid: {error}"))?;
            if blocks
                .iter()
                .any(|block| !matches!(block, ContentBlock::Text(_) | ContentBlock::Image(_)))
            {
                return Err("message.content only supports text and image blocks".to_string());
            }
            CustomMessageContent::Blocks(blocks)
        }
        Some(_) => {
            return Err("message.content must be a string, an array, null, or omitted".to_string());
        }
    };
    let display = match object.get("display") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(display)) => *display,
        Some(_) => return Err("message.display must be a boolean".to_string()),
    };
    Ok(Message::custom(CustomMessage {
        custom_type,
        content,
        display,
        details: object.get("details").cloned(),
        timestamp_ms: now_ms(),
    }))
}

fn message_end_replacement(value: &Value) -> Result<Message, String> {
    let mut value = value.clone();
    let object = value
        .as_object_mut()
        .ok_or_else(|| "message_end replacement must be an object".to_string())?;
    if matches!(
        object.get("role").and_then(Value::as_str),
        Some("user" | "assistant" | "toolResult" | "custom")
    ) && object.get("content").is_none_or(Value::is_null)
    {
        object.insert("content".to_string(), Value::Array(Vec::new()));
    }
    serde_json::from_value(value)
        .map_err(|error| format!("invalid message_end replacement: {error}"))
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

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[async_trait]
impl AgentPlugin for JsAgentPlugin {
    fn id(&self) -> PluginId {
        self.id.clone()
    }

    fn hook_interests(&self) -> AgentHookInterests {
        let mut hooks: Vec<_> = self
            .hooks
            .iter()
            .map(|hook| {
                AgentHook::from_name(&hook.name)
                    .expect("validated JavaScript agent hook must have a core route")
            })
            .collect();
        if hooks.contains(&AgentHook::MessageUpdate) && !hooks.contains(&AgentHook::MessageEnd) {
            hooks.push(AgentHook::MessageEnd);
        }
        AgentHookInterests::from_hooks(&hooks)
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
        let original = event.clone();
        let mut text = event.text;
        let mut images = event.images;
        let context_value = json!({
            "pluginId": context.plugin_id().as_str(),
            "cwd": context.cwd().to_string_lossy(),
        });
        for hook in self.hook_callbacks("input") {
            let mut event_value = json!({
                "type": "input",
                "text": text,
                "source": input_source(event.source),
            });
            if let Some(current_images) = &images {
                event_value
                    .as_object_mut()
                    .expect("input event is an object")
                    .insert("images".to_string(), input_images_value(current_images));
            }
            if let Some(behavior) = event.streaming_behavior {
                event_value
                    .as_object_mut()
                    .expect("input event is an object")
                    .insert(
                        "streamingBehavior".to_string(),
                        Value::String(input_streaming_behavior(behavior).to_string()),
                    );
            }
            let result = match self
                .invoke_hook(
                    "input",
                    hook,
                    context.plugin_context_handle(),
                    context_value.clone(),
                    event_value,
                    context.signal(),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    context.report_hook_error("input", error.to_string());
                    continue;
                }
            };
            match result.get("action").and_then(Value::as_str) {
                None | Some("continue") => {}
                Some("handled") => return Ok(InputPatch::Handled),
                Some("transform") => {
                    let Some(next_text) = result.get("text").and_then(Value::as_str) else {
                        context.report_hook_error("input", "transform result is missing text");
                        continue;
                    };
                    let next_images = match result.get("images") {
                        Some(value) if !value.is_null() => match parse_input_images(value) {
                            Ok(images) => Some(images),
                            Err(error) => {
                                context.report_hook_error("input", error);
                                continue;
                            }
                        },
                        _ => images.clone(),
                    };
                    text = next_text.to_string();
                    images = next_images;
                }
                Some(action) => {
                    context.report_hook_error("input", format!("invalid input action: {action}"));
                }
            }
        }
        Ok(if text == original.text && images == original.images {
            InputPatch::Continue
        } else {
            InputPatch::Transform { text, images }
        })
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        let original_system_prompt = event.system_prompt;
        let mut system_prompt = original_system_prompt.clone();
        let mut messages = Vec::new();
        let (prompt, images) = pi_prompt_input(&event.input_messages);
        let plugin_context = context.plugin_context_handle();
        let system_prompt_options = plugin_context
            .access_for_adapter()
            .and_then(|access| access.system_prompt_options(PluginContextScope::Command))
            .unwrap_or_else(|_| json!({ "cwd": context.cwd() }));
        for hook in self.hook_callbacks("before_agent_start") {
            let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
            let mut event_value = json!({
                "type": "before_agent_start",
                "prompt": prompt,
                "systemPrompt": system_prompt,
                "systemPromptOptions": system_prompt_options,
                "inputMessages": event.input_messages,
                "activeTools": event.active_tools,
                "providerId": event.provider_id.as_str(),
                "modelId": event.model_id.as_str(),
            });
            if !images.is_empty() {
                event_value
                    .as_object_mut()
                    .expect("before_agent_start event is an object")
                    .insert("images".to_string(), json!(images));
            }
            let result = match self
                .invoke_hook(
                    "before_agent_start",
                    hook,
                    callback_context.plugin_context_handle(),
                    agent_plugin_context_value(&callback_context),
                    event_value,
                    callback_context.signal(),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    callback_context.report_hook_error("before_agent_start", error.to_string());
                    continue;
                }
            };
            if let Some(replacement) = result.get("systemPrompt").and_then(Value::as_str) {
                system_prompt = replacement.to_string();
            }
            if let Some(message) = result.get("message") {
                match before_agent_start_message(message) {
                    Ok(message) => messages.push(message),
                    Err(error) => {
                        callback_context.report_hook_error("before_agent_start", error);
                    }
                }
            }
        }
        Ok(BeforeAgentStartPatch {
            system_prompt: (system_prompt != original_system_prompt).then_some(system_prompt),
            messages,
        })
    }

    async fn agent_start(
        &self,
        context: AgentPluginContext,
        _event: AgentStartEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks("agent_start", &context, json!({ "type": "agent_start" }))
            .await
    }

    async fn agent_end(
        &self,
        context: AgentPluginContext,
        event: AgentEndEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "agent_end",
            &context,
            json!({ "type": "agent_end", "messages": event.messages }),
        )
        .await
    }

    async fn agent_settled(
        &self,
        context: AgentPluginContext,
        _event: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "agent_settled",
            &context,
            json!({ "type": "agent_settled" }),
        )
        .await
    }

    async fn turn_start(
        &self,
        context: AgentPluginContext,
        event: TurnStartEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "turn_start",
            &context,
            json!({
                "type": "turn_start",
                "turnIndex": event.turn_index,
                "timestamp": event.timestamp_ms,
            }),
        )
        .await
    }

    async fn turn_end(
        &self,
        context: AgentPluginContext,
        event: TurnEndEvent,
    ) -> Result<(), PluginError> {
        self.notify_hooks(
            "turn_end",
            &context,
            json!({
                "type": "turn_end",
                "turnIndex": event.turn_index,
                "message": event.message,
                "toolResults": event.tool_results,
            }),
        )
        .await
    }

    async fn message_start(
        &self,
        context: AgentPluginContext,
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
        context: AgentPluginContext,
        event: MessageUpdateEvent,
    ) -> Result<(), PluginError> {
        self.notify_stream_hooks(&context, event).await
    }

    async fn message_end(
        &self,
        context: AgentPluginContext,
        event: MessageEndEvent,
    ) -> Result<MessageEndPatch, PluginError> {
        if let Some(active_stream) = self
            .active_streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(context.run_id())
        {
            self.lease.release_stream(&active_stream.id);
        }
        let original = event.message;
        let mut message = original.clone();
        for hook in self.hook_callbacks("message_end") {
            let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
            let result = match self
                .invoke_hook(
                    "message_end",
                    hook,
                    callback_context.plugin_context_handle(),
                    agent_plugin_context_value(&callback_context),
                    json!({ "type": "message_end", "message": message }),
                    callback_context.signal(),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    callback_context.report_hook_error("message_end", error.to_string());
                    continue;
                }
            };
            let Some(replacement) = result.get("message") else {
                continue;
            };
            let replacement = match message_end_replacement(replacement) {
                Ok(replacement) => replacement,
                Err(error) => {
                    callback_context.report_hook_error("message_end", error);
                    continue;
                }
            };
            if same_message_role(&message, &replacement) {
                message = replacement;
            } else {
                callback_context.report_hook_error(
                    "message_end",
                    "message_end handlers must return a message with the same role",
                );
            }
        }
        Ok(MessageEndPatch {
            message: (message != original).then_some(message),
        })
    }

    async fn tool_execution_start(
        &self,
        context: AgentPluginContext,
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
        context: AgentPluginContext,
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
        context: AgentPluginContext,
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
        context: AgentPluginContext,
        event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        let original = event.messages;
        let mut messages = original.clone();
        for hook in self.hook_callbacks("context") {
            let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
            let result = match self
                .invoke_hook(
                    "context",
                    hook,
                    callback_context.plugin_context_handle(),
                    agent_plugin_context_value(&callback_context),
                    json!({ "type": "context", "messages": messages }),
                    callback_context.signal(),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    callback_context.report_hook_error("context", error.to_string());
                    continue;
                }
            };
            if let Some(replacement) = result.get("messages") {
                match serde_json::from_value::<Vec<Message>>(replacement.clone()) {
                    Ok(replacement) => messages = replacement,
                    Err(error) => callback_context
                        .report_hook_error("context", format!("invalid messages result: {error}")),
                }
            }
        }
        Ok(ContextPatch {
            messages: (messages != original).then_some(messages),
        })
    }

    async fn tool_call(
        &self,
        context: AgentPluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        let original_arguments = event.validated_args;
        let mut arguments = original_arguments.clone();
        for hook in self.hook_callbacks("tool_call") {
            let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
            let result = self
                .invoke_hook(
                    "tool_call",
                    hook,
                    callback_context.plugin_context_handle(),
                    agent_plugin_context_value(&callback_context),
                    json!({
                        "type": "tool_call",
                        "toolCallId": event.tool_call.id.as_str(),
                        "toolName": event.tool_call.name,
                        "input": arguments,
                    }),
                    callback_context.signal(),
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
        context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        let mut result = event.result;
        let mut aggregate = JsToolResultPatch::default();
        for hook in self.hook_callbacks("tool_result") {
            let callback_context = context.for_adapter_plugin(hook.plugin_id.clone());
            let patch = match self
                .invoke_hook(
                    "tool_result",
                    hook,
                    callback_context.plugin_context_handle(),
                    agent_plugin_context_value(&callback_context),
                    json!({
                        "type": "tool_result",
                        "toolCallId": event.tool_call.id.as_str(),
                        "toolName": event.tool_call.name,
                        "input": event.validated_args,
                        "content": result.content,
                        "details": result.details,
                        "usage": result.usage,
                        "addedToolNames": result.added_tool_names,
                        "isError": result.is_error,
                    }),
                    callback_context.signal(),
                )
                .await
            {
                Ok(patch) => patch,
                Err(error) => {
                    callback_context.report_hook_error("tool_result", error.to_string());
                    continue;
                }
            };
            let parsed = match serde_json::from_value::<JsToolResultPatch>(patch) {
                Ok(parsed) => parsed,
                Err(error) => {
                    callback_context.report_hook_error(
                        "tool_result",
                        format!("invalid tool result patch: {error}"),
                    );
                    continue;
                }
            };
            parsed.clone().into_core().apply(&mut result);
            aggregate.merge(&parsed);
        }
        Ok(aggregate.into_core())
    }
}

fn tool_result_value(result: &ToolResult) -> Value {
    json!({
        "content": result.content,
        "details": result.details,
        "usage": result.usage,
        "addedToolNames": result.added_tool_names,
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
                    "context": { "cwd": context.cwd().to_string_lossy() },
                    "arguments": arguments,
                }),
                context.plugin_context_handle(),
                Some(context.signal()),
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

#[pi_core::provider_plugin]
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
            let result = match self
                .lease
                .invoke(
                    &hook.callback_id,
                    JsInvocationKind::ProviderHook,
                    json!({
                        "hook": "before_provider_request",
                        "context": {
                            "pluginId": context.plugin_id().as_str(),
                            "generation": context.generation(),
                            "providerId": context.provider_id().as_str(),
                            "modelId": context.model_id().as_str(),
                            "cwd": context.cwd().to_string_lossy(),
                        },
                        "event": {
                            "type": "before_provider_request",
                            "payload": payload,
                        },
                    }),
                    context.plugin_context_handle(),
                    Some(context.signal()),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let message = match error {
                        JsInvokeError::Aborted => "JavaScript hook was aborted".to_string(),
                        JsInvokeError::Callback(error) => error.to_string(),
                    };
                    context.report_hook_error("before_provider_request", message);
                    continue;
                }
            };
            if !result.is_null() {
                payload = result;
            }
        }
        Ok((payload != original).then_some(payload))
    }

    async fn before_provider_headers(
        &self,
        context: ProviderPluginContext,
        event: BeforeProviderHeadersEvent,
    ) -> Result<Option<std::collections::BTreeMap<String, Option<String>>>, PluginError> {
        let original = event.headers;
        let mut headers = original.clone();
        for hook in self
            .hooks
            .iter()
            .filter(|hook| hook.name == "before_provider_headers")
        {
            let result = match self
                .lease
                .invoke(
                    &hook.callback_id,
                    JsInvocationKind::ProviderHook,
                    json!({
                        "hook": "before_provider_headers",
                        "context": provider_hook_context(&context),
                        "event": {
                            "type": "before_provider_headers",
                            "headers": headers,
                        },
                    }),
                    context.plugin_context_handle(),
                    Some(context.signal()),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    report_provider_invoke_error(&context, "before_provider_headers", error);
                    continue;
                }
            };
            match serde_json::from_value(result) {
                Ok(replacement) => headers = replacement,
                Err(error) => context.report_hook_error(
                    "before_provider_headers",
                    format!("invalid JavaScript header result: {error}"),
                ),
            }
        }
        Ok((headers != original).then_some(headers))
    }

    async fn after_provider_response(
        &self,
        context: ProviderPluginContext,
        event: AfterProviderResponseEvent,
    ) -> Result<(), PluginError> {
        for hook in self
            .hooks
            .iter()
            .filter(|hook| hook.name == "after_provider_response")
        {
            if let Err(error) = self
                .lease
                .invoke(
                    &hook.callback_id,
                    JsInvocationKind::ProviderHook,
                    json!({
                        "hook": "after_provider_response",
                        "context": provider_hook_context(&context),
                        "event": {
                            "type": "after_provider_response",
                            "status": event.status,
                            "headers": event.headers,
                        },
                    }),
                    context.plugin_context_handle(),
                    Some(context.signal()),
                )
                .await
            {
                report_provider_invoke_error(&context, "after_provider_response", error);
            }
        }
        Ok(())
    }
}

fn provider_hook_context(context: &ProviderPluginContext) -> Value {
    json!({
        "pluginId": context.plugin_id().as_str(),
        "generation": context.generation(),
        "providerId": context.provider_id().as_str(),
        "modelId": context.model_id().as_str(),
        "cwd": context.cwd().to_string_lossy(),
    })
}

fn report_provider_invoke_error(
    context: &ProviderPluginContext,
    hook: &'static str,
    error: JsInvokeError,
) {
    let message = match error {
        JsInvokeError::Aborted => "JavaScript hook was aborted".to_string(),
        JsInvokeError::Callback(error) => error.to_string(),
    };
    context.report_hook_error(hook, message);
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
                    context.plugin_context_handle(),
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
    let identity = context.identity();
    json!({
        "pluginId": context.plugin_id().as_str(),
        "generation": context.generation(),
        "session": {
            "id": identity.id,
            "path": identity.path,
            "cwd": identity.cwd,
            "parentSessionId": identity.parent_session_id,
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

#[pi_session::session_plugin]
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
    prepare_callback_id: Option<String>,
    spec: ToolSpec,
    lease: Arc<JsGenerationLease>,
}

impl JsTool {
    fn new(manifest: JsToolManifest, lease: Arc<JsGenerationLease>) -> Self {
        Self {
            callback_id: manifest.callback_id,
            prepare_callback_id: manifest.prepare_callback_id,
            spec: ToolSpec {
                name: manifest.name,
                label: manifest.label,
                description: manifest.description,
                parameters: manifest.parameters,
                execution_mode: manifest.execution_mode,
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
    added_tool_names: Option<Vec<String>>,
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
    added_tool_names: Option<Vec<String>>,
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
            added_tool_names: self.added_tool_names,
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
        if next.added_tool_names.is_some() {
            self.added_tool_names.clone_from(&next.added_tool_names);
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
            added_tool_names: result.added_tool_names,
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

    async fn prepare_arguments(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> Result<Value, ToolError> {
        let Some(callback_id) = &self.prepare_callback_id else {
            return Ok(input);
        };
        self.lease
            .invoke(
                callback_id,
                JsInvocationKind::ToolPrepareArguments,
                json!({ "input": input }),
                context.plugin_context_handle(),
                None,
            )
            .await
            .map_err(|error| match error {
                JsInvokeError::Aborted => ToolError::Aborted,
                JsInvokeError::Callback(error) => ToolError::InvalidArguments(error.to_string()),
            })
    }

    async fn execute(
        &self,
        context: ToolContext,
        tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let response = self
            .lease
            .invoke_tool(
                &self.callback_id,
                json!({
                "context": {
                    "cwd": context.cwd().to_string_lossy(),
                    "toolCallId": tool_call_id.as_str(),
                },
                "input": input,
                }),
                context.plugin_context_handle(),
                context.signal(),
                updates,
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
