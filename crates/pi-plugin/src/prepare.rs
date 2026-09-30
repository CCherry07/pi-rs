use std::path::{Path, PathBuf};

/// Host-supplied environment for preparing an unpublished plugin instance.
///
/// Plugins own their configuration and file layout beneath the agent directory.
/// Constructing this context performs no discovery or filesystem operations.
#[derive(Debug, Clone)]
pub struct PrepareContext {
    workspace: crate::WorkspaceSnapshot,
    agent_dir: PathBuf,
    project_trusted: bool,
}

impl PrepareContext {
    pub fn new(
        workspace: crate::WorkspaceSnapshot,
        agent_dir: impl Into<PathBuf>,
        project_trusted: bool,
    ) -> Self {
        Self {
            workspace,
            agent_dir: agent_dir.into(),
            project_trusted,
        }
    }

    pub fn workspace(&self) -> &crate::WorkspaceSnapshot {
        &self.workspace
    }

    pub fn cwd(&self) -> &Path {
        self.workspace.cwd()
    }

    /// The host's agent profile root, shared by plugins and distinct from cwd.
    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    /// The host's resolved decision to enable project-local settings and resources.
    /// This is not a filesystem or tool-execution permission.
    pub fn project_trusted(&self) -> bool {
        self.project_trusted
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("invalid plugin options: {0}")]
    InvalidOptions(String),
    #[error("plugin initialization failed: {0}")]
    Initialization(String),
}

pub type PrepareResult<T> = Result<T, PrepareError>;

impl PrepareError {
    pub fn initialization(message: impl Into<String>) -> Self {
        Self::Initialization(message.into())
    }
}
