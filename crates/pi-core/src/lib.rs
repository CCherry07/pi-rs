#![forbid(unsafe_code)]
mod abort;
mod agent_context;
mod assistant_stream;
mod context_usage;
mod events;
mod ids;
pub mod json;
mod message;
mod model;
mod model_operations;
pub mod session;
mod stream;
mod tool;
mod usage;
mod workspace;
pub use abort::{AbortError, AbortHandle, AbortSignal};
pub use agent_context::AgentContext;
pub use assistant_stream::{AssistantStream, AssistantStreamId, AssistantStreamView};
pub use context_usage::{
    ContextBudget, ContextCategory, ContextCategoryUsage, ContextRequestUsage, ContextUsageItem,
    PromptContribution,
};
pub use events::AgentEvent;
pub use ids::{ModelId, PluginId, ProviderId, RunId, ToolCallId};
pub use message::{
    AssistantMessage, ContentBlock, CustomMessage, CustomMessageContent, CustomMessageInput,
    DeferredHandle, ImageContent, Message, SystemMessage, TextContent, ThinkingContent, ToolCall,
    ToolResultMessage, UserMessage,
};
pub use model::{
    ModelCost, ModelCostTier, ModelImageInputLimits, ModelImageResizeOptions, ModelInput,
    ModelInputLimits, ModelPromptCache, ModelSamplingParamsByThinkingLevel, ModelSelection,
    ModelSpec, ResponseMetadata, StopReason, ThinkingBudgets, ThinkingLevel,
};
pub use model_operations::{
    AnyModelSpec, ClassifierAnswer, ClassifierAnswers, ClassifierBoolCriteria, ClassifierContext,
    ClassifierModelSpec, ClassifierQuestion, ClassifierResult, ImageModelSpec, ImagesContext,
    ImagesResult, ModelCallStopReason, ModelType,
};
pub use stream::{ContentMetadata, ResponseMetadataPatch, StreamEvent};
pub use tool::{
    ConstrainedSampling, GrammarFormat, ToolDeclaration, ToolExecutionMode, ToolReference,
    ToolResult, ToolSchemaStrict, ToolSpec, ToolUpdate,
};
pub use usage::{Usage, UsageCost};
pub use workspace::{
    WorkspaceError, WorkspaceRoot, WorkspaceRootId, WorkspaceRootOwnership, WorkspaceSnapshot,
    WorkspaceSpec,
};
