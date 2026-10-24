//! Registered commands for controlling a session's direct child agents.
use std::sync::Arc;

use async_trait::async_trait;
use pi_plugin::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel,
    RegisterContext,
};
use serde::Deserialize;

use crate::SubagentRuntime;

#[derive(Clone, Copy)]
enum Action {
    Interrupt,
    FollowUp,
}

struct SubagentCommand {
    runtime: SubagentRuntime,
    action: Action,
}

pub(crate) fn register_commands(
    context: &mut RegisterContext<'_>,
    runtime: &SubagentRuntime,
) -> pi_plugin::Result<()> {
    for action in [Action::Interrupt, Action::FollowUp] {
        context.register_command(Arc::new(SubagentCommand {
            runtime: runtime.clone(),
            action,
        }))?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowUp {
    target: String,
    task: String,
}

fn required(value: &str, field: &str) -> Result<(), CommandError> {
    if value.trim().is_empty() {
        return Err(CommandError::InvalidArguments(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

#[async_trait]
impl Command for SubagentCommand {
    fn spec(&self) -> CommandSpec {
        match self.action {
            Action::Interrupt => CommandSpec {
                name: "subagents:interrupt".into(),
                description: "Stop a direct child's active turn".into(),
                argument_hint: Some("{\"target\":\"agent-id\"}".into()),
            },
            Action::FollowUp => CommandSpec {
                name: "subagents:followup".into(),
                description: "Give a direct child more work in its existing session".into(),
                argument_hint: Some("{\"target\":\"agent-id\",\"task\":\"...\"}".into()),
            },
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        context
            .signal()
            .check()
            .map_err(|_| CommandError::Aborted)?;
        let owner = context.session.id()?;
        let notice = match self.action {
            Action::Interrupt => {
                let input: Target = serde_json::from_str(&arguments)
                    .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
                required(&input.target, "target")?;
                self.runtime
                    .interrupt(&owner, &input.target)
                    .map_err(CommandError::Execution)?;
                "Agent interruption requested."
            }
            Action::FollowUp => {
                let input: FollowUp = serde_json::from_str(&arguments)
                    .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
                required(&input.target, "target")?;
                required(&input.task, "task")?;
                let (started, _) = self
                    .runtime
                    .follow_up(&owner, &input.target, input.task)
                    .await
                    .map_err(CommandError::Execution)?;
                if started {
                    "Follow-up started in the existing agent session."
                } else {
                    "Follow-up delivered to the running agent."
                }
            }
        };
        context.ui.notify(NoticeLevel::Info, notice)?;
        Ok(CommandOutcome::Handled)
    }
}
