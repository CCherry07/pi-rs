//! Shared one-child launch and caller wait, used by both tool shapes.
use crate::runtime::{LaunchTicket, SubagentRuntime, WeakSubagentRuntime};
use pi_core::{
    ContentBlock, CustomMessageContent, IsolatedSessionOptions, IsolatedSessionRequest,
    TextContent, ToolContext, ToolError, ToolResult, ToolUpdate, ToolUpdateSink,
};
use serde_json::json;
use std::time::Duration;

struct RunGuard {
    runtime: WeakSubagentRuntime,
    run_id: String,
    transferred: bool,
}

impl RunGuard {
    fn reserved(runtime: WeakSubagentRuntime, ticket: &LaunchTicket) -> Self {
        Self {
            runtime,
            run_id: ticket.run_id().to_string(),
            transferred: false,
        }
    }

    fn mark_launched(&mut self) {
        self.transferred = true;
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.transferred
            && let Some(runtime) = self.runtime.upgrade()
        {
            runtime.coordination().remove(&self.run_id);
            runtime.cancel_unlaunched(&self.run_id);
        }
    }
}

pub(crate) struct ForegroundGuard(pub(crate) Option<pi_core::AbortHandle>);
impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        if let Some(abort) = &self.0 {
            abort.abort();
        }
    }
}

pub(crate) struct PreparedChild {
    pub ticket: LaunchTicket,
    pub agent: String,
    pub options: IsolatedSessionOptions,
    pub task: String,
    pub timeout: Option<Duration>,
    pub workflow_id: Option<String>,
}

pub(crate) async fn launch_child(
    weak: WeakSubagentRuntime,
    context: &ToolContext,
    child: PreparedChild,
    updates: &ToolUpdateSink,
) -> Result<(String, pi_core::AbortHandle), ToolError> {
    let PreparedChild {
        ticket,
        agent: profile_name,
        options,
        task,
        timeout,
        workflow_id,
    } = child;
    let mut guard = RunGuard::reserved(weak.clone(), &ticket);
    let parent_session_id = context.session.id()?;
    let runtime = weak
        .upgrade()
        .ok_or_else(|| ToolError::Execution("Subagent runtime closed.".into()))?;
    let run_id = ticket.run_id().to_string();
    let depth = ticket.depth();
    let context_mode = options.context;
    let (abort, signal) = pi_core::AbortHandle::new();
    runtime.coordination().reserve(
        &run_id,
        crate::coordination::ManagedRun::new(
            parent_session_id.clone(),
            crate::coordination::RunMetadata::new(profile_name.clone(), depth, context_mode),
            abort.clone(),
            timeout,
        )
        .in_workflow(workflow_id),
    );
    let request =
        IsolatedSessionRequest::new(CustomMessageContent::Text(ticket.child_prompt(&task)))
            .options(options);
    drop(runtime);
    let handle = match context.session.launch_isolated_session(request).await {
        Ok(handle) => handle,
        Err(error) => return Err(error.into()),
    };
    let Some(runtime) = weak.upgrade() else {
        let _ = handle.abort();
        return Err(ToolError::Execution(
            "Subagent runtime closed during launch.".into(),
        ));
    };
    runtime
        .coordination()
        .launched(&run_id, handle.id().as_str());
    updates.send(ToolUpdate {
        content: vec![ContentBlock::Text(TextContent::new(format!(
            "{} subagent running",
            profile_name
        )))],
        details: Some(json!({
            "runId": run_id,
            "isolatedSessionId": handle.id().as_str(),
            "agent": profile_name,
            "depth": depth,
            "context": context_mode,
            "state": "running"
        })),
    });

    runtime.spawn_monitor(
        parent_session_id.clone(),
        run_id.clone(),
        crate::child_run::ChildRun {
            runtime: runtime.downgrade(),
            run_id: run_id.clone(),
            owner: parent_session_id.clone(),
            handle,
            signal,
            timeout,
        }
        .monitor(),
    );
    guard.mark_launched();

    Ok((run_id, abort))
}

pub(crate) async fn wait_foreground(
    runtime: &SubagentRuntime,
    context: &ToolContext,
    run_id: String,
    abort: pi_core::AbortHandle,
    background: bool,
) -> Result<ToolResult, ToolError> {
    let parent_session_id = context.session.id()?;
    let mut changed = runtime.coordination().subscribe();
    let mut foreground = ForegroundGuard(Some(abort));
    if background {
        let result = runtime
            .coordination()
            .background(&parent_session_id, &run_id)
            .map_err(ToolError::Execution)?;
        foreground.0.take();
        return Ok(result);
    }
    loop {
        let run = runtime
            .coordination()
            .run(&parent_session_id, &run_id)
            .map_err(ToolError::Execution)?;
        if let Some(result) = run.result {
            foreground.0.take();
            return result.map_err(ToolError::Execution);
        }
        // All foreground waits owned by this session must yield together:
        // a parallel sibling otherwise prevents the parent's next turn.
        if !runtime
            .coordination()
            .pending(&parent_session_id)
            .is_empty()
        {
            let result = runtime
                .coordination()
                .detach(&parent_session_id, &run_id)
                .map_err(ToolError::Execution)?;
            foreground.0.take();
            return Ok(result);
        }
        tokio::select! {
            () = context.signal().wait() => return Err(ToolError::Aborted),
            _ = changed.changed() => {}
        }
    }
}
