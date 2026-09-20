#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
extern crate self as pi_plugin;
#[doc(hidden)]
pub use async_trait::async_trait as __plugin_async_trait;
pub use pi_core::{
    AbortError, AbortHandle, AbortSignal, AgentContext, AgentEvent, AssistantMessage,
    AssistantStream, AssistantStreamId, AssistantStreamView, ContentBlock, ContentMetadata,
    CustomMessage, CustomMessageContent, CustomMessageInput, DeferredHandle, ImageContent, Message,
    ModelCost, ModelCostTier, ModelId, ModelInput, ModelSelection, ModelSpec, PluginId, ProviderId,
    ResponseMetadata, ResponseMetadataPatch, RunId, StopReason, StreamEvent, TextContent,
    ThinkingBudgets, ThinkingContent, ThinkingLevel, ToolCall, ToolCallId, ToolExecutionMode,
    ToolResult, ToolResultMessage, ToolSpec, ToolUpdate, Usage, UsageCost, UserMessage,
    WorkspaceError, WorkspaceRoot, WorkspaceRootId, WorkspaceRootOwnership, WorkspaceSnapshot,
    WorkspaceSpec, session,
};
mod command;
mod error;
mod model_runtime;
mod plugin;
mod prepare;
mod provider;
mod registry;
mod tool;
pub use command::{Command, CommandContext, CommandError, CommandOutcome, CommandSpec};
pub use error::{CoreError, Result};
pub use model_runtime::{ModelRuntime, ProviderStatus};
pub use pi_plugin_macros::{plugin, provider_plugin};
pub use plugin::{
    AfterProviderResponseEvent, AgentEndEvent, AgentHook, AgentHookInterests, AgentPluginContext,
    AgentSettledEvent, AgentStartEvent, BeforeAgentStartEvent, BeforeAgentStartPatch,
    BeforeProviderHeadersEvent, BeforeProviderRequestEvent, CommandContextParts,
    CommandModelsContext, CommandSessionContext, CompactOptions, ContextEvent, ContextParts,
    ContextPatch, ContextUsage, DirectCompletionRequest, EphemeralCompactionOptions,
    EphemeralSessionOutcome, EphemeralSessionRequest, EphemeralSessionStatus, ForkOptions,
    ForkPosition, InputContext, InputEvent, InputPatch, InputSource, InputStreamingBehavior,
    IsolatedContextMode, IsolatedFollowUp, IsolatedFollowUpReceipt, IsolatedForkPoint,
    IsolatedMessageDelivery, IsolatedMessageReceipt, IsolatedSessionHandle, IsolatedSessionId,
    IsolatedSessionOptions, IsolatedSessionOutcome, IsolatedSessionRequest,
    IsolatedSessionTurnHandle, IsolatedSessionTurnId, MessageDelivery, MessageEndEvent,
    MessageEndPatch, MessageStartEvent, MessageUpdateEvent, ModelsContext, ModelsContextAccess,
    NavigateTreeOptions, NewSessionOptions, NoticeLevel, Plugin, PluginContext, PluginContextEpoch,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, PluginDiagnostic, PluginDriver, PluginError, PluginHook, PresentationMode,
    ProviderHook, ProviderPlugin, ProviderPluginContext, ProviderPluginDriver,
    ProviderRegisterContext, RegisterContext, ReplacedSessionContext, ScopedModel,
    SendMessageOptions, SendUserMessageOptions, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionBeforeForkEvent, SessionBeforeForkResult,
    SessionBeforeSwitchEvent, SessionBeforeSwitchResult, SessionBeforeTreeEvent,
    SessionBeforeTreeResult, SessionCompactEvent, SessionCompactFailedEvent, SessionContext,
    SessionContextAccess, SessionDispatchContext, SessionEntryKind, SessionEntryView,
    SessionExecutionOrigin, SessionForkPosition, SessionHook, SessionHookInterests,
    SessionIdentity, SessionInfoChangedEvent, SessionPluginContext, SessionReplacement,
    SessionShutdownEvent, SessionShutdownReason, SessionSnapshot, SessionStartEvent,
    SessionStartReason, SessionSwitchReason, SessionTreeEvent, SessionTreeSummary, ToolCallBlock,
    ToolCallEvent, ToolCallPatch, ToolExecutionEndEvent, ToolExecutionStartEvent,
    ToolExecutionUpdateEvent, ToolResultEvent, ToolResultPatch, TreePreparation, TurnEndEvent,
    TurnStartEvent, UiContext, UiContextAccess, UiMultiSelectAction, UiMultiSelectOption,
    UiMultiSelectRequest, UiMultiSelectResponse, UnavailablePluginContext,
};

pub use provider::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderRequest,
    ProviderStream, is_retryable_provider_error_message,
};
pub use registry::{FrozenRegistries, RegistriesBuilder};
pub use tool::{Tool, ToolContext, ToolError, ToolUpdateSink};

pub use prepare::{PluginFactory, PluginScope, PrepareContext, PrepareError, PrepareResult};

pub mod desktop;
/// Native export contracts; enable only when building or loading a native plugin.
#[cfg(feature = "native")]
#[allow(unsafe_code)]
pub mod native;

#[cfg(feature = "native")]
pub use pi_plugin_macros::{native_plugin, provider as native_provider};

/// Author-facing contracts and callbacks for static and native plugins.
pub mod prelude {
    pub use crate::*;
    pub use async_trait::async_trait;
    pub use pi_core::session::*;
    pub use serde_json::{Value, json};
}
