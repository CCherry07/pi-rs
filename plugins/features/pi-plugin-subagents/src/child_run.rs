//! A child run outlives any one foreground tool call or bg_wait invocation.
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures::FutureExt;
use pi_core::{AbortSignal, IsolatedSessionHandle, ToolResult};
use serde_json::json;

use crate::coordination::{RunResult, TerminalState};
use crate::runtime::WeakSubagentRuntime;
use crate::tool::{final_text, with_warnings};

pub(crate) struct ChildRun {
    pub runtime: WeakSubagentRuntime,
    pub run_id: String,
    pub owner: String,
    pub handle: IsolatedSessionHandle,
    pub signal: AbortSignal,
    pub timeout: Option<Duration>,
}

/// Constructed before spawn: even an unpolled/dropped monitor publishes failure.
struct CompletionGuard {
    run: ChildRun,
    completed: bool,
}

impl CompletionGuard {
    fn complete(&mut self, state: TerminalState, result: RunResult) {
        let Some(runtime) = self.run.runtime.upgrade() else {
            self.completed = true;
            return;
        };
        runtime.finish(&self.run.run_id);
        // Mark before calling the delivery adapter, which is trusted plugin code
        // and may itself panic. The terminal receipt is committed before delivery.
        self.completed = true;
        runtime
            .coordination()
            .complete_with_state(&self.run.run_id, state, result);
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if !self.completed {
            // Cleanup must not double-panic if an adapter unwinds during shutdown.
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| self.run.abort()));
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                self.complete(
                    TerminalState::Cancelled,
                    Err("Subagent monitor was cancelled before completion.".into()),
                );
            }));
        }
    }
}

impl ChildRun {
    pub fn monitor(self) -> impl Future<Output = ()> + Send + 'static {
        let mut guard = CompletionGuard {
            run: self,
            completed: false,
        };
        async move {
            let result = AssertUnwindSafe(guard.run.execute())
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("Subagent monitor panicked before completion.".into()));
            if result.is_err() {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| guard.run.abort()));
            }
            match result {
                Ok((state, result)) => guard.complete(state, Ok(result)),
                Err(error) => guard.complete(TerminalState::Failed, Err(error)),
            }
        }
    }

    fn abort(&self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.coordination().cancelling(&self.owner, &self.run_id);
        }
        // Prefer the currently bound owner handle for control operations.
        let _ = self
            .runtime
            .upgrade()
            .ok_or_else(|| "Subagent owner was dropped.".to_string())
            .and_then(|runtime| {
                runtime
                    .coordination()
                    .abort_isolated(&self.owner, self.handle.id().clone())
            })
            .or_else(|_| self.handle.abort().map_err(|error| error.to_string()));
    }

    fn account_outcome(
        &self,
        outcome: &pi_core::IsolatedSessionOutcome,
        details: &mut serde_json::Value,
    ) -> Option<String> {
        details["sessionId"] = json!(outcome.session_id);
        details["usage"] = json!(outcome.usage);
        if outcome.usage == Default::default() {
            return None;
        }
        let runtime = self.runtime.upgrade().ok_or_else(|| {
            "Subagent owner was dropped before usage could be recorded.".to_string()
        });
        runtime
            .and_then(|runtime| {
                let mut attribution = json!({
                    "source": "subagent",
                    "runId": self.run_id,
                    "childSessionId": outcome.session_id,
                    "agent": details.get("agent").cloned(),
                    "depth": details.get("depth").cloned(),
                });
                if let Some(workflow_id) = details.get("workflowId") {
                    attribution["workflowId"] = workflow_id.clone();
                }
                runtime.coordination().record_usage(
                    &self.run_id,
                    &self.owner,
                    outcome.usage.clone(),
                    attribution,
                )
            })
            .err()
            .map(|error| format!("Subagent usage accounting failed: {error}"))
    }

    async fn execute(&self) -> Result<(TerminalState, ToolResult), String> {
        let end = self
            .runtime
            .upgrade()
            .and_then(|runtime| runtime.coordination().monitor_started(&self.run_id));
        let deadline = async {
            match end {
                Some(end) => tokio::time::sleep_until(end).await,
                None => std::future::pending::<()>().await,
            }
        };
        let waiting = self.handle.wait();
        tokio::pin!(waiting);
        let mut details = self
            .runtime
            .upgrade()
            .ok_or_else(|| "Subagent owner was dropped.".to_string())?
            .coordination()
            .run(&self.owner, &self.run_id)?
            .details(&self.run_id, Vec::new());
        let outcome = tokio::select! {
            biased;
            result = &mut waiting => result.map_err(|error| error.to_string())?,
            () = self.signal.wait() => {
                self.abort();
                let accounting_warning = waiting
                    .await
                    .ok()
                    .and_then(|outcome| self.account_outcome(&outcome, &mut details));
                let mut error = "Subagent was aborted before it completed.".to_string();
                if let Some(warning) = accounting_warning {
                    error.push(' ');
                    error.push_str(&warning);
                }
                let mut result = ToolResult::error(error);
                details["state"] = json!("cancelled");
                result.details = Some(details);
                return Ok((TerminalState::Cancelled, result));
            }
            () = deadline => {
                self.abort();
                let accounting_warning = waiting
                    .await
                    .ok()
                    .and_then(|outcome| self.account_outcome(&outcome, &mut details));
                let timeout_ms = self.timeout.expect("configured deadline").as_millis();
                details["state"] = json!("timed_out");
                details["timeoutMs"] = json!(timeout_ms);
                let mut warnings = self.runtime.upgrade().map(|runtime| runtime.warnings(&self.run_id)).unwrap_or_default();
                warnings.extend(accounting_warning);
                details["warnings"] = json!(warnings);
                let mut result = ToolResult::error(with_warnings(format!("{} subagent timed out after {timeout_ms} ms", details["agent"].as_str().unwrap_or("child")), &warnings));
                result.details = Some(details);
                return Ok((TerminalState::TimedOut, result));
            }
        };
        let mut warnings = self
            .runtime
            .upgrade()
            .map(|runtime| runtime.warnings(&self.run_id))
            .unwrap_or_default();
        warnings.extend(self.account_outcome(&outcome, &mut details));
        details["warnings"] = json!(warnings);
        let failure = outcome
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                pi_core::Message::Assistant(message) => Some(message),
                _ => None,
            })
            .filter(|message| message.stop_reason == pi_core::StopReason::Error)
            .map(|message| {
                message
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Subagent provider failed.".into())
            });
        let terminal = if outcome.aborted {
            TerminalState::Cancelled
        } else if failure.is_some() {
            TerminalState::Failed
        } else {
            TerminalState::Completed
        };
        details["state"] = json!(match terminal {
            TerminalState::Completed => "completed",
            TerminalState::Failed => "failed",
            TerminalState::Cancelled => "cancelled",
            TerminalState::TimedOut => "timed_out",
        });
        details["aborted"] = json!(outcome.aborted);
        let mut result = if outcome.aborted {
            ToolResult::error("Subagent was aborted before it completed.")
        } else if let Some(failure) = failure {
            ToolResult::error(with_warnings(failure, &warnings))
        } else {
            ToolResult::text(with_warnings(final_text(&outcome.messages), &warnings))
        };
        result.details = Some(details);
        Ok((terminal, result))
    }
}
