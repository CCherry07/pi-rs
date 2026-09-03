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
    ContextUsage, DirectCompletionRequest, EphemeralCompactionOptions, EphemeralSessionOutcome,
    EphemeralSessionRequest, EphemeralSessionStatus, ForkOptions, ForkPosition,
    IsolatedSessionHandle, IsolatedSessionId, IsolatedSessionOptions, IsolatedSessionOutcome,
    IsolatedSessionRequest, MessageDelivery, ModelsContext, ModelsContextAccess,
    NavigateTreeOptions, NewSessionOptions, NoticeLevel, PluginContext, PluginContextEpoch,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, PresentationMode, ReplacedSessionContext, ScopedModel, SendMessageOptions,
    SendUserMessageOptions, SessionContext, SessionContextAccess, SessionEntryKind,
    SessionEntryView, SessionExecutionOrigin, SessionReplacement, SessionSnapshot, UiContext,
    UiContextAccess, UiMultiSelectAction, UiMultiSelectOption, UiMultiSelectRequest,
    UiMultiSelectResponse, UnavailablePluginContext,
};
pub use provider::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    ProviderPlugin, ProviderPluginContext, ProviderPluginDriver, ProviderRegisterContext,
};
