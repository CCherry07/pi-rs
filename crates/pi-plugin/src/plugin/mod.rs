//! Plugin contracts, drivers, and generation-bound product capabilities.

// Keep typed hooks, diagnostic names, and their wire representation in one declaration.
macro_rules! define_hooks {
    ($(#[$meta:meta])* $kind:ident { $($variant:ident => $name:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[repr(u8)]
        pub enum $kind {
            $(#[serde(rename = $name)] $variant,)*
        }

        impl $kind {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)* _ => None }
            }
        }
    };
}

mod agent;
mod capabilities;
mod diagnostics;
mod provider;

pub use agent::{
    AgentEndEvent, AgentHook, AgentHookInterests, AgentPluginContext, AgentSettledEvent,
    AgentStartEvent, BeforeAgentStartEvent, BeforeAgentStartPatch, ContextEvent, ContextPatch,
    InputContext, InputEvent, InputPatch, InputSource, InputStreamingBehavior, MessageEndEvent,
    MessageEndPatch, MessageStartEvent, MessageUpdateEvent, Plugin, PluginDriver, RegisterContext,
    ToolCallBlock, ToolCallEvent, ToolCallPatch, ToolExecutionEndEvent, ToolExecutionStartEvent,
    ToolExecutionUpdateEvent, ToolResultEvent, ToolResultPatch, TurnEndEvent, TurnStartEvent,
};
pub use capabilities::{
    CommandContextParts, CommandModelsContext, CommandSessionContext, CompactOptions, ContextParts,
    ContextUsage, DirectCompletionRequest, EphemeralCompactionOptions, EphemeralSessionOutcome,
    EphemeralSessionRequest, EphemeralSessionStatus, ForkOptions, ForkPosition,
    IsolatedContextMode, IsolatedFollowUp, IsolatedFollowUpReceipt, IsolatedForkPoint,
    IsolatedMessageDelivery, IsolatedMessageReceipt, IsolatedSessionHandle, IsolatedSessionId,
    IsolatedSessionOptions, IsolatedSessionOutcome, IsolatedSessionRequest,
    IsolatedSessionTurnHandle, IsolatedSessionTurnId, MessageDelivery, ModelsContext,
    ModelsContextAccess, NavigateTreeOptions, NewSessionOptions, NoticeLevel, PluginContext,
    PluginContextEpoch, PluginContextError, PluginContextHandle, PluginContextReplacement,
    PluginContextResult, PluginContextScope, PresentationMode, ReplacedSessionContext, ScopedModel,
    SendMessageOptions, SendUserMessageOptions, SessionContext, SessionContextAccess,
    SessionEntryKind, SessionEntryView, SessionExecutionOrigin, SessionReplacement,
    SessionSnapshot, UiContext, UiContextAccess, UiMultiSelectAction, UiMultiSelectOption,
    UiMultiSelectRequest, UiMultiSelectResponse, UnavailablePluginContext,
};
pub use provider::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    ProviderPlugin, ProviderPluginContext, ProviderPluginDriver, ProviderRegisterContext,
};

mod session;
mod session_dispatch;
pub use session::*;

pub use diagnostics::{PluginDiagnostic, PluginError, PluginHook, ProviderHook};
