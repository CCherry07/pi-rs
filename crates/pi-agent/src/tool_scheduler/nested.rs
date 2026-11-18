use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AbortHandle, AbortSignal, AgentContext, AgentEvent, AssistantMessage, RunId, ToolCall,
    ToolCallId, ToolExecutionMode, ToolResult, Usage,
};
use pi_plugin::{NestedToolExecutor, NestedToolResult, ToolError, ToolInfo};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use super::ToolScheduler;
use crate::{AgentEventSink, EventError};

/// Only the invocation owner can keep task admission open. A retained executor
/// cannot keep an invocation alive after preparation/execution settles.
pub(super) struct InvocationLifetime {
    handle: AbortHandle,
    signal: AbortSignal,
    tasks: Arc<Mutex<InvocationTasks>>,
}

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct InvocationTasks {
    running: JoinSet<()>,
    closed: bool,
    usage: Option<Usage>,
}

impl InvocationLifetime {
    pub(super) fn new() -> Self {
        let (handle, signal) = AbortHandle::new();
        Self {
            handle,
            signal,
            tasks: Arc::default(),
        }
    }

    pub(super) fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    pub(super) fn close(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.closed = true;
        self.handle.abort();
    }

    /// Close admission before cancellation, then let started calls report
    /// interruption through the ordinary result hooks. A stuck async hook is
    /// aborted after the fixed cleanup window; no task is detached.
    pub(super) async fn settle(&self) -> Option<Usage> {
        let mut running = {
            let mut tasks = self.tasks.lock().unwrap();
            tasks.closed = true;
            self.handle.abort();
            std::mem::take(&mut tasks.running)
        };
        let completed = tokio::time::timeout(CLEANUP_TIMEOUT, async {
            while running.join_next().await.is_some() {}
        })
        .await;
        if completed.is_err() {
            running.shutdown().await;
        }
        self.tasks.lock().unwrap().usage.clone()
    }
}

impl Drop for InvocationLifetime {
    fn drop(&mut self) {
        self.handle.abort();
        // Fallback for a dropped scheduler/prepare future: a JoinSet aborts
        // children on drop, rather than allowing retained generation work.
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.closed = true;
            tasks.running.abort_all();
        }
    }
}

#[derive(Clone)]
pub(super) struct InvocationNestedExecutor {
    scheduler: ToolScheduler,
    run_id: RunId,
    assistant: Arc<AssistantMessage>,
    context: Arc<AgentContext>,
    parent: ToolCall,
    run_signal: AbortSignal,
    lifetime: AbortSignal,
    events: Arc<dyn AgentEventSink>,
    allowed: Arc<Vec<ToolInfo>>,
    next_id: Arc<AtomicU64>,
    tasks: Arc<Mutex<InvocationTasks>>,
}

impl InvocationNestedExecutor {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        scheduler: ToolScheduler,
        run_id: RunId,
        assistant: Arc<AssistantMessage>,
        context: Arc<AgentContext>,
        parent: ToolCall,
        run_signal: AbortSignal,
        lifetime: &InvocationLifetime,
        events: Arc<dyn AgentEventSink>,
        allowed: Arc<Vec<ToolInfo>>,
    ) -> Self {
        Self {
            scheduler,
            run_id,
            assistant,
            context,
            parent,
            run_signal,
            lifetime: lifetime.signal(),
            events,
            allowed,
            next_id: Arc::new(AtomicU64::new(1)),
            tasks: Arc::clone(&lifetime.tasks),
        }
    }

    async fn execute_scoped(
        &self,
        name: &str,
        arguments: Value,
        signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        // Check the snapshot before resolving a tool. Guessed names must not
        // reach inactive direct tools, model-only tools or scoped-out tools.
        if signal.is_aborted() {
            return Err(ToolError::Aborted);
        }
        if !self.allowed.iter().any(|allowed| allowed.spec.name == name) {
            return Err(ToolError::Execution(format!(
                "Tool {name} is not available for nested execution"
            )));
        }
        let tool = self
            .scheduler
            .registries
            .tool(name)
            .ok_or_else(|| ToolError::Execution(format!("Tool {name} not found")))?;
        let call = ToolCall::new(
            ToolCallId::new(format!(
                "{}/{}",
                self.parent.id,
                self.next_id.fetch_add(1, Ordering::Relaxed)
            )),
            name,
            arguments,
        );
        let events: Arc<dyn AgentEventSink> = Arc::new(NestedEvents {
            parent: self.parent.clone(),
            events: Arc::clone(&self.events),
            lifetime: self.lifetime.clone(),
        });
        if signal.is_aborted() {
            return Err(ToolError::Aborted);
        }
        let scheduler = ToolScheduler {
            parent_call_id: Some(self.parent.id.clone()),
            ..self.scheduler.clone()
        };
        scheduler
            .emit_start(&call, &signal, Arc::clone(&events))
            .await
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let prepared = scheduler
            .prepare_call(
                &self.run_id,
                &self.assistant,
                &self.context,
                &self.context.active_tools,
                0,
                call,
                &signal,
                Arc::clone(&events),
            )
            .await;
        let finalized = scheduler
            .execute_prepared(
                &self.run_id,
                &self.assistant,
                &self.context,
                prepared,
                &signal,
                Arc::clone(&events),
            )
            .await;
        scheduler
            .emit_end(&finalized, &signal, events)
            .await
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let structured_content = tool.structured_output(&finalized.result);
        Ok(NestedToolResult {
            call_id: finalized.call.id,
            result: finalized.result,
            structured_content,
        })
    }

    async fn execute_linked(
        &self,
        name: &str,
        arguments: Value,
        signal: AbortSignal,
        caller_dropped: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        let (handle, call_signal) = AbortHandle::new();
        let _cancel = CancelOnDrop(handle.clone());
        let cancel = async {
            tokio::select! {
                biased;
                () = self.lifetime.wait() => {},
                () = self.run_signal.wait() => {},
                () = signal.wait() => {},
                () = caller_dropped.wait() => {},
            }
        };
        let execution = self.execute_scoped(name, arguments, call_signal);
        tokio::pin!(execution);
        tokio::select! {
            biased;
            () = cancel => {
                handle.abort();
                tokio::time::timeout(CLEANUP_TIMEOUT, &mut execution).await
                    .unwrap_or(Err(ToolError::Aborted))
            },
            result = &mut execution => result,
        }
    }
}

struct CancelOnDrop(AbortHandle);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[async_trait]
impl NestedToolExecutor for InvocationNestedExecutor {
    fn record_usage(&self, usage: &Usage) {
        let mut tasks = self.tasks.lock().unwrap();
        if !tasks.closed {
            add_usage(&mut tasks.usage, Some(usage));
        }
    }

    async fn execute(
        &self,
        name: &str,
        arguments: Value,
        signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        let (handle, caller_dropped) = AbortHandle::new();
        let _cancel = CancelOnDrop(handle);
        if self.lifetime.is_aborted() || self.run_signal.is_aborted() || signal.is_aborted() {
            return Err(ToolError::Aborted);
        }
        let tool = self
            .allowed
            .iter()
            .find(|tool| tool.spec.name == name)
            .ok_or_else(|| {
                ToolError::Execution(format!("Tool {name} is not available for nested execution"))
            })?;
        let exclusive = self.scheduler.sequential_nested
            || tool.spec.execution_mode == ToolExecutionMode::Sequential;
        // Enter the fair queue in caller order, before moving execution to a
        // task. Task scheduling must not reorder sequential mutations. Queued
        // calls have not started a pipeline and can simply be dropped.
        let gate = Arc::clone(&self.scheduler.nested_gate);
        let lease = tokio::select! {
            biased;
            () = self.lifetime.wait() => return Err(ToolError::Aborted),
            () = self.run_signal.wait() => return Err(ToolError::Aborted),
            () = signal.wait() => return Err(ToolError::Aborted),
            lease = async {
                if exclusive {
                    (None, Some(gate.write_owned().await))
                } else {
                    (Some(gate.read_owned().await), None)
                }
            } => lease,
        };
        let (reply, result) = oneshot::channel();
        {
            let mut tasks = self.tasks.lock().unwrap();
            if tasks.closed
                || self.lifetime.is_aborted()
                || self.run_signal.is_aborted()
                || signal.is_aborted()
            {
                return Err(ToolError::Aborted);
            }
            while tasks.running.try_join_next().is_some() {}
            let executor = self.clone();
            let name = name.to_owned();
            tasks.running.spawn(async move {
                let _lease = lease;
                let outcome = executor
                    .execute_linked(&name, arguments, signal, caller_dropped)
                    .await;
                if let Ok(result) = &outcome {
                    add_usage(
                        &mut executor.tasks.lock().unwrap().usage,
                        result.result.usage.as_ref(),
                    );
                }
                let _ = reply.send(outcome);
            });
        }
        result.await.map_err(|_| {
            ToolError::Execution("Nested tool pipeline stopped during cleanup".into())
        })?
    }
}

fn add_usage(total: &mut Option<Usage>, next: Option<&Usage>) {
    let Some(next) = next else {
        return;
    };
    let total = total.get_or_insert_with(Usage::default);
    total.input = total.input.saturating_add(next.input);
    total.output = total.output.saturating_add(next.output);
    total.cache_read = total.cache_read.saturating_add(next.cache_read);
    total.cache_write = total.cache_write.saturating_add(next.cache_write);
    total.total_tokens = total.total_tokens.saturating_add(next.total_tokens);
    if let Some(value) = next.cache_write_1h {
        total.cache_write_1h = Some(
            total
                .cache_write_1h
                .unwrap_or_default()
                .saturating_add(value),
        );
    }
    if let Some(value) = next.reasoning {
        total.reasoning = Some(total.reasoning.unwrap_or_default().saturating_add(value));
    }
    total.cost.input += next.cost.input;
    total.cost.output += next.cost.output;
    total.cost.cache_read += next.cost.cache_read;
    total.cost.cache_write += next.cost.cache_write;
    total.cost.total += next.cost.total;
}

/// Nested progress belongs to its model-issued parent. Mapping it to an update
/// avoids both orphan result messages and terminal entries without a parent.
struct NestedEvents {
    parent: ToolCall,
    events: Arc<dyn AgentEventSink>,
    lifetime: AbortSignal,
}

#[async_trait]
impl AgentEventSink for NestedEvents {
    async fn emit(&self, event: AgentEvent, signal: AbortSignal) -> Result<AgentEvent, EventError> {
        if self.lifetime.is_aborted() {
            return Ok(event);
        }
        let (id, name, status) = match &event {
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                ..
            } => (tool_call_id, tool_name, "running"),
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                ..
            } => (tool_call_id, tool_name, "running"),
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                is_error,
                ..
            } => (
                tool_call_id,
                tool_name,
                if *is_error { "error" } else { "ok" },
            ),
            _ => {
                return Err(EventError(
                    "nested execution cannot emit transcript events".into(),
                ));
            }
        };
        self.events
            .emit(
                AgentEvent::ToolExecutionUpdate {
                    tool_call_id: self.parent.id.clone(),
                    tool_name: self.parent.name.clone(),
                    args: self.parent.arguments.clone(),
                    partial_result: ToolResult {
                        structured_content: None,
                        content: Vec::new(),
                        details: Some(serde_json::json!({
                            "nested_call": { "id": id, "name": name, "status": status }
                        })),
                        usage: None,
                        added_tool_names: None,
                        is_error: false,
                        terminate: false,
                    },
                },
                signal,
            )
            .await?;
        Ok(event)
    }
}

#[cfg(test)]
mod tests;
