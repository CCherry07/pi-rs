#![forbid(unsafe_code)]

mod agent;
mod agent_loop;
mod event_dispatcher;
mod pending_queue;
mod stream_assembler;
mod tool_scheduler;

pub use agent::{
    Agent, AgentConfigurationPatch, AgentOptions, AgentRestoreState, AgentRuntime,
    AgentStateSnapshot, PromptInput, SubscriptionId,
};
pub use agent_loop::{
    AgentContext, AgentLoopConfig, AgentLoopOutcome, AgentLoopServices, AgentLoopStop,
    AgentMessageQueues, NoopMessageQueues, run_agent_loop, run_agent_loop_continue,
};
pub use event_dispatcher::{AgentEventListener, AgentEventSink, EventError};
pub use pending_queue::{PendingMessageQueue, QueueMode};
pub use stream_assembler::{AssemblerError, StreamAssembler, StreamUpdate};
pub use tool_scheduler::{ExecutedToolBatch, ToolScheduler};
