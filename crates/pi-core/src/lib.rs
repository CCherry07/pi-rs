#![forbid(unsafe_code)]

extern crate self as pi_core;

#[doc(hidden)]
pub use async_trait::async_trait as __plugin_async_trait;

mod abort;
mod agent_context;
mod assistant_stream;
mod command;
mod error;
mod events;
mod ids;
mod message;
mod model;
mod model_runtime;
mod plugin;
mod provider;
mod registry;
mod stream;
mod tool;
mod usage;

pub use abort::{AbortError, AbortHandle, AbortSignal};
pub use agent_context::AgentContext;
pub use assistant_stream::{AssistantStream, AssistantStreamId, AssistantStreamView};
pub use command::{Command, CommandContext, CommandError, CommandOutcome, CommandSpec};
pub use error::{CoreError, Result};
pub use events::AgentEvent;
pub use ids::{ModelId, PluginId, ProviderId, RunId, ToolCallId};
pub use message::{
    AssistantMessage, ContentBlock, CustomMessage, CustomMessageContent, CustomMessageInput,
    DeferredHandle, ImageContent, Message, TextContent, ThinkingContent, ToolCall,
    ToolResultMessage, UserMessage,
};
pub use model::{
    ModelCost, ModelCostTier, ModelInput, ModelSpec, ResponseMetadata, StopReason, ThinkingBudgets,
    ThinkingLevel,
};
pub use model_runtime::{ModelRuntime, ProviderStatus};
pub use pi_plugin_macros::{agent_plugin, provider_plugin};
pub use plugin::{
    AfterProviderResponseEvent, AgentEndEvent, AgentHook, AgentHookInterests, AgentPlugin,
    AgentPluginContext, AgentSettledEvent, AgentStartEvent, BeforeAgentStartEvent,
    BeforeAgentStartPatch, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    CommandContextParts, CommandModelsContext, CommandSessionContext, CompactOptions, ContextEvent,
    ContextParts, ContextPatch, ContextUsage, ForkOptions, ForkPosition, InputContext, InputEvent,
    InputPatch, InputSource, InputStreamingBehavior, MessageDelivery, MessageEndEvent,
    MessageEndPatch, MessageStartEvent, MessageUpdateEvent, ModelsContext, ModelsContextAccess,
    NavigateTreeOptions, NewSessionOptions, NoticeLevel, PluginContext, PluginContextEpoch,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, PluginDiagnostic, PluginDriver, PluginError, PresentationMode,
    ProviderPlugin, ProviderPluginContext, ProviderPluginDriver, ProviderRegisterContext,
    RegisterContext, ReplacedSessionContext, ScopedModel, SendMessageOptions,
    SendUserMessageOptions, SessionContext, SessionContextAccess, SessionEntryKind,
    SessionEntryView, SessionReplacement, SessionSnapshot, ToolCallBlock, ToolCallEvent,
    ToolCallPatch, ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
    ToolResultEvent, ToolResultPatch, TurnEndEvent, TurnStartEvent, UiContext, UiContextAccess,
    UnavailablePluginContext,
};
pub use provider::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderRequest,
    ProviderStream, is_retryable_provider_error_message,
};
pub use registry::{FrozenRegistries, RegistriesBuilder};
pub use stream::{ContentMetadata, ResponseMetadataPatch, StreamEvent};
pub use tool::{
    Tool, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdate,
    ToolUpdateSink,
};
pub use usage::{Usage, UsageCost};
