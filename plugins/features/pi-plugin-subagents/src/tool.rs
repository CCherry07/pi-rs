use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    CustomMessageContent, IsolatedContextMode, IsolatedSessionRequest, Tool, ToolCallId,
    ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdate, ToolUpdateSink,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::SubagentCatalog;
use crate::launch_context::LaunchContext;
use crate::launch_plan::SubagentLaunchPlan;
use crate::runtime::{SubagentRuntime, WaitEvaluation, WaitMode, result_with_details};

#[derive(Clone, Copy)]
pub(crate) enum AgentToolKind {
    Spawn,
    SendMessage,
    FollowUp,
    Wait,
    Interrupt,
    List,
}

pub(crate) struct AgentTool {
    runtime: SubagentRuntime,
    catalog: SubagentCatalog,
    max_depth: usize,
    kind: AgentToolKind,
}

impl AgentTool {
    pub(crate) fn new(
        runtime: SubagentRuntime,
        catalog: SubagentCatalog,
        max_depth: usize,
        kind: AgentToolKind,
    ) -> Self {
        Self {
            runtime,
            catalog,
            max_depth,
            kind,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpawnInput {
    agent: String,
    task: String,
    context: Option<IsolatedContextMode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessageInput {
    target: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowUpInput {
    target: String,
    task: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitInput {
    targets: Vec<String>,
    #[serde(default)]
    mode: WaitInputMode,
    timeout_ms: Option<u64>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WaitInputMode {
    #[default]
    Any,
    All,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetInput {
    target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[async_trait]
impl Tool for AgentTool {
    fn spec(&self) -> ToolSpec {
        let profiles = self.catalog.profile_names();
        let (name, label, description, properties, required, guidelines) = match self.kind {
            AgentToolKind::Spawn => (
                "spawn_agent",
                "Spawn agent",
                "Start one configured child agent asynchronously. The returned agent id names a reusable child session; call wait_agent for results and followup_task for later turns.",
                json!({
                    "agent":{"type":"string","enum":profiles},
                    "task":{"type":"string","minLength":1},
                    "context":{"type":"string","enum":["fresh","fork"]}
                }),
                vec!["agent", "task"],
                vec!["Use multiple spawn_agent calls in the same assistant response for independent parallel work. Spawn returns before the child finishes.".to_string()],
            ),
            AgentToolKind::SendMessage => (
                "send_message",
                "Message agent",
                "Send information without starting a new child turn. Use an exact direct-child agent id, or target parent from inside an assigned child.",
                json!({
                    "target":{"type":"string","minLength":1},
                    "message":{"type":"string","minLength":1}
                }),
                vec!["target", "message"],
                vec![],
            ),
            AgentToolKind::FollowUp => (
                "followup_task",
                "Follow up agent",
                "Give an existing direct child more work. An idle child starts a new turn in the same session; a running child receives a durable follow-up in its current turn.",
                json!({
                    "target":{"type":"string","minLength":1},
                    "task":{"type":"string","minLength":1}
                }),
                vec!["target", "task"],
                vec![],
            ),
            AgentToolKind::Wait => (
                "wait_agent",
                "Wait for agents",
                "Wait until any or all exact direct-child agent ids settle or send a message. Assigned children may wait on parent for a parent message. A timeout returns a normal running snapshot and never interrupts work.",
                json!({
                    "targets":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"string","minLength":1,"description":"Exact direct-child agent id, or parent inside an assigned child."}},
                    "mode":{"type":"string","enum":["any","all"],"default":"any"},
                    "timeoutMs":{"type":"integer","minimum":0,"maximum":3600000,"default":120000}
                }),
                vec!["targets"],
                vec!["mode:any is the collaboration equivalent of Promise.race. Re-evaluate the plan after each returned result or message.".to_string()],
            ),
            AgentToolKind::Interrupt => (
                "interrupt_agent",
                "Interrupt agent",
                "Request cancellation of one direct child's active turn while preserving the child session for future follow-ups.",
                json!({"target":{"type":"string","minLength":1}}),
                vec!["target"],
                vec![],
            ),
            AgentToolKind::List => (
                "list_agents",
                "List agents",
                "Return a read-only snapshot of the caller's descendant agent tree.",
                json!({}),
                vec![],
                vec![],
            ),
        };
        ToolSpec {
            name: name.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            parameters: json!({
                "type":"object",
                "properties":properties,
                "required":required,
                "additionalProperties":false
            }),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: guidelines,
        }
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        match self.kind {
            AgentToolKind::Spawn => parse::<SpawnInput>(input.clone()).map(|_| ()),
            AgentToolKind::SendMessage => parse::<SendMessageInput>(input.clone()).map(|_| ()),
            AgentToolKind::FollowUp => parse::<FollowUpInput>(input.clone()).map(|_| ()),
            AgentToolKind::Wait => {
                parse::<WaitInput>(input.clone()).and_then(|input| validate_wait(&input))
            }
            AgentToolKind::Interrupt => parse::<TargetInput>(input.clone()).map(|_| ()),
            AgentToolKind::List => parse::<EmptyInput>(input.clone()).map(|_| ()),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        match self.kind {
            AgentToolKind::Spawn => self.spawn(context, parse(input)?, updates).await,
            AgentToolKind::SendMessage => self.send(context, parse(input)?),
            AgentToolKind::FollowUp => self.follow_up(context, parse(input)?).await,
            AgentToolKind::Wait => self.wait(context, parse(input)?).await,
            AgentToolKind::Interrupt => self.interrupt(context, parse(input)?),
            AgentToolKind::List => self.list(context, parse(input)?),
        }
    }
}

impl AgentTool {
    async fn spawn(
        &self,
        context: ToolContext,
        input: SpawnInput,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let agent_name = non_empty(&input.agent, "agent")?;
        let task = non_empty(&input.task, "task")?;
        let profile = self.catalog.profile(agent_name).ok_or_else(|| {
            ToolError::InvalidArguments(format!("unknown agent profile {agent_name:?}"))
        })?;
        let mut options = SubagentLaunchPlan::resolve(&profile, &context)?.into_options();
        LaunchContext::new(&context).apply(&mut options, input.context)?;
        let owner = context.session.id()?;
        self.runtime
            .bind_session(owner.clone(), context.session.clone());
        let ticket = self
            .runtime
            .begin_launch(&owner, profile.clone(), self.max_depth)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let mut guard = SpawnGuard {
            runtime: self.runtime.clone(),
            id: ticket.id().to_string(),
            committed: false,
        };
        self.runtime.set_task(ticket.id(), task);
        let request =
            IsolatedSessionRequest::new(CustomMessageContent::Text(ticket.child_prompt(task)))
                .options(options);
        let handle = context.session.launch_isolated_session(request).await?;
        let turn = self
            .runtime
            .attach_handle(&owner, ticket.id(), handle.clone())
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let child_session_id = self
            .runtime
            .child_session_id(&owner, ticket.id())
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        self.runtime.spawn_monitor(
            owner.clone(),
            ticket.id().to_string(),
            turn.clone(),
            profile.timeout,
        );
        guard.committed = true;
        updates.send(ToolUpdate {
            content: vec![pi_core::ContentBlock::Text(pi_core::TextContent::new(
                format!("{} agent started", profile.name),
            ))],
            details: Some(json!({
                "agentId":ticket.id(),
                "agent":profile.name,
                "state":"running",
                "turnId":turn.id().as_str(),
                "sessionId":child_session_id
            })),
        });
        Ok(result_with_details(
            format!(
                "Agent {} started asynchronously. Use wait_agent with the exact id {} when you need its result.",
                profile.name,
                ticket.id()
            ),
            json!({
                "agentId":ticket.id(),
                "isolatedSessionId":handle.id().as_str(),
                "sessionId":child_session_id,
                "agent":profile.name,
                "depth":ticket.depth(),
                "state":"running",
                "turnId":turn.id().as_str()
            }),
        ))
    }

    fn send(&self, context: ToolContext, input: SendMessageInput) -> Result<ToolResult, ToolError> {
        let target = non_empty(&input.target, "target")?;
        let message = non_empty(&input.message, "message")?;
        let details = self
            .runtime
            .send_message(&context.session.id()?, target, message.to_string())
            .map_err(ToolError::Execution)?;
        Ok(result_with_details("Message accepted.", details))
    }

    async fn follow_up(
        &self,
        context: ToolContext,
        input: FollowUpInput,
    ) -> Result<ToolResult, ToolError> {
        let target = non_empty(&input.target, "target")?;
        let task = non_empty(&input.task, "task")?;
        let (started, turn_id) = self
            .runtime
            .follow_up(&context.session.id()?, target, task.to_string())
            .await
            .map_err(ToolError::Execution)?;
        Ok(result_with_details(
            if started {
                "Follow-up started a new turn in the existing agent session."
            } else {
                "Follow-up joined the agent's active turn."
            },
            json!({"agentId":target,"turnId":turn_id,"started":started}),
        ))
    }

    async fn wait(&self, context: ToolContext, input: WaitInput) -> Result<ToolResult, ToolError> {
        validate_wait(&input)?;
        let owner = context.session.id()?;
        self.runtime
            .validate_targets(&owner, &input.targets)
            .map_err(ToolError::Execution)?;
        let mode = match input.mode {
            WaitInputMode::Any => WaitMode::Any,
            WaitInputMode::All => WaitMode::All,
        };
        let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(120_000));
        let (_registration, mut changed) = self.runtime.register_wait(&owner);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            match self
                .runtime
                .evaluate_wait(&owner, &input.targets, mode)
                .map_err(ToolError::Execution)?
            {
                WaitEvaluation::Ready { agents, messages } => {
                    return Ok(result_with_details(
                        if messages.is_empty() {
                            "Agent wait condition satisfied."
                        } else {
                            "Agent message requires attention."
                        },
                        json!({"state":"ready","mode":match mode { WaitMode::Any => "any", WaitMode::All => "all" },"agents":agents,"messages":messages}),
                    ));
                }
                WaitEvaluation::Pending if timeout.is_zero() => {
                    return Ok(result_with_details(
                        "Agent work is still running.",
                        json!({"state":"running","timedOut":true,"agents":self.runtime.list(&owner)}),
                    ));
                }
                WaitEvaluation::Pending => {}
            }
            tokio::select! {
                biased;
                () = context.signal().wait() => return Err(ToolError::Aborted),
                () = &mut deadline => {
                    return Ok(result_with_details(
                        "Wait window elapsed; agent work continues.",
                        json!({"state":"running","timedOut":true,"agents":self.runtime.list(&owner)}),
                    ));
                }
                changed_result = changed.changed() => {
                    if changed_result.is_err() {
                        return Err(ToolError::Execution("agent runtime closed while waiting".into()));
                    }
                }
            }
        }
    }

    fn interrupt(&self, context: ToolContext, input: TargetInput) -> Result<ToolResult, ToolError> {
        let target = non_empty(&input.target, "target")?;
        let snapshot = self
            .runtime
            .interrupt(&context.session.id()?, target)
            .map_err(ToolError::Execution)?;
        Ok(result_with_details(
            "Interrupt request accepted; the agent session remains reusable.",
            json!({"agent":snapshot}),
        ))
    }

    fn list(&self, context: ToolContext, _input: EmptyInput) -> Result<ToolResult, ToolError> {
        let agents = self.runtime.list(&context.session.id()?);
        Ok(result_with_details(
            if agents.is_empty() {
                "No descendant agents."
            } else {
                "Agent tree snapshot."
            },
            json!({"agents":agents}),
        ))
    }
}

struct SpawnGuard {
    runtime: SubagentRuntime,
    id: String,
    committed: bool,
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if !self.committed {
            self.runtime.cancel_launch(&self.id);
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::InvalidArguments(error.to_string()))
}

fn non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str, ToolError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ToolError::InvalidArguments(format!(
            "{field} must not be empty"
        )))
    } else {
        Ok(value)
    }
}

fn validate_wait(input: &WaitInput) -> Result<(), ToolError> {
    if input.targets.is_empty() || input.targets.len() > 8 {
        return Err(ToolError::InvalidArguments(
            "targets must contain between 1 and 8 exact agent ids".into(),
        ));
    }
    if input.targets.iter().any(|target| target.trim().is_empty()) {
        return Err(ToolError::InvalidArguments(
            "targets must not contain empty ids".into(),
        ));
    }
    let unique = input
        .targets
        .iter()
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != input.targets.len() {
        return Err(ToolError::InvalidArguments(
            "targets must contain unique exact agent ids".into(),
        ));
    }
    if input.timeout_ms.is_some_and(|timeout| timeout > 3_600_000) {
        return Err(ToolError::InvalidArguments(
            "timeoutMs must be at most 3600000".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_tools_have_distinct_strict_schemas() {
        let runtime = SubagentRuntime::default();
        let catalog = SubagentCatalog::builtins();
        let kinds = [
            AgentToolKind::Spawn,
            AgentToolKind::SendMessage,
            AgentToolKind::FollowUp,
            AgentToolKind::Wait,
            AgentToolKind::Interrupt,
            AgentToolKind::List,
        ];
        let names = kinds
            .into_iter()
            .map(|kind| {
                AgentTool::new(runtime.clone(), catalog.clone(), 4, kind)
                    .spec()
                    .name
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "spawn_agent",
                "send_message",
                "followup_task",
                "wait_agent",
                "interrupt_agent",
                "list_agents"
            ]
        );
    }

    #[test]
    fn wait_input_is_bounded_and_exact() {
        assert!(validate_wait(&WaitInput::default()).is_err());
        assert!(
            validate_wait(&WaitInput {
                targets: vec!["same".into(), "same".into()],
                ..WaitInput::default()
            })
            .is_err()
        );
    }
}
