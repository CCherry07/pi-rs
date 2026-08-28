use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use pi_core::{
    AbortSignal, AgentEndEvent, AgentEvent, AgentStartEvent, Message, MessageEndEvent,
    MessageStartEvent, MessageUpdateEvent, PluginDriver, RunId, StopReason, ToolExecutionEndEvent,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent, TurnEndEvent, TurnStartEvent,
};

use crate::agent::{AgentState, RegisteredListeners};

#[derive(Debug, thiserror::Error)]
#[error("event listener failed: {0}")]
pub struct EventError(pub String);

#[async_trait]
pub trait AgentEventSink: Send + Sync {
    async fn emit(&self, event: AgentEvent, signal: AbortSignal) -> Result<AgentEvent, EventError>;
}

#[async_trait]
pub trait AgentEventListener: Send + Sync {
    async fn on_event(&self, event: AgentEvent, signal: AbortSignal) -> Result<(), EventError>;
}

#[async_trait]
impl<F, Fut> AgentEventListener for F
where
    F: Fn(AgentEvent, AbortSignal) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<(), EventError>> + Send,
{
    async fn on_event(&self, event: AgentEvent, signal: AbortSignal) -> Result<(), EventError> {
        self(event, signal).await
    }
}

pub(crate) struct AgentEventDispatcher {
    state: Arc<Mutex<AgentState>>,
    listeners: Arc<RwLock<RegisteredListeners>>,
    plugins: Arc<PluginDriver>,
    run_id: RunId,
    cwd: PathBuf,
    turn_index: AtomicU64,
}

impl AgentEventDispatcher {
    pub(crate) fn new(
        state: Arc<Mutex<AgentState>>,
        listeners: Arc<RwLock<RegisteredListeners>>,
        plugins: Arc<PluginDriver>,
        run_id: RunId,
        cwd: PathBuf,
    ) -> Self {
        Self {
            state,
            listeners,
            plugins,
            run_id,
            cwd,
            turn_index: AtomicU64::new(0),
        }
    }

    fn reduce(&self, event: &AgentEvent) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match event {
            AgentEvent::MessageStart { message } => {
                state.snapshot.streaming_message = match message {
                    Message::Assistant(message) => Some((**message).clone()),
                    _ => None,
                };
            }
            AgentEvent::MessageUpdate { message, .. } => {
                state.snapshot.streaming_message = Some(message.clone())
            }
            AgentEvent::MessageEnd { message } => {
                state.snapshot.streaming_message = None;
                state.snapshot.messages.push(message.clone());
            }
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                state
                    .snapshot
                    .pending_tool_calls
                    .insert(tool_call_id.clone());
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                state.snapshot.pending_tool_calls.remove(tool_call_id);
            }
            AgentEvent::TurnEnd { message, .. }
                if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) =>
            {
                state.snapshot.error_message = message.error_message.clone();
            }
            AgentEvent::AgentStart
            | AgentEvent::AgentEnd { .. }
            | AgentEvent::TurnStart
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::ToolExecutionUpdate { .. } => {}
        }
    }

    async fn dispatch_plugin(&self, event: &AgentEvent, signal: &AbortSignal) -> Option<Message> {
        match event {
            AgentEvent::AgentStart => {
                self.turn_index.store(0, Ordering::Relaxed);
                self.plugins
                    .agent_start(&self.run_id, &self.cwd, signal, AgentStartEvent)
                    .await;
                None
            }
            AgentEvent::AgentEnd { messages } => {
                self.plugins
                    .agent_end(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        AgentEndEvent {
                            messages: messages.clone(),
                        },
                    )
                    .await;
                None
            }
            AgentEvent::TurnStart => {
                self.plugins
                    .turn_start(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        TurnStartEvent {
                            turn_index: self.turn_index.load(Ordering::Relaxed),
                            timestamp_ms: now_ms(),
                        },
                    )
                    .await;
                None
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                let turn_index = self.turn_index.load(Ordering::Relaxed);
                self.plugins
                    .turn_end(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        TurnEndEvent {
                            turn_index,
                            message: message.clone(),
                            tool_results: tool_results.clone(),
                        },
                    )
                    .await;
                self.turn_index.fetch_add(1, Ordering::Relaxed);
                None
            }
            AgentEvent::MessageStart { message } => {
                self.plugins
                    .message_start(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        MessageStartEvent {
                            message: message.clone(),
                        },
                    )
                    .await;
                None
            }
            AgentEvent::MessageUpdate { message, event } => {
                self.plugins
                    .message_update(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        MessageUpdateEvent {
                            message: message.clone(),
                            event: event.clone(),
                        },
                    )
                    .await;
                None
            }
            AgentEvent::MessageEnd { message } => Some(
                self.plugins
                    .message_end(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        MessageEndEvent {
                            message: message.clone(),
                        },
                    )
                    .await,
            ),
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.plugins
                    .tool_execution_start(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        ToolExecutionStartEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                        },
                    )
                    .await;
                None
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => {
                self.plugins
                    .tool_execution_update(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        ToolExecutionUpdateEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                            partial_result: partial_result.clone(),
                        },
                    )
                    .await;
                None
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                self.plugins
                    .tool_execution_end(
                        &self.run_id,
                        &self.cwd,
                        signal,
                        ToolExecutionEndEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            result: result.clone(),
                            is_error: *is_error,
                        },
                    )
                    .await;
                None
            }
        }
    }
}

#[async_trait]
impl AgentEventSink for AgentEventDispatcher {
    async fn emit(
        &self,
        mut event: AgentEvent,
        signal: AbortSignal,
    ) -> Result<AgentEvent, EventError> {
        self.reduce(&event);
        if let Some(message) = self.dispatch_plugin(&event, &signal).await {
            event = AgentEvent::MessageEnd { message };
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let AgentEvent::MessageEnd { message } = &event
                && let Some(last) = state.snapshot.messages.last_mut()
            {
                *last = message.clone();
            }
        }
        let listeners = self
            .listeners
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect::<Vec<_>>();
        for listener in listeners {
            listener.on_event(event.clone(), signal.clone()).await?;
        }
        Ok(event)
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
