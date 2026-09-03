use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use pi_core::{
    AbortSignal, AgentContext, AgentEvent, AssistantMessage, FrozenRegistries, Message,
    PluginDriver, RunId, Tool, ToolCall, ToolCallEvent, ToolContext, ToolExecutionMode, ToolResult,
    ToolResultEvent, ToolResultMessage, ToolUpdateSink,
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
        context: ToolContext,
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
        context: Arc<AgentContext>,
        calls: Vec<ToolCall>,
        mode: ToolExecutionMode,
        signal: AbortSignal,
        events: Arc<dyn AgentEventSink>,
        timestamp_ms: i64,
    ) -> Result<ExecutedToolBatch, SchedulerError> {
        let force_sequential = mode == ToolExecutionMode::Sequential
            || calls.iter().any(|call| {
                self.registries
                    .tool(&call.name)
                    .is_some_and(|tool| tool.spec().execution_mode == ToolExecutionMode::Sequential)
            });

        if force_sequential {
            self.execute_sequential(
                run_id,
                assistant,
                &context,
                calls,
                signal,
                events,
                timestamp_ms,
            )
            .await
        } else {
            self.execute_parallel(
                run_id,
                assistant,
                &context,
                calls,
                signal,
                events,
                timestamp_ms,
            )
            .await
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_sequential(
        &self,
        run_id: &RunId,
        assistant: &AssistantMessage,
        context: &Arc<AgentContext>,
        calls: Vec<ToolCall>,
        signal: AbortSignal,
        events: Arc<dyn AgentEventSink>,
        timestamp_ms: i64,
    ) -> Result<ExecutedToolBatch, SchedulerError> {
        let mut messages = Vec::with_capacity(calls.len());
        let mut all_terminate = !calls.is_empty();

        for (source_index, call) in calls.into_iter().enumerate() {
            self.emit_start(&call, &signal, Arc::clone(&events)).await?;
            let prepared = self
                .prepare_call(run_id, assistant, context, source_index, call, &signal)
                .await;
            let finalized = self
                .execute_prepared(
                    run_id,
                    assistant,
                    context,
                    prepared,
                    &signal,
                    Arc::clone(&events),
                )
                .await;
            self.emit_end(&finalized, &signal, Arc::clone(&events))
                .await?;
            all_terminate &= finalized.result.terminate;
            messages.push(
                self.emit_result_message(&finalized, &signal, Arc::clone(&events), timestamp_ms)
                    .await?,
            );

            if signal.is_aborted() {
                break;
            }
        }

        Ok(ExecutedToolBatch {
            terminate: !messages.is_empty() && all_terminate,
            messages,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_parallel(
        &self,
        run_id: &RunId,
        assistant: &AssistantMessage,
        context: &Arc<AgentContext>,
        calls: Vec<ToolCall>,
        signal: AbortSignal,
        events: Arc<dyn AgentEventSink>,
        timestamp_ms: i64,
    ) -> Result<ExecutedToolBatch, SchedulerError> {
        let call_count = calls.len();
        let mut outcomes: Vec<Option<FinalizedToolCall>> = (0..call_count).map(|_| None).collect();
        let mut prepared_ready = Vec::with_capacity(call_count);

        // Pi prepares calls in source order. Immediate outcomes are finalized
        // during that pass, while executable calls do not start until every
        // preceding call has finished preparation.
        for (source_index, call) in calls.into_iter().enumerate() {
            self.emit_start(&call, &signal, Arc::clone(&events)).await?;
            let prepared = self
                .prepare_call(run_id, assistant, context, source_index, call, &signal)
                .await;
            match prepared {
                immediate @ PreparedToolCall::Immediate { .. } => {
                    let finalized = self
                        .execute_prepared(
                            run_id,
                            assistant,
                            context,
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
                ready @ PreparedToolCall::Ready { .. } => prepared_ready.push(ready),
            }

            if signal.is_aborted() {
                break;
            }
        }

        // FuturesUnordered preserves completion-order end events. The indexed
        // outcome slots below restore source order for transcript messages.
        let mut ready = FuturesUnordered::new();
        for prepared in prepared_ready {
            ready.push(self.execute_prepared(
                run_id,
                assistant,
                context,
                prepared,
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
        while let Some(finalized) = ready.next().await {
            self.emit_end(&finalized, &signal, Arc::clone(&events))
                .await?;
            let index = finalized.source_index;
            outcomes[index] = Some(finalized);
        }

        let mut messages = Vec::with_capacity(outcomes.len());
        let mut all_terminate = !outcomes.is_empty();
        for finalized in outcomes.into_iter().flatten() {
            all_terminate &= finalized.result.terminate;
            messages.push(
                self.emit_result_message(&finalized, &signal, Arc::clone(&events), timestamp_ms)
                    .await?,
            );
        }

        Ok(ExecutedToolBatch {
            messages,
            terminate: !call_count.eq(&0) && all_terminate,
        })
    }

    async fn emit_start(
        &self,
        call: &ToolCall,
        signal: &AbortSignal,
        events: Arc<dyn AgentEventSink>,
    ) -> Result<(), SchedulerError> {
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
            .map(|_| ())
            .map_err(|error| SchedulerError::Event(error.to_string()))
    }

    async fn emit_result_message(
        &self,
        finalized: &FinalizedToolCall,
        signal: &AbortSignal,
        events: Arc<dyn AgentEventSink>,
        timestamp_ms: i64,
    ) -> Result<ToolResultMessage, SchedulerError> {
        let message = ToolResultMessage {
            tool_call_id: finalized.call.id.clone(),
            tool_name: finalized.call.name.clone(),
            content: finalized.result.content.clone(),
            details: finalized.result.details.clone(),
            usage: finalized.result.usage.clone(),
            added_tool_names: finalized
                .result
                .added_tool_names
                .clone()
                .filter(|names| !names.is_empty()),
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
        let event = events
            .emit(
                AgentEvent::MessageEnd {
                    message: Message::tool_result(message),
                },
                signal.clone(),
            )
            .await
            .map_err(|error| SchedulerError::Event(error.to_string()))?;
        let AgentEvent::MessageEnd {
            message: Message::ToolResult(message),
        } = event
        else {
            return Err(SchedulerError::Event(
                "message_end changed a tool-result event role".to_string(),
            ));
        };
        Ok(Arc::unwrap_or_clone(message))
    }

    async fn prepare_call(
        &self,
        run_id: &RunId,
        assistant: &AssistantMessage,
        context: &Arc<AgentContext>,
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

        let tool_context = ToolContext::with_plugin_context(
            self.cwd.clone(),
            signal.child(),
            self.plugins.context_parts(),
        )
        .with_run_id(run_id.clone());
        let args = match tool
            .prepare_arguments(&tool_context, call.arguments.clone())
            .await
        {
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
            context: Arc::clone(context),
        };
        match self
            .plugins
            .tool_call(run_id, &self.cwd, signal, event)
            .await
        {
            Ok(patch) => {
                // Pi validates prepared provider arguments once, before the
                // hook. Hook-owned replacements go directly to execution.
                let patched_args = patch.arguments.unwrap_or(args);
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
                        context: tool_context,
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
        context: &Arc<AgentContext>,
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
                context,
                args,
            } => {
                let (updates, mut update_receiver) = ToolUpdateSink::channel();
                let execution = tool.execute(context, call.id.clone(), args.clone(), updates);
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
                        context: Arc::clone(context),
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
            added_tool_names: None,
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
            .map(|_| ())
            .map_err(|error| SchedulerError::Event(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use pi_core::{
        AbortHandle, AgentPlugin, AgentPluginContext, ModelId, PluginError, PluginId, ProviderId,
        RegisterContext, RegistriesBuilder, StopReason, ToolCallEvent, ToolError, ToolResultEvent,
        ToolResultPatch, ToolSpec, Usage,
    };

    use super::*;

    struct RecordingTool {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        added_tool_names: Option<Vec<String>>,
    }

    #[async_trait]
    impl Tool for RecordingTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.to_string(),
                label: self.name.to_string(),
                description: self.name.to_string(),
                parameters: serde_json::json!({"type": "object"}),
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            }
        }

        async fn execute(
            &self,
            _context: ToolContext,
            tool_call_id: pi_core::ToolCallId,
            _input: serde_json::Value,
            _updates: ToolUpdateSink,
        ) -> Result<ToolResult, ToolError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("execute:{tool_call_id}"));
            let mut result = ToolResult::text(self.name);
            result.added_tool_names.clone_from(&self.added_tool_names);
            Ok(result)
        }
    }

    struct RecordingPlugin {
        tools: Vec<Arc<dyn Tool>>,
        log: Arc<Mutex<Vec<String>>>,
        patched_added_tool_names: Option<Vec<String>>,
        observed_contexts: Option<Arc<Mutex<Vec<Arc<AgentContext>>>>>,
    }

    struct DelayedTool {
        name: &'static str,
        delay: Duration,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for DelayedTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.to_string(),
                label: self.name.to_string(),
                description: self.name.to_string(),
                parameters: serde_json::json!({"type": "object"}),
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            }
        }

        async fn execute(
            &self,
            _context: ToolContext,
            tool_call_id: pi_core::ToolCallId,
            _input: serde_json::Value,
            _updates: ToolUpdateSink,
        ) -> Result<ToolResult, ToolError> {
            tokio::time::sleep(self.delay).await;
            self.log
                .lock()
                .unwrap()
                .push(format!("execute:{tool_call_id}"));
            Ok(ToolResult::text(self.name))
        }
    }

    #[pi_core::agent_plugin]
    impl AgentPlugin for RecordingPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("recording")
        }

        fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
            for tool in &self.tools {
                context.register_tool(Arc::clone(tool))?;
            }
            Ok(())
        }

        async fn tool_call(
            &self,
            _context: AgentPluginContext,
            event: ToolCallEvent,
        ) -> Result<pi_core::ToolCallPatch, PluginError> {
            if let Some(observed) = &self.observed_contexts {
                observed.lock().unwrap().push(Arc::clone(&event.context));
            }
            self.log
                .lock()
                .unwrap()
                .push(format!("prepare:{}", event.tool_call.id));
            Ok(pi_core::ToolCallPatch::default())
        }

        async fn tool_result(
            &self,
            _context: AgentPluginContext,
            event: ToolResultEvent,
        ) -> Result<ToolResultPatch, PluginError> {
            if let Some(observed) = &self.observed_contexts {
                observed.lock().unwrap().push(Arc::clone(&event.context));
            }
            self.log
                .lock()
                .unwrap()
                .push(format!("finalize:{}", event.tool_call.id));
            Ok(ToolResultPatch {
                added_tool_names: self.patched_added_tool_names.clone(),
                ..ToolResultPatch::default()
            })
        }
    }

    struct RecordingEvents {
        log: Arc<Mutex<Vec<String>>>,
        abort_after_result: Option<pi_core::AbortHandle>,
    }

    #[async_trait]
    impl AgentEventSink for RecordingEvents {
        async fn emit(
            &self,
            event: AgentEvent,
            _signal: AbortSignal,
        ) -> Result<AgentEvent, crate::EventError> {
            let entry = match &event {
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    Some(format!("start:{tool_call_id}"))
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    Some(format!("end:{tool_call_id}"))
                }
                AgentEvent::MessageStart {
                    message: Message::ToolResult(message),
                } => Some(format!("message_start:{}", message.tool_call_id)),
                AgentEvent::MessageEnd {
                    message: Message::ToolResult(message),
                } => {
                    if let Some(handle) = &self.abort_after_result {
                        handle.abort();
                    }
                    Some(format!("message_end:{}", message.tool_call_id))
                }
                _ => None,
            };
            if let Some(entry) = entry {
                self.log.lock().unwrap().push(entry);
            }
            Ok(event)
        }
    }

    fn assistant() -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "test".to_string(),
            provider: ProviderId::new("test"),
            model: ModelId::new("test"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        }
    }

    fn context() -> Arc<AgentContext> {
        Arc::new(AgentContext {
            system_prompt: "test system prompt".to_string(),
            messages: vec![Message::assistant(assistant())],
            active_tools: vec!["one".to_string(), "two".to_string()],
        })
    }

    fn scheduler(log: Arc<Mutex<Vec<String>>>) -> ToolScheduler {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(RecordingTool {
                name: "one",
                log: Arc::clone(&log),
                added_tool_names: None,
            }),
            Arc::new(RecordingTool {
                name: "two",
                log: Arc::clone(&log),
                added_tool_names: None,
            }),
        ];
        let plugins: Vec<Arc<dyn AgentPlugin>> = vec![Arc::new(RecordingPlugin {
            tools,
            log: Arc::clone(&log),
            patched_added_tool_names: None,
            observed_contexts: None,
        })];
        let (plugins, registries) = RegistriesBuilder::new().register_plugins(plugins).unwrap();
        ToolScheduler::new(
            Arc::new(registries),
            Arc::new(plugins),
            PathBuf::from("."),
            8,
        )
    }

    #[tokio::test]
    async fn sequential_calls_complete_the_full_lifecycle_before_starting_the_next() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let scheduler = scheduler(Arc::clone(&log));
        let (_abort, signal) = AbortHandle::new();
        let events: Arc<dyn AgentEventSink> = Arc::new(RecordingEvents {
            log: Arc::clone(&log),
            abort_after_result: None,
        });

        scheduler
            .execute_batch(
                &RunId::new("run"),
                &assistant(),
                context(),
                vec![
                    ToolCall::new("call-1", "one", serde_json::json!({})),
                    ToolCall::new("call-2", "two", serde_json::json!({})),
                ],
                ToolExecutionMode::Sequential,
                signal,
                events,
                1,
            )
            .await
            .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            [
                "start:call-1",
                "prepare:call-1",
                "execute:call-1",
                "finalize:call-1",
                "end:call-1",
                "message_start:call-1",
                "message_end:call-1",
                "start:call-2",
                "prepare:call-2",
                "execute:call-2",
                "finalize:call-2",
                "end:call-2",
                "message_start:call-2",
                "message_end:call-2",
            ]
        );
    }

    #[tokio::test]
    async fn sequential_abort_after_a_result_does_not_start_remaining_calls() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let scheduler = scheduler(Arc::clone(&log));
        let (abort, signal) = AbortHandle::new();
        let events: Arc<dyn AgentEventSink> = Arc::new(RecordingEvents {
            log: Arc::clone(&log),
            abort_after_result: Some(abort),
        });

        let outcome = scheduler
            .execute_batch(
                &RunId::new("run"),
                &assistant(),
                context(),
                vec![
                    ToolCall::new("call-1", "one", serde_json::json!({})),
                    ToolCall::new("call-2", "two", serde_json::json!({})),
                ],
                ToolExecutionMode::Sequential,
                signal,
                events,
                1,
            )
            .await
            .unwrap();

        assert_eq!(outcome.messages.len(), 1);
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .all(|entry| !entry.ends_with("call-2"))
        );
    }

    #[tokio::test]
    async fn added_tool_names_and_agent_context_flow_through_result_hooks() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let observed_contexts = Arc::new(Mutex::new(Vec::new()));
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(RecordingTool {
            name: "one",
            log: Arc::clone(&log),
            added_tool_names: Some(vec!["from-tool".to_string()]),
        })];
        let plugins: Vec<Arc<dyn AgentPlugin>> = vec![Arc::new(RecordingPlugin {
            tools,
            log: Arc::clone(&log),
            patched_added_tool_names: Some(vec![
                "dynamic-one".to_string(),
                "dynamic-two".to_string(),
            ]),
            observed_contexts: Some(Arc::clone(&observed_contexts)),
        })];
        let (plugins, registries) = RegistriesBuilder::new().register_plugins(plugins).unwrap();
        let scheduler = ToolScheduler::new(
            Arc::new(registries),
            Arc::new(plugins),
            PathBuf::from("."),
            8,
        );
        let (_abort, signal) = AbortHandle::new();
        let events: Arc<dyn AgentEventSink> = Arc::new(RecordingEvents {
            log,
            abort_after_result: None,
        });

        let agent_context = context();
        let outcome = scheduler
            .execute_batch(
                &RunId::new("run"),
                &assistant(),
                Arc::clone(&agent_context),
                vec![ToolCall::new("call-1", "one", serde_json::json!({}))],
                ToolExecutionMode::Sequential,
                signal,
                events,
                1,
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.messages[0].added_tool_names.as_deref(),
            Some(["dynamic-one".to_string(), "dynamic-two".to_string()].as_slice())
        );
        let observed_contexts = observed_contexts.lock().unwrap();
        assert_eq!(observed_contexts.len(), 2);
        assert!(Arc::ptr_eq(&observed_contexts[0], &agent_context));
        assert!(Arc::ptr_eq(&observed_contexts[1], &agent_context));
        assert_eq!(observed_contexts[0].system_prompt, "test system prompt");
        assert!(observed_contexts[0].messages.last().unwrap().is_assistant());
        assert_eq!(observed_contexts[0].active_tools, ["one", "two"]);
    }

    #[tokio::test]
    async fn parallel_calls_end_in_completion_order_and_emit_results_in_source_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(DelayedTool {
                name: "one",
                delay: Duration::from_millis(30),
                log: Arc::clone(&log),
            }),
            Arc::new(DelayedTool {
                name: "two",
                delay: Duration::from_millis(1),
                log: Arc::clone(&log),
            }),
        ];
        let plugins: Vec<Arc<dyn AgentPlugin>> = vec![Arc::new(RecordingPlugin {
            tools,
            log: Arc::clone(&log),
            patched_added_tool_names: None,
            observed_contexts: None,
        })];
        let (plugins, registries) = RegistriesBuilder::new().register_plugins(plugins).unwrap();
        let scheduler = ToolScheduler::new(
            Arc::new(registries),
            Arc::new(plugins),
            PathBuf::from("."),
            8,
        );
        let (_abort, signal) = AbortHandle::new();
        let events: Arc<dyn AgentEventSink> = Arc::new(RecordingEvents {
            log: Arc::clone(&log),
            abort_after_result: None,
        });

        let outcome = scheduler
            .execute_batch(
                &RunId::new("run"),
                &assistant(),
                context(),
                vec![
                    ToolCall::new("call-1", "one", serde_json::json!({})),
                    ToolCall::new("call-2", "two", serde_json::json!({})),
                ],
                ToolExecutionMode::Parallel,
                signal,
                events,
                1,
            )
            .await
            .unwrap();

        assert_eq!(outcome.messages[0].tool_call_id.as_str(), "call-1");
        assert_eq!(outcome.messages[1].tool_call_id.as_str(), "call-2");
        assert_eq!(
            *log.lock().unwrap(),
            [
                "start:call-1",
                "prepare:call-1",
                "start:call-2",
                "prepare:call-2",
                "execute:call-2",
                "finalize:call-2",
                "end:call-2",
                "execute:call-1",
                "finalize:call-1",
                "end:call-1",
                "message_start:call-1",
                "message_end:call-1",
                "message_start:call-2",
                "message_end:call-2",
            ]
        );
    }
}
