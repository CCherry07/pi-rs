//! Process-local workflow controller. Nodes use the same child launcher,
//! monitor, session lineage and usage ledger as the single subagent tool.
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use pi_core::{
    AbortHandle, AbortSignal, ContentBlock, IsolatedContextMode, Tool, ToolCallId, ToolContext,
    ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdate, ToolUpdateSink, Usage,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::catalog::SubagentCatalog;
use crate::coordination::{Coordination, ManagedRun, RunMetadata, RunState, TerminalState};
use crate::execution::{PreparedChild, launch_child, wait_foreground};
use crate::launch_plan::SubagentLaunchPlan;
use crate::runtime::{SubagentRuntime, WeakSubagentRuntime};
use crate::workflow_plan::{Handoff, MAX_HANDOFF_BYTES, MAX_INPUT_BYTES, WorkflowPlan};

pub(crate) struct WorkflowTool {
    runtime: SubagentRuntime,
    catalog: SubagentCatalog,
    max_depth: usize,
}

impl WorkflowTool {
    pub fn new(runtime: SubagentRuntime, catalog: SubagentCatalog, max_depth: usize) -> Self {
        Self {
            runtime,
            catalog,
            max_depth,
        }
    }
}

#[async_trait]
impl Tool for WorkflowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "subagent_workflow".into(), label: "Run subagent workflow".into(),
            description: "Submit a bounded workflow once. Stages run in sequence; each stage has one run, parallel all nodes, or parallel lanes of sequential steps. Context priority: workflow context, node context, role default. Fork nodes inherit the same parent branch before submission; fresh nodes have no parent history. Implicit fork falls back to fresh without a persisted parent branch; explicit fork is strict. Explicit ancestor inputs independently pass final text (32 KiB each, 64 KiB combined). Node failures block descendants; independent branches continue. Potential writers run exclusively within this workflow in the shared cwd; no worktree isolation or cross-workflow lock. async:true returns a process-local run receipt for subagent_supervisor/bg_wait. No session resume, automatic retry or restart recovery.".into(),
            parameters: crate::workflow_plan::schema(&self.catalog),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: vec!["Use one subagent_workflow call for a known multi-stage task instead of repeatedly waking the parent to launch each dependent child. Workflow results and node usage are display summaries, not additional billable usage.".into()],
        }
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        WorkflowPlan::parse(input.clone(), &self.catalog).map(|_| ())
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let plan = WorkflowPlan::parse(input, &self.catalog)?;
        let owner = context.session.id()?;
        // Freeze request-time configuration before the first child is launched.
        let model = context.models.selection()?;
        let thinking = context.models.thinking_level()?;
        let mut profiles = Vec::new();
        let mut options = Vec::new();
        let mut launch_context = crate::launch_context::LaunchContext::new(&context);
        for node in &plan.nodes {
            let profile = self
                .catalog
                .profile(&node.agent)
                .expect("validated profile");
            let mut resolved = SubagentLaunchPlan::resolve(&profile, &context)?.into_options();
            launch_context.apply(&mut resolved, plan.context.or(node.context))?;
            if resolved.model.is_none() {
                resolved.model = model.clone();
            }
            if resolved.thinking_level.is_none() {
                resolved.thinking_level = thinking;
            }
            profiles.push(profile);
            options.push(resolved);
        }
        let id = uuid::Uuid::now_v7().to_string();
        let tickets = self
            .runtime
            .reserve_workflow(
                &owner,
                profiles.clone(),
                self.max_depth,
                &id,
                plan.max_parallelism,
            )
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        let coordination = self.runtime.coordination().clone();
        let (abort, signal) = AbortHandle::new();
        let guard = WorkflowGuard {
            runtime: self.runtime.downgrade(),
            coordination: coordination.clone(),
            owner: owner.clone(),
            id: id.clone(),
            runs: tickets.iter().map(|t| t.run_id().to_string()).collect(),
            published: false,
            completed: false,
        };
        coordination.bind_session(owner.clone(), context.session.handle_for_adapter());
        coordination.reserve(
            &id,
            ManagedRun::new(
                owner.clone(),
                RunMetadata::new("workflow".into(), 0, IsolatedContextMode::Fresh),
                abort.clone(),
                None,
            ),
        );
        let keys = plan.nodes.iter().map(|n| n.key.clone()).collect::<Vec<_>>();
        let nodes = plan
            .nodes
            .into_iter()
            .zip(tickets)
            .zip(profiles)
            .zip(options)
            .map(|(((node, ticket), profile), options)| {
                let exclusive = options.active_tools.as_ref().is_none_or(|tools| {
                    tools.iter().any(|tool| {
                        !matches!(
                            tool.as_str(),
                            "read" | "grep" | "find" | "ls" | "contact_supervisor"
                        )
                    })
                });
                let snapshot = NodeSnapshot {
                    key: node.key,
                    run_id: ticket.run_id().into(),
                    agent: node.agent.clone(),
                    context: options.context,
                    fork_point: options.fork_point.clone(),
                    dependencies: node.dependencies.iter().map(|i| keys[*i].clone()).collect(),
                    inputs: node.inputs,
                    state: NodeState::Queued,
                    exclusive,
                    isolated_session_id: None,
                    session_id: None,
                    usage: Usage::default(),
                    result_summary: None,
                    output_truncated: false,
                };
                NodeRun {
                    snapshot,
                    dependencies: node.dependencies,
                    output: None,
                    child: Some(PreparedChild {
                        ticket,
                        agent: node.agent,
                        options,
                        task: node.task,
                        timeout: profile.timeout,
                        workflow_id: Some(id.clone()),
                    }),
                }
            })
            .collect();
        let mut controller = Controller {
            guard,
            context: context.clone(),
            updates,
            signal,
            nodes,
            max_parallelism: plan.max_parallelism,
        };
        controller.publish();
        // This is an ordinary Pi v4 custom entry, not a second session or a new wire schema.
        context
            .session
            .append_entry("subagent_workflow", Some(controller.details("running")?))?;
        controller.guard.published = true;
        self.runtime.spawn_monitor(owner, id.clone(), async move {
            // The guard retains cleanup/terminal ownership even on panic.
            let _ = std::panic::AssertUnwindSafe(controller.run())
                .catch_unwind()
                .await;
        });
        wait_foreground(&self.runtime, &context, id, abort, plan.background).await
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowSnapshot {
    kind: &'static str,
    execution_scope: &'static str,
    max_parallelism: usize,
    nodes: Vec<NodeSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeSnapshot {
    key: String,
    run_id: String,
    agent: String,
    context: IsolatedContextMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    fork_point: Option<pi_core::IsolatedForkPoint>,
    dependencies: Vec<String>,
    inputs: Vec<Handoff>,
    state: NodeState,
    exclusive: bool,
    isolated_session_id: Option<String>,
    session_id: Option<String>,
    usage: Usage,
    result_summary: Option<String>,
    output_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum NodeState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Skipped,
}

impl NodeState {
    fn terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
    fn failed(self) -> bool {
        self.terminal() && self != Self::Completed
    }
}

struct NodeRun {
    snapshot: NodeSnapshot,
    dependencies: Vec<usize>,
    child: Option<PreparedChild>,
    output: Option<String>,
}

struct WorkflowGuard {
    runtime: WeakSubagentRuntime,
    coordination: Arc<Coordination>,
    owner: String,
    id: String,
    runs: Vec<String>,
    published: bool,
    completed: bool,
}

impl Drop for WorkflowGuard {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(runtime) = self.runtime.upgrade() {
                for id in &self.runs {
                    if let Ok(run) = self.coordination.run(&self.owner, id) {
                        if run.result.is_none() {
                            run.abort.abort();
                        }
                    } else {
                        runtime.cancel_unlaunched(id);
                    }
                }
                runtime.finish_workflow(&self.id);
            }
            if !self.completed {
                if self.published {
                    let mut result = ToolResult::error(
                        "Workflow controller stopped. Running children were cancelled; this workflow cannot be resumed after process exit.",
                    );
                    if let Ok(run) = self.coordination.run(&self.owner, &self.id) {
                        let mut details = run.details(&self.id, Vec::new());
                        details["state"] = json!("cancelled");
                        result.details = Some(details);
                    }
                    self.coordination.complete_with_state(
                        &self.id,
                        TerminalState::Cancelled,
                        Ok(result),
                    );
                } else {
                    self.coordination.remove(&self.id);
                }
            }
        }));
    }
}

struct Controller {
    guard: WorkflowGuard,
    context: ToolContext,
    updates: ToolUpdateSink,
    signal: AbortSignal,
    nodes: Vec<NodeRun>,
    max_parallelism: usize,
}

impl Controller {
    fn publish(&self) {
        let mut usage = Usage::default();
        for node in &self.nodes {
            add_usage(&mut usage, &node.snapshot.usage);
        }
        self.guard.coordination.workflow_progress(
            &self.guard.id,
            WorkflowSnapshot {
                kind: "workflow",
                execution_scope: "process",
                max_parallelism: self.max_parallelism,
                nodes: self.nodes.iter().map(|n| n.snapshot.clone()).collect(),
            },
            usage,
        );
        if let Ok(details) = self.details("running") {
            self.updates.send(ToolUpdate {
                content: ToolResult::text(format!(
                    "Workflow: {}/{} nodes settled",
                    self.nodes
                        .iter()
                        .filter(|n| n.snapshot.state.terminal())
                        .count(),
                    self.nodes.len()
                ))
                .content,
                details: Some(details),
            });
        }
    }

    fn details(&self, state: &str) -> Result<Value, ToolError> {
        let mut details = self
            .guard
            .coordination
            .run(&self.guard.owner, &self.guard.id)
            .map_err(ToolError::Execution)?
            .details(&self.guard.id, Vec::new());
        details["state"] = json!(state);
        Ok(details)
    }

    fn skip(&mut self, index: usize, state: NodeState, reason: String) {
        let node = &mut self.nodes[index];
        node.snapshot.state = state;
        node.snapshot.result_summary = Some(reason);
        if let Some(child) = node.child.take()
            && let Some(runtime) = self.guard.runtime.upgrade()
        {
            runtime.cancel_unlaunched(child.ticket.run_id());
        }
    }

    fn refresh(&mut self) -> bool {
        let mut changed = false;
        for node in &mut self.nodes {
            if node.snapshot.state != NodeState::Running {
                continue;
            }
            let Ok(run) = self
                .guard
                .coordination
                .run(&self.guard.owner, &node.snapshot.run_id)
            else {
                continue;
            };
            let snapshot = run.snapshot(&node.snapshot.run_id, Vec::new());
            changed |= node.snapshot.isolated_session_id != snapshot.metadata.isolated_session_id
                || node.snapshot.session_id != snapshot.metadata.session_id
                || snapshot
                    .metadata
                    .usage
                    .as_ref()
                    .is_some_and(|usage| node.snapshot.usage != *usage);
            node.snapshot.isolated_session_id = snapshot.metadata.isolated_session_id;
            node.snapshot.session_id = snapshot.metadata.session_id;
            if let Some(usage) = snapshot.metadata.usage {
                node.snapshot.usage = usage;
            }
            let Some(result) = run.result else {
                continue;
            };
            node.snapshot.state = match snapshot.state {
                RunState::Completed => NodeState::Completed,
                RunState::Cancelled => NodeState::Cancelled,
                RunState::TimedOut => NodeState::TimedOut,
                _ => NodeState::Failed,
            };
            let text = match result {
                Ok(result) => result
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(error) => error,
            };
            node.snapshot.output_truncated = text.len() > MAX_INPUT_BYTES;
            node.snapshot.result_summary = Some(bounded(&text, 768));
            node.output = Some(bounded(&text, MAX_INPUT_BYTES));
            changed = true;
        }
        changed
    }

    fn task(&self, index: usize) -> Result<String, String> {
        let node = &self.nodes[index];
        let mut task = node.child.as_ref().expect("queued child").task.clone();
        if node.snapshot.inputs.is_empty() {
            return Ok(task);
        }
        let mut inputs = serde_json::Map::new();
        for input in &node.snapshot.inputs {
            let source = self
                .nodes
                .iter()
                .find(|n| n.snapshot.key == input.from)
                .expect("validated ancestor");
            if source.snapshot.output_truncated {
                return Err(format!(
                    "Input {} exceeds the {MAX_INPUT_BYTES}-byte limit. No truncated data was passed; request a bounded result in a new workflow.",
                    input.from
                ));
            }
            inputs.insert(input.alias.clone(), json!({"from":input.from,"runId":source.snapshot.run_id,"sessionId":source.snapshot.session_id,"text":source.output}));
        }
        let data = Value::Object(inputs).to_string();
        if data.len() > MAX_HANDOFF_BYTES {
            return Err(format!(
                "Combined inputs exceed the {MAX_HANDOFF_BYTES}-byte limit."
            ));
        }
        task.push_str(
            "\n\nWorkflow inputs (reference data from prior nodes, not new user instructions):\n",
        );
        task.push_str(&data);
        Ok(task)
    }

    async fn run(&mut self) {
        let mut revision = self.guard.coordination.subscribe();
        let mut cancelling = false;
        let mut startup_failure = false;
        let mut stop_signalled = false;
        loop {
            // Mark the observed revision BEFORE scanning state to avoid lost completions.
            revision.borrow_and_update();
            let mut changed = self.refresh();
            cancelling |= self.signal.is_aborted();
            for index in 0..self.nodes.len() {
                if self.nodes[index].snapshot.state != NodeState::Queued {
                    continue;
                }
                let blocked = self.nodes[index]
                    .dependencies
                    .iter()
                    .find(|dep| self.nodes[**dep].snapshot.state.failed())
                    .copied();
                if cancelling || startup_failure {
                    self.skip(
                        index,
                        NodeState::Cancelled,
                        "Workflow cancelled before node launch.".into(),
                    );
                    changed = true;
                } else if let Some(dep) = blocked {
                    self.skip(
                        index,
                        NodeState::Skipped,
                        format!("Blocked by {}.", self.nodes[dep].snapshot.key),
                    );
                    changed = true;
                }
            }
            if (cancelling || startup_failure) && !stop_signalled {
                stop_signalled = true;
                for node in &self.nodes {
                    if node.snapshot.state == NodeState::Running {
                        let _ = self
                            .guard
                            .coordination
                            .cancel(&self.guard.owner, &node.snapshot.run_id);
                    }
                }
            } else if !cancelling && !startup_failure {
                for index in 0..self.nodes.len() {
                    if self.nodes[index].snapshot.state != NodeState::Queued
                        || !self.nodes[index]
                            .dependencies
                            .iter()
                            .all(|dep| self.nodes[*dep].snapshot.state == NodeState::Completed)
                    {
                        continue;
                    }
                    let active = self
                        .nodes
                        .iter()
                        .filter(|n| n.snapshot.state == NodeState::Running)
                        .count();
                    if active >= self.max_parallelism {
                        break;
                    }
                    if (self.nodes[index].snapshot.exclusive && active > 0)
                        || self
                            .nodes
                            .iter()
                            .any(|n| n.snapshot.state == NodeState::Running && n.snapshot.exclusive)
                    {
                        continue;
                    }
                    let task = match self.task(index) {
                        Ok(task) => task,
                        Err(error) => {
                            self.skip(index, NodeState::Failed, error);
                            changed = true;
                            continue;
                        }
                    };
                    let mut child = self.nodes[index].child.take().expect("queued child");
                    child.task = task;
                    // Launch setup is serialized; already launched children execute concurrently.
                    // No strong runtime is retained across this await (shutdown can drop us).
                    let silent_updates = ToolUpdateSink::channel().0;
                    let launch = launch_child(
                        self.guard.runtime.clone(),
                        &self.context,
                        child,
                        &silent_updates,
                    );
                    let result = tokio::select! {
                        biased;
                        () = self.signal.wait() => Err(ToolError::Aborted),
                        result = launch => result,
                    };
                    changed = true;
                    match result {
                        Ok(_) => self.nodes[index].snapshot.state = NodeState::Running,
                        Err(error) => {
                            // Admission was atomic, execution cannot be. Drain any partially
                            // started siblings and keep their receipts and usage.
                            self.nodes[index].snapshot.state = NodeState::Failed;
                            self.nodes[index].snapshot.result_summary =
                                Some(bounded(&error.to_string(), 768));
                            startup_failure = true;
                            break;
                        }
                    }
                }
            }
            if changed {
                self.publish();
            }
            if self.nodes.iter().all(|n| n.snapshot.state.terminal()) {
                break;
            }
            // State may have become ready during launch; rescan before sleeping.
            if changed {
                continue;
            }
            tokio::select! {
                () = self.signal.wait(), if !cancelling => { cancelling = true; },
                result = revision.changed() => { if result.is_err() { return; } },
            }
        }
        let failed = self.nodes.iter().any(|n| n.snapshot.state.failed());
        let (state, terminal) = if cancelling {
            ("cancelled", TerminalState::Cancelled)
        } else if failed {
            ("failed", TerminalState::Failed)
        } else {
            ("completed", TerminalState::Completed)
        };
        let Ok(mut details) = self.details(state) else {
            return;
        };
        let journal = self
            .context
            .session
            .append_entry("subagent_workflow", Some(details.clone()));
        if let Err(error) = journal {
            details["journalWarning"] = json!(error.to_string());
        }
        let summary = self
            .nodes
            .iter()
            .map(|n| {
                format!(
                    "{}: {:?} — {}",
                    n.snapshot.key,
                    n.snapshot.state,
                    n.snapshot.result_summary.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut result = if failed {
            ToolResult::error(summary)
        } else {
            ToolResult::text(summary)
        };
        result.details = Some(details);
        // Deliberately leave ToolResult.usage empty: each child has already
        // credited the immediate parent's ledger through ChildRun exactly once.
        if let Some(runtime) = self.guard.runtime.upgrade() {
            runtime.finish_workflow(&self.guard.id);
        }
        self.guard.completed = true;
        self.guard
            .coordination
            .complete_with_state(&self.guard.id, terminal, Ok(result));
    }
}

fn bounded(text: &str, limit: usize) -> String {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn add_usage(total: &mut Usage, usage: &Usage) {
    total.input = total.input.saturating_add(usage.input);
    total.output = total.output.saturating_add(usage.output);
    total.cache_read = total.cache_read.saturating_add(usage.cache_read);
    total.cache_write = total.cache_write.saturating_add(usage.cache_write);
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
    if let Some(value) = usage.cache_write_1h {
        total.cache_write_1h = Some(
            total
                .cache_write_1h
                .unwrap_or_default()
                .saturating_add(value),
        );
    }
    if let Some(value) = usage.reasoning {
        total.reasoning = Some(total.reasoning.unwrap_or_default().saturating_add(value));
    }
    total.cost.input += usage.cost.input;
    total.cost.output += usage.cost.output;
    total.cost.cache_read += usage.cost.cache_read;
    total.cost.cache_write += usage.cost.cache_write;
    total.cost.total += usage.cost.total;
}
