use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{
    AbortSignal, AgentContext, AgentEvent, AssistantMessage, ContentBlock, FrozenRegistries,
    Message, ModelId, PluginDriver, ProviderCallContext, ProviderId, ProviderPluginDriver,
    ProviderRequest, RunId, StopReason, StreamEvent, TextContent, ThinkingBudgets, ThinkingLevel,
    ToolExecutionMode, ToolResult, ToolResultMessage, Usage,
};
use pi_telemetry::{
    ActiveSpan, AiOperation, AiRequestEnd, AiRequestSpan, AiRequestStart, AiStopReason, SpanStatus,
    TelemetryContext,
};

use crate::{AgentEventSink, StreamAssembler, ToolScheduler};

#[derive(Debug, thiserror::Error)]
pub enum AgentLoopError {
    #[error("agent context is empty")]
    EmptyContext,
    #[error("cannot continue from an assistant message")]
    CannotContinueFromAssistant,
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("stream assembly failed: {0}")]
    Assembly(String),
    #[error("event dispatch failed: {0}")]
    Event(String),
    #[error("tool scheduling failed: {0}")]
    ToolScheduling(String),
    #[error(transparent)]
    TurnControl(#[from] AgentTurnControlError),
    #[error("maximum tool iterations exceeded: {0}")]
    MaxToolIterations(usize),
}

#[derive(Debug, Clone)]
pub struct AgentLoopConfig {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub thinking_level: ThinkingLevel,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub block_images: bool,
    pub tool_execution: ToolExecutionMode,
    pub max_tool_iterations: usize,
    pub max_parallel_tools: usize,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLoopStop {
    Completed,
    Aborted,
    ProviderError,
    MaxToolIterations,
    TerminatedByTools,
}

#[derive(Debug, Clone)]
pub struct AgentLoopOutcome {
    pub new_messages: Vec<Message>,
    pub final_context: AgentContext,
    pub stop: AgentLoopStop,
}

/// Shared, immutable state captured after one assistant response and its tool
/// batch have completed.
///
/// Cloning this value or any field is O(1). The live loop uses copy-on-write,
/// so retaining a snapshot beyond the callback is safe but may make a later
/// transcript mutation clone the retained data.
#[derive(Debug, Clone)]
pub struct AgentTurnContext {
    pub message: Arc<AssistantMessage>,
    pub tool_results: Arc<Vec<ToolResultMessage>>,
    pub context: Arc<AgentContext>,
    pub new_messages: Arc<Vec<Message>>,
}

/// Run-local replacements applied before the next provider request.
#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub provider_id: Option<ProviderId>,
    pub model_id: Option<ModelId>,
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, thiserror::Error)]
#[error("turn control failed: {0}")]
pub struct AgentTurnControlError(pub String);

/// Run-local control seam invoked between a completed turn and queue polling.
#[async_trait]
pub trait AgentTurnControl: Send + Sync {
    async fn prepare_next_turn(
        &self,
        _context: AgentTurnContext,
        _signal: AbortSignal,
    ) -> Result<Option<AgentLoopTurnUpdate>, AgentTurnControlError> {
        Ok(None)
    }

    async fn should_stop_after_turn(
        &self,
        _context: AgentTurnContext,
        _signal: AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        Ok(false)
    }
}

pub struct NoopAgentTurnControl;

#[async_trait]
impl AgentTurnControl for NoopAgentTurnControl {}

type TurnControlFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, AgentTurnControlError>> + Send + 'static>>;
type PrepareNextTurnFn = dyn Fn(AgentTurnContext, AbortSignal) -> TurnControlFuture<Option<AgentLoopTurnUpdate>>
    + Send
    + Sync;
type ShouldStopAfterTurnFn =
    dyn Fn(AgentTurnContext, AbortSignal) -> TurnControlFuture<bool> + Send + Sync;

/// Closure adapter for [`AgentTurnControl`].
///
/// Hooks that are not configured retain the trait's no-op behavior. The
/// adapter owns future boxing so callers can provide ordinary async closures.
#[derive(Default)]
pub struct FnTurnControl {
    prepare_next_turn: Option<Box<PrepareNextTurnFn>>,
    should_stop_after_turn: Option<Box<ShouldStopAfterTurnFn>>,
}

impl FnTurnControl {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prepare_next_turn: None,
            should_stop_after_turn: None,
        }
    }

    #[must_use]
    pub fn with_prepare_next_turn<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(AgentTurnContext, AbortSignal) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<AgentLoopTurnUpdate>, AgentTurnControlError>>
            + Send
            + 'static,
    {
        self.prepare_next_turn = Some(Box::new(move |context, signal| {
            Box::pin(callback(context, signal))
        }));
        self
    }

    #[must_use]
    pub fn with_should_stop_after_turn<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(AgentTurnContext, AbortSignal) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool, AgentTurnControlError>> + Send + 'static,
    {
        self.should_stop_after_turn = Some(Box::new(move |context, signal| {
            Box::pin(callback(context, signal))
        }));
        self
    }
}

#[async_trait]
impl AgentTurnControl for FnTurnControl {
    async fn prepare_next_turn(
        &self,
        context: AgentTurnContext,
        signal: AbortSignal,
    ) -> Result<Option<AgentLoopTurnUpdate>, AgentTurnControlError> {
        let Some(callback) = &self.prepare_next_turn else {
            return Ok(None);
        };
        callback(context, signal).await
    }

    async fn should_stop_after_turn(
        &self,
        context: AgentTurnContext,
        signal: AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        let Some(callback) = &self.should_stop_after_turn else {
            return Ok(false);
        };
        callback(context, signal).await
    }
}

pub trait AgentMessageQueues: Send + Sync {
    fn drain_steering(&self) -> Vec<Message>;
    fn drain_follow_up(&self) -> Vec<Message>;
}

pub struct NoopMessageQueues;

#[derive(Clone)]
pub struct AgentLoopServices {
    pub generation: u64,
    pub registries: Arc<FrozenRegistries>,
    pub plugins: Arc<PluginDriver>,
    pub provider_plugins: Arc<ProviderPluginDriver>,
    pub queues: Arc<dyn AgentMessageQueues>,
    pub turn_control: Arc<dyn AgentTurnControl>,
    pub telemetry: TelemetryContext,
    pub events: Arc<dyn AgentEventSink>,
}

struct AssistantResponseServices {
    generation: u64,
    registries: Arc<FrozenRegistries>,
    plugins: Arc<PluginDriver>,
    provider_plugins: Arc<ProviderPluginDriver>,
    telemetry: TelemetryContext,
    events: Arc<dyn AgentEventSink>,
}

impl AgentMessageQueues for NoopMessageQueues {
    fn drain_steering(&self) -> Vec<Message> {
        Vec::new()
    }

    fn drain_follow_up(&self) -> Vec<Message> {
        Vec::new()
    }
}

pub async fn run_agent_loop(
    run_id: RunId,
    prompts: Vec<Message>,
    mut context: AgentContext,
    config: AgentLoopConfig,
    services: AgentLoopServices,
    signal: AbortSignal,
) -> Result<AgentLoopOutcome, AgentLoopError> {
    let AgentLoopServices {
        generation,
        registries,
        plugins,
        provider_plugins,
        queues,
        turn_control,
        telemetry,
        events,
    } = services;
    let mut new_messages = Vec::with_capacity(prompts.len());
    emit(&events, AgentEvent::AgentStart, &signal).await?;
    emit(&events, AgentEvent::TurnStart, &signal).await?;
    for prompt in prompts {
        emit(
            &events,
            AgentEvent::MessageStart {
                message: prompt.clone(),
            },
            &signal,
        )
        .await?;
        let prompt = emit_message_end(&events, &signal, prompt).await?;
        context.messages.push(prompt.clone());
        new_messages.push(prompt);
    }
    run_loop(
        run_id,
        context,
        new_messages,
        config,
        generation,
        registries,
        plugins,
        provider_plugins,
        queues,
        turn_control,
        telemetry,
        signal,
        events,
        true,
    )
    .await
}

pub async fn run_agent_loop_continue(
    run_id: RunId,
    context: AgentContext,
    config: AgentLoopConfig,
    services: AgentLoopServices,
    signal: AbortSignal,
) -> Result<AgentLoopOutcome, AgentLoopError> {
    let AgentLoopServices {
        generation,
        registries,
        plugins,
        provider_plugins,
        queues,
        turn_control,
        telemetry,
        events,
    } = services;
    if context.messages.is_empty() {
        return Err(AgentLoopError::EmptyContext);
    }
    if context.messages.last().is_some_and(Message::is_assistant) {
        return Err(AgentLoopError::CannotContinueFromAssistant);
    }
    emit(&events, AgentEvent::AgentStart, &signal).await?;
    emit(&events, AgentEvent::TurnStart, &signal).await?;
    run_loop(
        run_id,
        context,
        Vec::new(),
        config,
        generation,
        registries,
        plugins,
        provider_plugins,
        queues,
        turn_control,
        telemetry,
        signal,
        events,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    run_id: RunId,
    context: AgentContext,
    new_messages: Vec<Message>,
    mut config: AgentLoopConfig,
    generation: u64,
    registries: Arc<FrozenRegistries>,
    plugins: Arc<PluginDriver>,
    provider_plugins: Arc<ProviderPluginDriver>,
    queues: Arc<dyn AgentMessageQueues>,
    turn_control: Arc<dyn AgentTurnControl>,
    telemetry: TelemetryContext,
    signal: AbortSignal,
    events: Arc<dyn AgentEventSink>,
    mut first_turn: bool,
) -> Result<AgentLoopOutcome, AgentLoopError> {
    let mut context = Arc::new(context);
    let mut new_messages = Arc::new(new_messages);
    let scheduler = ToolScheduler::new(
        Arc::clone(&registries),
        Arc::clone(&plugins),
        config.cwd.clone(),
        config.max_parallel_tools,
    );
    let response_services = AssistantResponseServices {
        generation,
        registries: Arc::clone(&registries),
        plugins: Arc::clone(&plugins),
        provider_plugins,
        telemetry,
        events: Arc::clone(&events),
    };
    let mut pending = queues.drain_steering();
    let mut tool_iterations = 0usize;
    let mut stop = AgentLoopStop::Completed;
    let mut last_batch_terminated = false;

    'run: loop {
        let mut should_continue = true;
        while should_continue || !pending.is_empty() {
            if !first_turn {
                emit(&events, AgentEvent::TurnStart, &signal).await?;
            }
            first_turn = false;

            for message in std::mem::take(&mut pending) {
                emit(
                    &events,
                    AgentEvent::MessageStart {
                        message: message.clone(),
                    },
                    &signal,
                )
                .await?;
                let message = emit_message_end(&events, &signal, message).await?;
                Arc::make_mut(&mut context).messages.push(message.clone());
                Arc::make_mut(&mut new_messages).push(message);
            }

            // Pi enters the turn even when cancellation raced with queue
            // polling: already-drained steering is committed before the
            // aborted assistant closes the turn. Avoid returning early here,
            // otherwise those messages disappear from the observable run.
            if signal.is_aborted() {
                let assistant = provider_failure_message(
                    &config,
                    "operation aborted".to_string(),
                    StopReason::Aborted,
                );
                let assistant = emit_message_pair(&events, &signal, &assistant).await?;
                Arc::make_mut(&mut context)
                    .messages
                    .push(Message::assistant(assistant.clone()));
                Arc::make_mut(&mut new_messages).push(Message::assistant(assistant.clone()));
                emit(
                    &events,
                    AgentEvent::TurnEnd {
                        message: assistant,
                        tool_results: Vec::new(),
                    },
                    &signal,
                )
                .await?;
                stop = AgentLoopStop::Aborted;
                break 'run;
            }

            let assistant = stream_assistant_response(
                &run_id,
                Arc::make_mut(&mut context),
                &config,
                &response_services,
                signal.clone(),
            )
            .await?;
            Arc::make_mut(&mut new_messages).push(Message::assistant(assistant.clone()));

            if matches!(
                assistant.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) {
                emit(
                    &events,
                    AgentEvent::TurnEnd {
                        message: assistant.clone(),
                        tool_results: Vec::new(),
                    },
                    &signal,
                )
                .await?;
                stop = if assistant.stop_reason == StopReason::Aborted {
                    AgentLoopStop::Aborted
                } else {
                    AgentLoopStop::ProviderError
                };
                break 'run;
            }

            let calls = assistant.tool_calls();
            let mut tool_results = Vec::new();
            should_continue = false;
            if !calls.is_empty() {
                tool_iterations += 1;
                if tool_iterations > config.max_tool_iterations {
                    let limit = config.max_tool_iterations;
                    let batch = fail_unexecuted_tool_calls(
                        calls,
                        &signal,
                        Arc::clone(&events),
                        now_ms(),
                        move |call| {
                            format!(
                                "Tool call \"{}\" was not executed: the maximum of {limit} tool iterations was reached.",
                                call.name
                            )
                        },
                    )
                    .await?;
                    tool_results = batch.messages;
                    for result in &tool_results {
                        let message = Message::tool_result(result.clone());
                        Arc::make_mut(&mut context).messages.push(message.clone());
                        Arc::make_mut(&mut new_messages).push(message);
                    }
                    emit(
                        &events,
                        AgentEvent::TurnEnd {
                            message: assistant,
                            tool_results,
                        },
                        &signal,
                    )
                    .await?;
                    stop = AgentLoopStop::MaxToolIterations;
                    break 'run;
                }

                let batch = if assistant.stop_reason == StopReason::Length {
                    fail_truncated_tool_calls(calls, &signal, Arc::clone(&events), now_ms()).await?
                } else {
                    scheduler
                        .execute_batch(
                            &run_id,
                            &assistant,
                            Arc::clone(&context),
                            calls,
                            config.tool_execution,
                            signal.clone(),
                            Arc::clone(&events),
                            now_ms(),
                        )
                        .await
                        .map_err(|error| AgentLoopError::ToolScheduling(error.to_string()))?
                };
                should_continue = !batch.terminate;
                last_batch_terminated = batch.terminate;
                tool_results = batch.messages;
                for result in &tool_results {
                    let message = Message::tool_result(result.clone());
                    Arc::make_mut(&mut context).messages.push(message.clone());
                    Arc::make_mut(&mut new_messages).push(message);
                }
            }

            emit(
                &events,
                AgentEvent::TurnEnd {
                    message: assistant.clone(),
                    tool_results: tool_results.clone(),
                },
                &signal,
            )
            .await?;

            let message = Arc::new(assistant);
            let tool_results = Arc::new(tool_results);

            if let Some(update) = turn_control
                .prepare_next_turn(
                    AgentTurnContext {
                        message: Arc::clone(&message),
                        tool_results: Arc::clone(&tool_results),
                        context: Arc::clone(&context),
                        new_messages: Arc::clone(&new_messages),
                    },
                    signal.clone(),
                )
                .await?
            {
                if let Some(next_context) = update.context {
                    context = Arc::new(next_context);
                }
                if let Some(provider_id) = update.provider_id {
                    config.provider_id = provider_id;
                }
                if let Some(model_id) = update.model_id {
                    config.model_id = model_id;
                }
                if let Some(thinking_level) = update.thinking_level {
                    config.thinking_level = thinking_level;
                }
            }

            if turn_control
                .should_stop_after_turn(
                    AgentTurnContext {
                        message,
                        tool_results,
                        context: Arc::clone(&context),
                        new_messages: Arc::clone(&new_messages),
                    },
                    signal.clone(),
                )
                .await?
            {
                break 'run;
            }

            // Steering is polled after every completed turn, including a final
            // text-only turn and a terminating tool batch. This matches Pi's
            // contract: steering may keep an otherwise settled run alive.
            pending = queues.drain_steering();
        }

        if stop != AgentLoopStop::Completed {
            break;
        }
        let follow_up = queues.drain_follow_up();
        if follow_up.is_empty() {
            if last_batch_terminated {
                stop = AgentLoopStop::TerminatedByTools;
            }
            break;
        }
        last_batch_terminated = false;
        pending = follow_up;
    }

    emit(
        &events,
        AgentEvent::AgentEnd {
            messages: new_messages.as_ref().clone(),
        },
        &signal,
    )
    .await?;

    Ok(AgentLoopOutcome {
        new_messages: Arc::unwrap_or_clone(new_messages),
        final_context: Arc::unwrap_or_clone(context),
        stop,
    })
}

async fn stream_assistant_response(
    run_id: &RunId,
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    services: &AssistantResponseServices,
    signal: AbortSignal,
) -> Result<AssistantMessage, AgentLoopError> {
    let generation = services.generation;
    let registries = Arc::clone(&services.registries);
    let plugins = Arc::clone(&services.plugins);
    let provider_plugins = Arc::clone(&services.provider_plugins);
    let events = Arc::clone(&services.events);

    let Some(provider) = registries.provider(&config.provider_id) else {
        return Err(AgentLoopError::ProviderNotFound(
            config.provider_id.to_string(),
        ));
    };
    let tools = registries
        .tool_specs(&context.active_tools)
        .map_err(|error| AgentLoopError::ProviderNotFound(error.to_string()))?;
    let request_messages = plugins
        .context(run_id, &config.cwd, &signal, context.messages.clone())
        .await
        .map_err(|error| AgentLoopError::Event(error.to_string()))?
        .into_iter()
        .map(Message::into_provider_message)
        .map(|message| filter_blocked_images(message, config.block_images))
        .collect();
    let model_spec = registries
        .model(&config.provider_id, &config.model_id)
        .cloned();
    let model_cost = model_spec.as_ref().map(|model| model.cost.clone());
    let api = model_spec
        .as_ref()
        .map_or_else(|| "unknown".to_string(), |model| model.api.clone());
    let request = ProviderRequest {
        model: config.model_id.clone(),
        model_spec,
        system_prompt: context.system_prompt.clone(),
        messages: request_messages,
        tools,
        thinking_level: config.thinking_level,
        thinking_budgets: config.thinking_budgets,
        max_output_tokens: None,
        headers: Default::default(),
        sampling_params: Default::default(),
        session_id: config.session_id.clone(),
    };

    let call_context = ProviderCallContext::new(
        generation,
        config.cwd.clone(),
        config.provider_id.clone(),
        config.model_id.clone(),
        provider_plugins,
    );
    let telemetry_span = services
        .telemetry
        .start_span::<AiRequestSpan>(AiRequestStart {
            operation: AiOperation::Stream,
            provider: config.provider_id.to_string(),
            model: config.model_id.to_string(),
            api,
            streaming: true,
            deferred: None,
        });
    let request_started = std::time::Instant::now();
    let mut chunk_count = 0_u64;
    let mut time_to_first_chunk_ms = None;
    let stream_result = provider.stream(request, call_context, signal.child()).await;
    let mut assembler = StreamAssembler::new();
    let mut started = false;
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            let message = provider_failure_message(config, error.to_string(), StopReason::Error);
            let message = emit_message_pair(&events, &signal, &message).await?;
            context.messages.push(Message::assistant(message.clone()));
            finish_ai_request(
                &telemetry_span,
                &message,
                chunk_count,
                time_to_first_chunk_ms,
            );
            return Ok(message);
        }
    };

    loop {
        let item = tokio::select! {
            biased;
            () = signal.wait() => {
                let message = assembler.failure_message(StopReason::Aborted, "operation aborted");
                if !started {
                    emit(&events, AgentEvent::MessageStart { message: Message::assistant(message.clone()) }, &signal).await?;
                }
                let message = message_as_assistant(
                    emit_message_end(&events, &signal, Message::assistant(message)).await?
                )?;
                if started {
                    replace_last_assistant(context, message.clone());
                } else {
                    context.messages.push(Message::assistant(message.clone()));
                }
                finish_ai_request(
                    &telemetry_span,
                    &message,
                    chunk_count,
                    time_to_first_chunk_ms,
                );
                return Ok(message);
            }
            item = stream.next() => item,
        };
        let Some(item) = item else {
            break;
        };
        chunk_count = chunk_count.saturating_add(1);
        time_to_first_chunk_ms.get_or_insert_with(|| {
            u64::try_from(request_started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        let mut stream_event = match item {
            Ok(event) => event,
            Err(error) => {
                let message = assembler.failure_message(StopReason::Error, error.to_string());
                if !started {
                    emit(
                        &events,
                        AgentEvent::MessageStart {
                            message: Message::assistant(message.clone()),
                        },
                        &signal,
                    )
                    .await?;
                }
                let message = message_as_assistant(
                    emit_message_end(&events, &signal, Message::assistant(message)).await?,
                )?;
                if started {
                    replace_last_assistant(context, message.clone());
                } else {
                    context.messages.push(Message::assistant(message.clone()));
                }
                finish_ai_request(
                    &telemetry_span,
                    &message,
                    chunk_count,
                    time_to_first_chunk_ms,
                );
                return Ok(message);
            }
        };
        if let (Some(cost), StreamEvent::Done { usage, .. }) = (&model_cost, &mut stream_event) {
            usage.cost = cost.calculate(usage);
        }
        let update = match assembler.push(stream_event) {
            Ok(update) => update,
            Err(error) => {
                let message = assembler.failure_message(StopReason::Error, error.to_string());
                let message =
                    commit_failed_assistant(context, &events, &signal, message, started).await?;
                finish_ai_request(
                    &telemetry_span,
                    &message,
                    chunk_count,
                    time_to_first_chunk_ms,
                );
                return Ok(message);
            }
        };
        if update.started && !started {
            let partial = assembler
                .snapshot()
                .map_err(|error| AgentLoopError::Assembly(error.to_string()))?;
            context.messages.push(Message::assistant(partial.clone()));
            emit(
                &events,
                AgentEvent::MessageStart {
                    message: Message::assistant(partial),
                },
                &signal,
            )
            .await?;
            started = true;
        } else if let Some(message_event) = update.message_event {
            let partial = assembler
                .snapshot()
                .map_err(|error| AgentLoopError::Assembly(error.to_string()))?;
            replace_last_assistant(context, partial.clone());
            emit(
                &events,
                AgentEvent::MessageUpdate {
                    message: partial,
                    event: message_event,
                },
                &signal,
            )
            .await?;
        }
    }

    let final_message = match assembler.finish() {
        Ok(message) => message,
        Err(error) => {
            let message = assembler.failure_message(StopReason::Error, error.to_string());
            let message =
                commit_failed_assistant(context, &events, &signal, message, started).await?;
            finish_ai_request(
                &telemetry_span,
                &message,
                chunk_count,
                time_to_first_chunk_ms,
            );
            return Ok(message);
        }
    };
    if started {
        replace_last_assistant(context, final_message.clone());
    } else {
        context
            .messages
            .push(Message::assistant(final_message.clone()));
        emit(
            &events,
            AgentEvent::MessageStart {
                message: Message::assistant(final_message.clone()),
            },
            &signal,
        )
        .await?;
    }
    let final_message = message_as_assistant(
        emit_message_end(&events, &signal, Message::assistant(final_message)).await?,
    )?;
    replace_last_assistant(context, final_message.clone());
    finish_ai_request(
        &telemetry_span,
        &final_message,
        chunk_count,
        time_to_first_chunk_ms,
    );
    Ok(final_message)
}

fn filter_blocked_images(mut message: Message, block_images: bool) -> Message {
    if !block_images {
        return message;
    }
    let content = match &mut message {
        Message::User(message) => &mut message.content,
        Message::ToolResult(message) => &mut Arc::make_mut(message).content,
        Message::Assistant(_) | Message::Custom(_) => return message,
    };
    if !content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(_)))
    {
        return message;
    }
    let mut previous_was_placeholder = false;
    *content = std::mem::take(content)
        .into_iter()
        .filter_map(|block| {
            let block = if matches!(block, ContentBlock::Image(_)) {
                ContentBlock::Text(TextContent::new("Image reading is disabled."))
            } else {
                block
            };
            let is_placeholder = matches!(
                &block,
                ContentBlock::Text(text) if text.text == "Image reading is disabled."
            );
            if is_placeholder && previous_was_placeholder {
                return None;
            }
            previous_was_placeholder = is_placeholder;
            Some(block)
        })
        .collect();
    message
}

fn finish_ai_request(
    span: &ActiveSpan<AiRequestSpan>,
    message: &AssistantMessage,
    chunk_count: u64,
    time_to_first_chunk_ms: Option<u64>,
) {
    let stop_reason = match message.stop_reason {
        StopReason::Stop => Some(AiStopReason::Stop),
        StopReason::Length => Some(AiStopReason::Length),
        StopReason::ToolUse => Some(AiStopReason::ToolUse),
        StopReason::Error => Some(AiStopReason::Error),
        StopReason::Aborted => Some(AiStopReason::Aborted),
        StopReason::Deferred => Some(AiStopReason::Deferred),
        StopReason::Pending => None,
    };
    span.set_end_attributes(AiRequestEnd {
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        stop_reason,
        input_tokens: Some(message.usage.input),
        output_tokens: Some(message.usage.output),
        cache_read_tokens: Some(message.usage.cache_read),
        cache_write_tokens: Some(message.usage.cache_write),
        reasoning_tokens: message.usage.reasoning,
        total_tokens: Some(message.usage.total_tokens),
        cost: Some(message.usage.cost.total),
        chunk_count: Some(chunk_count),
        time_to_first_chunk_ms,
        error_type: message
            .error_message
            .as_ref()
            .map(|_| "provider".to_string()),
        ..AiRequestEnd::default()
    });
    if message.stop_reason == StopReason::Error {
        span.set_status(SpanStatus::Error);
    }
    span.finish();
}

async fn commit_failed_assistant(
    context: &mut AgentContext,
    events: &Arc<dyn AgentEventSink>,
    signal: &AbortSignal,
    message: AssistantMessage,
    started: bool,
) -> Result<AssistantMessage, AgentLoopError> {
    if !started {
        emit(
            events,
            AgentEvent::MessageStart {
                message: Message::assistant(message.clone()),
            },
            signal,
        )
        .await?;
    }
    let message =
        message_as_assistant(emit_message_end(events, signal, Message::assistant(message)).await?)?;
    if started {
        replace_last_assistant(context, message.clone());
    } else {
        context.messages.push(Message::assistant(message.clone()));
    }
    Ok(message)
}

fn replace_last_assistant(context: &mut AgentContext, message: AssistantMessage) {
    if let Some(last) = context.messages.last_mut()
        && last.is_assistant()
    {
        *last = Message::assistant(message);
    }
}

fn provider_failure_message(
    config: &AgentLoopConfig,
    error: String,
    reason: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text(TextContent::new(""))],
        api: "unknown".to_string(),
        provider: config.provider_id.clone(),
        model: config.model_id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: reason,
        error_message: Some(error),
        deferred: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp_ms: now_ms(),
    }
}

/// Best-effort outer lifecycle recovery matching Pi's `handleRunFailure`.
///
/// The originating loop error remains the caller's result. Recovery emission
/// deliberately continues after listener failures so the state reducer still
/// observes `message_end`, `turn_end`, and `agent_end` whenever possible.
pub(crate) async fn emit_run_failure_lifecycle(
    events: &Arc<dyn AgentEventSink>,
    signal: &AbortSignal,
    config: &AgentLoopConfig,
    error: &AgentLoopError,
) {
    let reason = if signal.is_aborted() {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    let mut assistant = provider_failure_message(config, error.to_string(), reason);
    let _ = events
        .emit(
            AgentEvent::MessageStart {
                message: Message::assistant(assistant.clone()),
            },
            signal.clone(),
        )
        .await;
    if let Ok(AgentEvent::MessageEnd {
        message: Message::Assistant(message),
    }) = events
        .emit(
            AgentEvent::MessageEnd {
                message: Message::assistant(assistant.clone()),
            },
            signal.clone(),
        )
        .await
    {
        assistant = Arc::unwrap_or_clone(message);
    }
    let _ = events
        .emit(
            AgentEvent::TurnEnd {
                message: assistant.clone(),
                tool_results: Vec::new(),
            },
            signal.clone(),
        )
        .await;
    let _ = events
        .emit(
            AgentEvent::AgentEnd {
                messages: vec![Message::assistant(assistant)],
            },
            signal.clone(),
        )
        .await;
}

async fn emit_message_pair(
    events: &Arc<dyn AgentEventSink>,
    signal: &AbortSignal,
    message: &AssistantMessage,
) -> Result<AssistantMessage, AgentLoopError> {
    emit(
        events,
        AgentEvent::MessageStart {
            message: Message::assistant(message.clone()),
        },
        signal,
    )
    .await?;
    message_as_assistant(
        emit_message_end(events, signal, Message::assistant(message.clone())).await?,
    )
}

async fn fail_truncated_tool_calls(
    calls: Vec<pi_core::ToolCall>,
    signal: &AbortSignal,
    events: Arc<dyn AgentEventSink>,
    timestamp_ms: i64,
) -> Result<crate::ExecutedToolBatch, AgentLoopError> {
    fail_unexecuted_tool_calls(calls, signal, events, timestamp_ms, |call| {
        format!(
            "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
            call.name
        )
    })
    .await
}

async fn fail_unexecuted_tool_calls<F>(
    calls: Vec<pi_core::ToolCall>,
    signal: &AbortSignal,
    events: Arc<dyn AgentEventSink>,
    timestamp_ms: i64,
    error_message: F,
) -> Result<crate::ExecutedToolBatch, AgentLoopError>
where
    F: Fn(&pi_core::ToolCall) -> String,
{
    let mut messages = Vec::with_capacity(calls.len());
    for call in calls {
        emit(
            &events,
            AgentEvent::ToolExecutionStart {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                args: call.arguments.clone(),
            },
            signal,
        )
        .await?;
        let result = ToolResult::error(error_message(&call));
        emit(
            &events,
            AgentEvent::ToolExecutionEnd {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                result: result.clone(),
                is_error: true,
            },
            signal,
        )
        .await?;
        let message = ToolResultMessage {
            tool_call_id: call.id,
            tool_name: call.name,
            content: result.content,
            details: result.details,
            usage: result.usage,
            added_tool_names: None,
            is_error: true,
            timestamp_ms,
        };
        emit(
            &events,
            AgentEvent::MessageStart {
                message: Message::tool_result(message.clone()),
            },
            signal,
        )
        .await?;
        let message = message_as_tool_result(
            emit_message_end(&events, signal, Message::tool_result(message)).await?,
        )?;
        messages.push(message);
    }
    Ok(crate::ExecutedToolBatch {
        messages,
        terminate: false,
    })
}

async fn emit(
    events: &Arc<dyn AgentEventSink>,
    event: AgentEvent,
    signal: &AbortSignal,
) -> Result<(), AgentLoopError> {
    events
        .emit(event, signal.clone())
        .await
        .map(|_| ())
        .map_err(|error| AgentLoopError::Event(error.to_string()))
}

async fn emit_message_end(
    events: &Arc<dyn AgentEventSink>,
    signal: &AbortSignal,
    message: Message,
) -> Result<Message, AgentLoopError> {
    match events
        .emit(AgentEvent::MessageEnd { message }, signal.clone())
        .await
        .map_err(|error| AgentLoopError::Event(error.to_string()))?
    {
        AgentEvent::MessageEnd { message } => Ok(message),
        event => Err(AgentLoopError::Event(format!(
            "message_end dispatch returned a different event: {event:?}"
        ))),
    }
}

fn message_as_assistant(message: Message) -> Result<AssistantMessage, AgentLoopError> {
    match message {
        Message::Assistant(message) => Ok(Arc::unwrap_or_clone(message)),
        message => Err(AgentLoopError::Event(format!(
            "message_end changed assistant role to {message:?}"
        ))),
    }
}

fn message_as_tool_result(message: Message) -> Result<ToolResultMessage, AgentLoopError> {
    match message {
        Message::ToolResult(message) => Ok(Arc::unwrap_or_clone(message)),
        message => Err(AgentLoopError::Event(format!(
            "message_end changed tool-result role to {message:?}"
        ))),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ImageContent, ToolCallId, ToolResultMessage};

    #[test]
    fn blocked_images_are_replaced_only_in_the_provider_projection() {
        let original = Message::tool_result(ToolResultMessage {
            tool_call_id: ToolCallId::new("call"),
            tool_name: "read".to_string(),
            content: vec![
                ContentBlock::Image(ImageContent {
                    data: "one".to_string(),
                    mime_type: "image/png".to_string(),
                }),
                ContentBlock::Image(ImageContent {
                    data: "two".to_string(),
                    mime_type: "image/png".to_string(),
                }),
                ContentBlock::Text(TextContent::new("kept")),
            ],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 0,
        });

        let filtered = filter_blocked_images(original.clone(), true);

        let Message::ToolResult(filtered) = filtered else {
            panic!("expected tool result")
        };
        assert_eq!(filtered.content.len(), 2);
        assert!(matches!(
            &filtered.content[0],
            ContentBlock::Text(text) if text.text == "Image reading is disabled."
        ));
        assert!(matches!(
            &filtered.content[1],
            ContentBlock::Text(text) if text.text == "kept"
        ));
        let Message::ToolResult(original) = original else {
            panic!("expected tool result")
        };
        assert!(matches!(original.content[0], ContentBlock::Image(_)));
    }
}
