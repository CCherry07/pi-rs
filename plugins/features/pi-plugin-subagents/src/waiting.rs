//! Shared, snapshot-based wait decisions. Watch notifications are only wake hints.
use pi_core::ToolResult;
use serde::Serialize;
use serde_json::json;

use crate::coordination::{RunResult, SupervisorRequest};
use crate::run_state::ManagedRun;

pub(crate) enum WaitDecision {
    Pending,
    Ready(Box<RunResult>),
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WaitDeadline {
    pub deadline_at: u64,
}

pub(crate) fn armed(id: &str, deadline: WaitDeadline, reused: bool) -> ToolResult {
    let mut result = ToolResult::text(format!(
        "Watching subagent {id} without blocking. Completion and decisions use the existing notifications; wait expiry attempts a separate reminder without cancelling work. Repeated registration keeps the original deadline."
    ));
    result.details = Some(json!({
        "runId": id, "nonBlocking": true, "armed": true,
        "deadlineAt": deadline.deadline_at, "reused": reused,
    }));
    result
}

pub(crate) fn decide(
    ids: &[String],
    runs: &[&ManagedRun],
    pending: Vec<SupervisorRequest>,
    all: bool,
) -> WaitDecision {
    if !pending.is_empty() {
        let mut result = ToolResult::text(
            "Subagent attention required. Reply to pending supervisor requests, then wait on the same run. Do not launch a replacement.",
        );
        result.details = Some(json!({"state":"needs_attention","pending":pending,"runIds":ids}));
        return WaitDecision::Ready(Box::new(Ok(result)));
    }
    let finished = runs.iter().filter(|run| run.result.is_some()).count();
    if !runs.is_empty() && ((all && finished != runs.len()) || (!all && finished == 0)) {
        return WaitDecision::Pending;
    }
    if runs.len() == 1 {
        return WaitDecision::Ready(Box::new(runs[0].result.clone().expect("terminal run")));
    }
    let completed = runs
        .iter()
        .filter_map(|run| run.result.as_ref())
        .map(|result| match result {
            Ok(result) => {
                json!({"content":result.content,"details":result.details,"isError":result.is_error})
            }
            Err(error) => json!({"error":error}),
        })
        .collect::<Vec<_>>();
    let mut result = ToolResult::text(if runs.is_empty() {
        "No active subagent runs."
    } else {
        "Subagent wait completed."
    });
    result.details = Some(json!({"state":"completed","results":completed,"runIds":ids}));
    WaitDecision::Ready(Box::new(Ok(result)))
}

pub(crate) fn window_elapsed(ids: &[String]) -> ToolResult {
    let mut result = ToolResult::text(
        "Wait window elapsed; subagent work keeps going. Call bg_wait again on the same run.",
    );
    result.details = Some(json!({"state":"running","timedOut":true,"runIds":ids}));
    result
}
