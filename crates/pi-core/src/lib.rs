#![forbid(unsafe_code)]

extern crate self as pi_core;

#[doc(hidden)]
pub use async_trait::async_trait as __plugin_async_trait;

mod abort;
mod agent_context;
mod command;
mod error;
mod events;
mod ids;
mod message;
mod model;
mod model_runtime;
mod plugin;
mod provider;
mod provider_plugin;
mod registry;
mod stream;
mod tool;
mod usage;

pub use abort::{AbortError, AbortHandle, AbortSignal};
pub use agent_context::AgentContext;
pub use command::{Command, CommandContext, CommandError, CommandOutcome, CommandSpec};
pub use error::{CoreError, Result};
pub use events::{AgentEvent, AssistantMessageEvent};
pub use ids::{ModelId, PluginId, ProviderId, RunId, ToolCallId};
pub use message::{
    AssistantMessage, ContentBlock, CustomMessage, CustomMessageContent, DeferredHandle,
    ImageContent, Message, TextContent, ThinkingContent, ToolCall, ToolResultMessage, UserMessage,
};
pub use model::{
    ModelCost, ModelCostTier, ModelInput, ModelSpec, ResponseMetadata, StopReason, ThinkingLevel,
};
pub use model_runtime::{ModelRuntime, ProviderStatus};
pub use pi_plugin_macros::{agent_plugin, provider_plugin};
pub use plugin::{
    AgentEndEvent, AgentHook, AgentHookInterests, AgentPlugin, AgentSettledEvent, AgentStartEvent,
    BeforeAgentStartEvent, BeforeAgentStartPatch, ContextEvent, ContextPatch, InputContext,
    InputEvent, InputPatch, InputSource, InputStreamingBehavior, MessageEndEvent, MessageEndPatch,
    MessageStartEvent, MessageUpdateEvent, PluginContext, PluginDiagnostic, PluginDriver,
    PluginError, RegisterContext, ToolCallBlock, ToolCallEvent, ToolCallPatch,
    ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResultEvent,
    ToolResultPatch, TurnEndEvent, TurnStartEvent,
};
pub use provider::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderRequest,
    ProviderStream,
};
pub use provider_plugin::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    ProviderPlugin, ProviderPluginContext, ProviderPluginDriver, ProviderRegisterContext,
};
pub use registry::{FrozenRegistries, RegistriesBuilder};
pub use stream::{ContentMetadata, ResponseMetadataPatch, StreamEvent};
pub use tool::{
    Tool, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdate,
    ToolUpdateSink,
};
pub use usage::{Usage, UsageCost};
