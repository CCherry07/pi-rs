//! Plugin contracts, drivers, and generation-bound product capabilities.

mod agent;
mod capabilities;
mod provider;

pub use agent::{
    AgentEndEvent, AgentHook, AgentHookInterests, AgentPlugin, AgentPluginContext,
    AgentSettledEvent, AgentStartEvent, BeforeAgentStartEvent, BeforeAgentStartPatch, ContextEvent,
    ContextPatch, InputContext, InputEvent, InputPatch, InputSource, InputStreamingBehavior,
    MessageEndEvent, MessageEndPatch, MessageStartEvent, MessageUpdateEvent, PluginDiagnostic,
    PluginDriver, PluginError, RegisterContext, ToolCallBlock, ToolCallEvent, ToolCallPatch,
    ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResultEvent,
    ToolResultPatch, TurnEndEvent, TurnStartEvent,
};
pub use capabilities::{
    CommandContextParts, CommandModelsContext, CommandSessionContext, CompactOptions, ContextParts,
    ContextUsage, ForkOptions, ForkPosition, MessageDelivery, ModelsContext, ModelsContextAccess,
    NavigateTreeOptions, NewSessionOptions, NoticeLevel, PluginContext, PluginContextEpoch,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, PresentationMode, ReplacedSessionContext, ScopedModel, SendMessageOptions,
    SendUserMessageOptions, SessionContext, SessionContextAccess, SessionEntryKind,
    SessionEntryView, SessionReplacement, SessionSnapshot, UiContext, UiContextAccess,
    UnavailablePluginContext,
};
pub use provider::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    ProviderPlugin, ProviderPluginContext, ProviderPluginDriver, ProviderRegisterContext,
};
