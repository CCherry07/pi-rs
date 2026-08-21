use std::path::PathBuf;

use async_trait::async_trait;

use crate::AbortSignal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
}

#[derive(Clone)]
pub struct CommandContext {
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// The command consumed the input and no Agent run should start.
    Handled,
    /// Replace the slash command with text that is sent to the Agent.
    TransformInput(String),
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("command aborted")]
    Aborted,
    #[error("invalid command arguments: {0}")]
    InvalidArguments(String),
    #[error("command failed: {0}")]
    Execution(String),
}

#[async_trait]
pub trait Command: Send + Sync {
    fn spec(&self) -> CommandSpec;

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError>;
}
