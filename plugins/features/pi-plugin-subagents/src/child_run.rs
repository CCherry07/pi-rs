//! A child run outlives any one foreground tool call or bg_wait invocation.
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures::FutureExt;
use pi_core::{AbortSignal, IsolatedSessionHandle, ToolResult};
use serde_json::json;

use crate::coordination::RunResult;
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
    fn complete(&mut self, result: RunResult) {
        let Some(runtime) = self.run.runtime.upgrade() else {
            self.completed = true;
            return;
        };
        runtime.finish(&self.run.run_id);
        // Mark before calling the delivery adapter, which is trusted plugin code
        // and may itself panic. The terminal receipt is committed before delivery.
        self.completed = true;
        runtime.coordination().complete(&self.run.run_id, result);
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if !self.completed {
            // Cleanup must not double-panic if an adapter unwinds during shutdown.
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| self.run.abort()));
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                self.complete(Err(
                    "Subagent monitor was cancelled before completion.".into()
                ));
            }));
        }
    }
}

impl ChildRun {
    pub fn monitor(
        self,
        started: tokio::sync::oneshot::Sender<()>,
    ) -> impl Future<Output = ()> + Send + 'static {
        let mut guard = CompletionGuard {
            run: self,
            completed: false,
        };
        async move {
            let result = {
                let execution = AssertUnwindSafe(guard.run.execute()).catch_unwind();
                tokio::pin!(execution);
                let mut started = Some(started);
                std::future::poll_fn(|context| {
                    let result = execution.as_mut().poll(context);
                    // Acquire the generation-bound wait before the foreground
                    // caller can detach and allow its generation to reload.
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                    result
                })
                .await
                .unwrap_or_else(|_| Err("Subagent monitor panicked before completion.".into()))
            };
            if result.is_err() {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| guard.run.abort()));
            }
            guard.complete(result);
        }
    }

    fn abort(&self) {
        // Resolve the current owner's generation for new control operations.
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
                runtime.coordination().record_usage(
                    &self.owner,
                    outcome.usage.clone(),
                    json!({
                        "source": "subagent",
                        "runId": self.run_id,
                        "childSessionId": outcome.session_id,
                        "agent": details.get("agent").cloned(),
                        "depth": details.get("depth").cloned(),
                    }),
                )
            })
            .err()
            .map(|error| format!("Subagent usage accounting failed: {error}"))
    }

    async fn execute(&self) -> RunResult {
        let deadline = async {
            match self.timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
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
            .details;
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
                return Err(error);
            }
            () = deadline => {
                self.abort();
                // Continue the already-authorized wait across a parent reload;
                // do not acquire another wait using its retired generation.
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
                return Ok(result);
            }
        };
        let mut warnings = self
            .runtime
            .upgrade()
            .map(|runtime| runtime.warnings(&self.run_id))
            .unwrap_or_default();
        warnings.extend(self.account_outcome(&outcome, &mut details));
        details["warnings"] = json!(warnings);
        details["state"] = json!(if outcome.aborted {
            "aborted"
        } else {
            "completed"
        });
        details["aborted"] = json!(outcome.aborted);
        let mut result = if outcome.aborted {
            ToolResult::error("Subagent was aborted before it completed.")
        } else {
            ToolResult::text(with_warnings(final_text(&outcome.messages), &warnings))
        };
        result.details = Some(details);
        Ok(result)
    }
}
