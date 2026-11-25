#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
extern crate self as pi_plugin;
#[doc(hidden)]
pub use async_trait::async_trait as __plugin_async_trait;
pub use pi_core::{
    AbortError, AbortHandle, AbortSignal, AgentContext, AgentEvent, AnyModelSpec, AssistantMessage,
    AssistantStream, AssistantStreamId, AssistantStreamView, ClassifierAnswer,
    ClassifierBoolCriteria, ClassifierContext, ClassifierModelSpec, ClassifierQuestion,
    ClassifierResult, ConstrainedSampling, ContentBlock, ContentMetadata, ContextBudget,
    ContextCategory, ContextCategoryUsage, ContextRequestUsage, ContextUsageItem, CustomMessage,
    CustomMessageContent, CustomMessageInput, DeferredHandle, GrammarFormat, ImageContent,
    ImageModelSpec, ImagesContext, ImagesResult, Message, ModelCallStopReason, ModelCost,
    ModelCostTier, ModelId, ModelInput, ModelSelection, ModelSpec, ModelType, PluginId,
    PromptContribution, ProviderId, ResponseMetadata, ResponseMetadataPatch, RunId, StopReason,
    StreamEvent, SystemMessage, TextContent, ThinkingBudgets, ThinkingContent, ThinkingLevel,
    ToolCall, ToolCallId, ToolExecutionMode, ToolResult, ToolResultMessage, ToolSchemaStrict,
    ToolSpec, ToolUpdate, Usage, UsageCost, UserMessage, WorkspaceError, WorkspaceRoot,
    WorkspaceRootId, WorkspaceRootOwnership, WorkspaceSnapshot, WorkspaceSpec, session,
};
mod command;
mod error;
mod model_runtime;
mod plugin;
mod prepare;
mod prompt;
mod provider;
mod registry;
mod tool;
pub use command::{
    Command, CommandCatalogEntry, CommandCompletions, CommandContext, CommandError,
    CommandMetadata, CommandOutcome, CommandSource, CommandSourceInfo, CommandSourceOrigin,
    CommandSourceScope, CommandSpec,
};
pub use error::{CoreError, Result};
pub use model_runtime::{ModelRuntime, ProviderStatus};
pub use pi_plugin_macros::{plugin, provider_plugin};
pub use plugin::{
    AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent, AgentEndEvent,
    AgentHook, AgentHookInterests, AgentPluginContext, AgentSettledEvent, AgentStartEvent,
    BashOutputSink, BeforeAgentStartEvent, BeforeAgentStartPatch, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, BoundaryCompaction, BoundaryContextPreview, BoundaryResult,
    BoundaryState, CommandContextParts, CommandModelsContext, CommandSessionContext,
    CompactOptions, ContextEvent, ContextHookBatch, ContextParts, ContextPatch, ContextUsage,
    DirectCompletionRequest, EphemeralCompactionOptions, EphemeralSessionOutcome,
    EphemeralSessionRequest, EphemeralSessionStatus, ForkOptions, ForkPosition, InputContext,
    InputEvent, InputPatch, InputSource, InputStreamingBehavior, IsolatedContextMode,
    IsolatedFollowUp, IsolatedFollowUpReceipt, IsolatedForkPoint, IsolatedMessageDelivery,
    IsolatedMessageReceipt, IsolatedSessionHandle, IsolatedSessionId, IsolatedSessionOptions,
    IsolatedSessionOutcome, IsolatedSessionRequest, IsolatedSessionTurnHandle,
    IsolatedSessionTurnId, MessageDelivery, MessageEndEvent, MessageEndPatch, MessageStartEvent,
    MessageUpdateEvent, ModelSelectEvent, ModelSelectSource, ModelsContext, ModelsContextAccess,
    NavigateTreeOptions, NewSessionOptions, NoticeLevel, Plugin, PluginContext, PluginContextEpoch,
    PluginContextError, PluginContextHandle, PluginContextReplacement, PluginContextResult,
    PluginContextScope, PluginDiagnostic, PluginDiagnosticObserver, PluginDiagnosticSubscription,
    PluginDriver, PluginError, PluginHook, PresentationMode, ProviderHook, ProviderPlugin,
    ProviderPluginContext, ProviderPluginDriver, ProviderRegisterContext, RegisterContext,
    ReplacedSessionContext, ScopedModel, SendMessageOptions, SendUserMessageOptions,
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionBoundaryDraft, SessionBoundaryEvent,
    SessionCompactEvent, SessionCompactFailedEvent, SessionContext, SessionContextAccess,
    SessionDispatchContext, SessionEntryKind, SessionEntryView, SessionExecutionOrigin,
    SessionForkPosition, SessionHook, SessionHookInterests, SessionIdentity,
    SessionInfoChangedEvent, SessionPluginContext, SessionReplacement, SessionShutdownEvent,
    SessionShutdownReason, SessionSnapshot, SessionStartEvent, SessionStartReason,
    SessionSwitchReason, SessionTreeEvent, SessionTreeSummary, ThinkingLevelSelectEvent,
    ToolCallBlock, ToolCallEvent, ToolCallPatch, ToolExecutionEndEvent, ToolExecutionStartEvent,
    ToolExecutionUpdateEvent, ToolResultEvent, ToolResultPatch, TreePreparation, TurnEndEvent,
    TurnStartEvent, UiAppAction, UiCommand, UiCompletion, UiContext, UiContextAccess,
    UiDialogRequest, UiDialogResponse, UiFocusTarget, UiHostOperation, UiHostRequest,
    UiInteraction, UiMouseButton, UiMouseEvent, UiMouseEventKind, UiMouseResult,
    UiMultiSelectAction, UiMultiSelectOption, UiMultiSelectRequest, UiMultiSelectResponse, UiNode,
    UiNodeKind, UiNotification, UiOverlayAnchor, UiOverlayMargin, UiOverlayOptions,
    UiOverlayPosition, UiProductCommand, UiRegion, UiRegionBounds, UiRegionPlacement,
    UiRegionSnapshot, UiShortcut, UiSurfaceArea, UiSurfaceCommand, UiSurfaceEvent, UiSurfaceFrame,
    UiSurfaceSnapshot, UiTone, UiTranscriptItem, UiTranscriptRender, UiWidgetPlacement,
    UnavailablePluginContext, UserBashEvent, UserBashOperations, UserBashOverride, UserBashRequest,
    UserBashResult,
};
pub use prompt::{RenderedSystemPrompt, RunSystemPrompt, SystemPromptResolver};

pub use provider::{
    CacheRetention, Provider, ProviderAvailability, ProviderCallContext, ProviderError,
    ProviderRequest, ProviderStream, is_retryable_provider_error_message,
};
pub use registry::{FrozenRegistries, RegistriesBuilder};
pub use tool::{
    NestedToolExecutor, NestedToolResult, PreparedToolLoadout, Tool, ToolContext, ToolDiscovery,
    ToolError, ToolExposure, ToolInfo, ToolLoadout, ToolLoadoutChanges, ToolNamespace, ToolOrigin,
    ToolUpdateSink,
};

pub use prepare::{PrepareContext, PrepareError, PrepareResult};

pub mod ui;
pub use ui::{UiComponent, UiComponentUpdate, UiComponentWaker};
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
