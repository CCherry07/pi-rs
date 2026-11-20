#![forbid(unsafe_code)]

mod agent;
mod agent_loop;
mod context_usage;
mod event_dispatcher;
mod llm_callbacks;
mod namespace_prompt;
mod pending_queue;
mod stream_assembler;
mod tool_declarations;
mod tool_scheduler;

pub use agent::{
    Agent, AgentConfigurationPatch, AgentOptions, AgentRestoreState, AgentRuntime,
    AgentStateSnapshot, PreparedAgentConfiguration, PreparedAgentModelConfiguration, PromptInput,
    SubscriptionId, WeakAgent,
};
pub use agent_loop::{
    AgentLoopConfig, AgentLoopError, AgentLoopOutcome, AgentLoopServices, AgentLoopStop,
    AgentLoopTurnUpdate, AgentMessageQueues, AgentTurnContext, AgentTurnControl,
    AgentTurnControlError, FnTurnControl, NoopAgentTurnControl, NoopMessageQueues, run_agent_loop,
    run_agent_loop_continue,
};
pub use event_dispatcher::{AgentEventListener, AgentEventSink, EventError};
pub use llm_callbacks::{AssistantResponse, ConvertToLlm, StreamFn, TransformContext};
pub use pending_queue::{PendingMessageQueue, QueueDestination, QueueMode};
pub use pi_core::AgentContext;
pub use stream_assembler::{AssemblerError, StreamAssembler, StreamUpdate};
pub use tool_scheduler::{ExecutedToolBatch, ToolScheduler};

#[cfg(test)]
mod conformance_tests;

#[cfg(test)]
mod shared_snapshot_tests;
