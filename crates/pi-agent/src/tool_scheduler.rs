use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use pi_core::{
    AbortSignal, AgentEvent, AssistantMessage, FrozenRegistries, Message, PluginDriver, RunId,
    Tool, ToolCall, ToolCallEvent, ToolContext, ToolExecutionMode, ToolResult, ToolResultEvent,
    ToolResultMessage, ToolUpdateSink,
};

use crate::AgentEventSink;

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("event dispatch failed: {0}")]
    Event(String),
}

pub struct ExecutedToolBatch {
    pub messages: Vec<ToolResultMessage>,
    pub terminate: bool,
}

enum PreparedToolCall {
    Ready {
        source_index: usize,
        call: ToolCall,
        tool: Arc<dyn Tool>,
        args: serde_json::Value,
    },
    Immediate {
        source_index: usize,
        call: ToolCall,
        result: ToolResult,
    },
}

struct FinalizedToolCall {
    source_index: usize,
    call: ToolCall,
    result: ToolResult,
}

pub struct ToolScheduler {
    registries: Arc<FrozenRegistries>,
    plugins: Arc<PluginDriver>,
    cwd: PathBuf,
    max_parallel_tools: usize,
}

impl ToolScheduler {
    pub fn new(
        registries: Arc<FrozenRegistries>,
        plugins: Arc<PluginDriver>,
        cwd: PathBuf,
        max_parallel_tools: usize,
    ) -> Self {
        Self {
            registries,
            plugins,
            cwd,
            max_parallel_tools: max_parallel_tools.max(1),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_batch(
        &self,
        run_id: &RunId,
        assistant: &AssistantMessage,
        calls: Vec<ToolCall>,
        mode: ToolExecutionMode,
        signal: AbortSignal,
        events: Arc<dyn AgentEventSink>,
        timestamp_ms: i64,
    ) -> Result<ExecutedToolBatch, SchedulerError> {
        let mut prepared = Vec::with_capacity(calls.len());
        for (source_index, call) in calls.into_iter().enumerate() {
            events
                .emit(
                    AgentEvent::ToolExecutionStart {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        args: call.arguments.clone(),
                    },
                    signal.clone(),
                )
                .await
                .map_err(|error| SchedulerError::Event(error.to_string()))?;

            prepared.push(
                self.prepare_call(run_id, assistant, source_index, call, &signal)
                    .await,
            );
        }

        let force_sequential = mode == ToolExecutionMode::Sequential
            || prepared.iter().any(|prepared| match prepared {
                PreparedToolCall::Ready { tool, .. } => {
                    tool.spec().execution_mode == ToolExecutionMode::Sequential
                }
                PreparedToolCall::Immediate { .. } => false,
            });

        let mut outcomes: Vec<Option<FinalizedToolCall>> =
            (0..prepared.len()).map(|_| None).collect();
        if force_sequential {
            for item in prepared {
                let finalized = self
                    .execute_prepared(run_id, assistant, item, &signal, Arc::clone(&events))
                    .await;
                self.emit_end(&finalized, &signal, Arc::clone(&events))
                    .await?;
                let index = finalized.source_index;
                outcomes[index] = Some(finalized);
            }
        } else {
            let mut ready = FuturesUnordered::new();
            for item in prepared {
                match item {
                    immediate @ PreparedToolCall::Immediate { .. } => {
                        let finalized = self
                            .execute_prepared(
                                run_id,
                                assistant,
                                immediate,
                                &signal,
                                Arc::clone(&events),
                            )
                            .await;
                        self.emit_end(&finalized, &signal, Arc::clone(&events))
                            .await?;
                        let index = finalized.source_index;
                        outcomes[index] = Some(finalized);
                    }
                    ready_call @ PreparedToolCall::Ready { .. } => {
                        ready.push(self.execute_prepared(
                            run_id,
                            assistant,
                            ready_call,
                            &signal,
                            Arc::clone(&events),
                        ));
                        if ready.len() >= self.max_parallel_tools
                            && let Some(finalized) = ready.next().await
                        {
                            self.emit_end(&finalized, &signal, Arc::clone(&events))
                                .await?;
                            let index = finalized.source_index;
                            outcomes[index] = Some(finalized);
                        }
                    }
                }
            }
            while let Some(finalized) = ready.next().await {
                self.emit_end(&finalized, &signal, Arc::clone(&events))
                    .await?;
                let index = finalized.source_index;
                outcomes[index] = Some(finalized);
            }
        }

        let mut messages = Vec::with_capacity(outcomes.len());
        let mut all_terminate = !outcomes.is_empty();
        for finalized in outcomes.into_iter().flatten() {
            all_terminate &= finalized.result.terminate;
            let message = ToolResultMessage {
                tool_call_id: finalized.call.id,
                tool_name: finalized.call.name,
                content: finalized.result.content,
                details: finalized.result.details,
                usage: finalized.result.usage,
                added_tool_names: None,
                is_error: finalized.result.is_error,
                timestamp_ms,
            };
            events
                .emit(
                    AgentEvent::MessageStart {
                        message: Message::tool_result(message.clone()),
                    },
                    signal.clone(),
                )
                .await
                .map_err(|error| SchedulerError::Event(error.to_string()))?;
            events
                .emit(
                    AgentEvent::MessageEnd {
                        message: Message::tool_result(message.clone()),
                    },
                    signal.clone(),
                )
                .await
                .map_err(|error| SchedulerError::Event(error.to_string()))?;
            messages.push(message);
        }

        Ok(ExecutedToolBatch {
            messages,
            terminate: all_terminate,
        })
    }

    async fn prepare_call(
        &self,
        run_id: &RunId,
        assistant: &AssistantMessage,
        source_index: usize,
        call: ToolCall,
        signal: &AbortSignal,
    ) -> PreparedToolCall {
        if signal.is_aborted() {
            return PreparedToolCall::Immediate {
                source_index,
                call,
                result: ToolResult::error("Operation aborted"),
            };
        }

        let Some(tool) = self.registries.tool(&call.name) else {
            let name = call.name.clone();
            return PreparedToolCall::Immediate {
                source_index,
                call,
                result: ToolResult::error(format!("Tool {name} not found")),
            };
        };

        let args = match tool.prepare_arguments(call.arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return PreparedToolCall::Immediate {
                    source_index,
                    call,
                    result: ToolResult::error(error.to_string()),
                };
            }
        };
        if let Err(error) = tool.validate_arguments(&args) {
            return PreparedToolCall::Immediate {
                source_index,
                call,
                result: ToolResult::error(error.to_string()),
            };
        }

        let event = ToolCallEvent {
            assistant_message: assistant.clone(),
            tool_call: call.clone(),
            validated_args: args.clone(),
        };
        match self
            .plugins
            .tool_call(run_id, &self.cwd, signal, event)
            .await
        {
            Ok(patch) => {
                let patched_args = patch.arguments.unwrap_or(args);
                if let Err(error) = tool.validate_arguments(&patched_args) {
                    return PreparedToolCall::Immediate {
                        source_index,
                        call,
                        result: ToolResult::error(error.to_string()),
                    };
                }
                if let Some(block) = patch.block {
                    let mut result = ToolResult::error(block.reason);
                    result.terminate = block.terminate;
                    PreparedToolCall::Immediate {
                        source_index,
                        call,
                        result,
                    }
                } else {
                    PreparedToolCall::Ready {
                        source_index,
                        call,
                        tool,
                        args: patched_args,
                    }
                }
            }
            Err(error) => PreparedToolCall::Immediate {
                source_index,
                call,
                result: ToolResult::error(format!("Extension failed, blocking execution: {error}")),
            },
        }
    }

    async fn execute_prepared(
        &self,
        run_id: &RunId,
        assistant: &AssistantMessage,
        prepared: PreparedToolCall,
        signal: &AbortSignal,
        events: Arc<dyn AgentEventSink>,
    ) -> FinalizedToolCall {
        let (source_index, call, args, mut result, run_after_hook) = match prepared {
            PreparedToolCall::Immediate {
                source_index,
                call,
                result,
            } => (source_index, call, serde_json::json!({}), result, false),
            PreparedToolCall::Ready {
                source_index,
                call,
                tool,
                args,
            } => {
                let (updates, mut update_receiver) = ToolUpdateSink::channel();
                let tool_context = ToolContext {
                    cwd: self.cwd.clone(),
                    abort_signal: signal.child(),
                };
                let execution = tool.execute(tool_context, call.id.clone(), args.clone(), updates);
                tokio::pin!(execution);

                let result = loop {
                    tokio::select! {
                        biased;
                        () = signal.wait() => break ToolResult::error("Operation aborted"),
                        update = update_receiver.recv() => {
                            if let Some(update) = update {
                                Self::emit_update(&events, signal, &call, update).await;
                            }
                        }
                        executed = &mut execution => {
                            break executed.unwrap_or_else(|error| ToolResult::error(error.to_string()));
                        }
                    }
                };
                // Dropping the execution future closes its update sink. Drain
                // updates accepted before settlement; sends after settlement fail
                // and are ignored by ToolUpdateSink.
                while let Ok(update) = update_receiver.try_recv() {
                    Self::emit_update(&events, signal, &call, update).await;
                }
                (source_index, call, args, result, true)
            }
        };

        if run_after_hook {
            result = self
                .plugins
                .tool_result(
                    run_id,
                    &self.cwd,
                    signal,
                    ToolResultEvent {
                        assistant_message: assistant.clone(),
                        tool_call: call.clone(),
                        validated_args: args,
                        result,
                    },
                )
                .await;
        }

        FinalizedToolCall {
            source_index,
            call,
            result,
        }
    }

    async fn emit_update(
        events: &Arc<dyn AgentEventSink>,
        signal: &AbortSignal,
        call: &ToolCall,
        update: pi_core::ToolUpdate,
    ) {
        let partial_result = ToolResult {
            content: update.content,
            details: update.details,
            usage: None,
            is_error: false,
            terminate: false,
        };
        let _ = events
            .emit(
                AgentEvent::ToolExecutionUpdate {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    args: call.arguments.clone(),
                    partial_result,
                },
                signal.clone(),
            )
            .await;
    }

    async fn emit_end(
        &self,
        finalized: &FinalizedToolCall,
        signal: &AbortSignal,
        events: Arc<dyn AgentEventSink>,
    ) -> Result<(), SchedulerError> {
        events
            .emit(
                AgentEvent::ToolExecutionEnd {
                    tool_call_id: finalized.call.id.clone(),
                    tool_name: finalized.call.name.clone(),
                    result: finalized.result.clone(),
                    is_error: finalized.result.is_error,
                },
                signal.clone(),
            )
            .await
            .map_err(|error| SchedulerError::Event(error.to_string()))
    }
}
