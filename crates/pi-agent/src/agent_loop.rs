use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use pi_core::{
    AbortSignal, AgentEvent, AssistantMessage, ContentBlock, FrozenRegistries, Message, ModelId,
    PluginDriver, ProviderCallContext, ProviderId, ProviderPluginDriver, ProviderRequest, RunId,
    StopReason, StreamEvent, TextContent, ThinkingLevel, ToolExecutionMode, ToolResult,
    ToolResultMessage, Usage,
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
    #[error("maximum tool iterations exceeded: {0}")]
    MaxToolIterations(usize),
}

#[derive(Debug, Clone)]
pub struct AgentContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub active_tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AgentLoopConfig {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub thinking_level: ThinkingLevel,
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
    pub events: Arc<dyn AgentEventSink>,
}

struct AssistantResponseServices {
    generation: u64,
    registries: Arc<FrozenRegistries>,
    plugins: Arc<PluginDriver>,
    provider_plugins: Arc<ProviderPluginDriver>,
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
        signal,
        events,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    run_id: RunId,
    mut context: AgentContext,
    mut new_messages: Vec<Message>,
    config: AgentLoopConfig,
    generation: u64,
    registries: Arc<FrozenRegistries>,
    plugins: Arc<PluginDriver>,
    provider_plugins: Arc<ProviderPluginDriver>,
    queues: Arc<dyn AgentMessageQueues>,
    signal: AbortSignal,
    events: Arc<dyn AgentEventSink>,
    mut first_turn: bool,
) -> Result<AgentLoopOutcome, AgentLoopError> {
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
        events: Arc::clone(&events),
    };
    let mut pending = queues.drain_steering();
    let mut tool_iterations = 0usize;
    let mut stop = AgentLoopStop::Completed;
    let mut last_batch_terminated = false;

    'run: loop {
        let mut should_continue = true;
        while should_continue || !pending.is_empty() {
            if signal.is_aborted() {
                stop = AgentLoopStop::Aborted;
                break 'run;
            }
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
                context.messages.push(message.clone());
                new_messages.push(message);
            }

            let assistant = stream_assistant_response(
                &run_id,
                &mut context,
                &config,
                &response_services,
                signal.clone(),
            )
            .await?;
            new_messages.push(Message::assistant(assistant.clone()));

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
                        context.messages.push(message.clone());
                        new_messages.push(message);
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
                    context.messages.push(message.clone());
                    new_messages.push(message);
                }
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
            messages: new_messages.clone(),
        },
        &signal,
    )
    .await?;

    Ok(AgentLoopOutcome {
        new_messages,
        final_context: context,
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
        .collect();
    let model_spec = registries
        .model(&config.provider_id, &config.model_id)
        .cloned();
    let model_cost = model_spec.as_ref().map(|model| model.cost.clone());
    let request = ProviderRequest {
        model: config.model_id.clone(),
        model_spec,
        system_prompt: context.system_prompt.clone(),
        messages: request_messages,
        tools,
        thinking_level: config.thinking_level,
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
    let stream_result = provider.stream(request, call_context, signal.child()).await;
    let mut assembler = StreamAssembler::new();
    let mut started = false;
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            let message = provider_failure_message(config, error.to_string(), StopReason::Error);
            let message = emit_message_pair(&events, &signal, &message).await?;
            context.messages.push(Message::assistant(message.clone()));
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
                return Ok(message);
            }
            item = stream.next() => item,
        };
        let Some(item) = item else {
            break;
        };
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
            let message = provider_failure_message(config, error.to_string(), StopReason::Error);
            let message =
                commit_failed_assistant(context, &events, &signal, message, started).await?;
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
    Ok(final_message)
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
