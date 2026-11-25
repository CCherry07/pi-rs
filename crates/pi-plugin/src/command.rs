mod completion;
pub use completion::CommandCompletions;

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    AbortSignal, CommandContextParts, CommandModelsContext, CommandSessionContext,
    PluginContextHandle, UiContext,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
}

/// The resource that contributes a command, preserved by generation registries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSourceInfo {
    pub path: String,
    pub source: String,
    pub scope: CommandSourceScope,
    pub origin: CommandSourceOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandSourceScope {
    User,
    Project,
    Temporary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandSourceOrigin {
    Package,
    TopLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandSource {
    Extension,
    Prompt,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMetadata {
    pub source: CommandSource,
    pub source_info: CommandSourceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCatalogEntry {
    pub spec: CommandSpec,
    pub metadata: CommandMetadata,
}

#[derive(Clone)]
pub struct CommandContext {
    workspace: crate::WorkspaceSnapshot,
    abort_signal: AbortSignal,
    pub session: CommandSessionContext,
    pub models: CommandModelsContext,
    pub ui: UiContext,
}

impl CommandContext {
    /// Constructs a context for running a command outside a Pi session.
    ///
    /// Session, model, and presentation capabilities are intentionally
    /// unavailable on standalone contexts.
    pub fn standalone(cwd: PathBuf, abort_signal: AbortSignal) -> Self {
        Self::with_plugin_context(cwd, abort_signal, CommandContextParts::unavailable())
    }

    #[doc(hidden)]
    pub fn with_plugin_context(
        cwd: PathBuf,
        abort_signal: AbortSignal,
        context: CommandContextParts,
    ) -> Self {
        Self {
            workspace: context.workspace_or_cwd(&cwd),
            abort_signal,
            session: context.session,
            models: context.models,
            ui: context.ui,
        }
    }

    pub fn workspace(&self) -> &crate::WorkspaceSnapshot {
        &self.workspace
    }

    pub fn cwd(&self) -> &std::path::Path {
        self.workspace.cwd()
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.abort_signal
    }

    #[doc(hidden)]
    pub fn plugin_context_handle(&self) -> PluginContextHandle {
        self.session.handle_for_adapter()
    }
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
    #[error(transparent)]
    Context(#[from] crate::PluginContextError),
}

#[async_trait]
pub trait Command: Send + Sync {
    fn spec(&self) -> CommandSpec;

    /// Resource owners supply provenance; native commands default to their plugin identity.
    fn metadata(&self) -> Option<CommandMetadata> {
        None
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError>;
}
