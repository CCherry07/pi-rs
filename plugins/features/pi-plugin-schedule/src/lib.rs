#![forbid(unsafe_code)]

mod interface;
mod runtime;
mod schedule;
mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_core::{AgentPlugin, PluginId, RegisterContext};

pub use runtime::ScheduleSessionPlugin;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Context(#[from] pi_core::PluginContextError),
}

type Result<T> = std::result::Result<T, Error>;

/// Construction is side-effect free. Project schedules are visible only when
/// the host's existing project-trust decision allows project resources.
#[derive(Clone)]
pub struct ScheduleOptions {
    cwd: PathBuf,
    global: PathBuf,
    project: Option<PathBuf>,
}

impl ScheduleOptions {
    pub fn new(cwd: &Path, agent_dir: &Path, project_trusted: bool) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            global: agent_dir.join("schedule"),
            project: project_trusted.then(|| cwd.join(".pi/schedule")),
        }
    }

    fn store(&self, scope: interface::Scope) -> Result<store::Store> {
        let path = match scope {
            interface::Scope::Global => &self.global,
            interface::Scope::Project => self
                .project
                .as_ref()
                .ok_or_else(|| Error::Invalid("project schedules require project trust".into()))?,
        };
        Ok(store::Store::new(path.clone()))
    }
}

pub struct SchedulePlugin {
    options: ScheduleOptions,
}

impl SchedulePlugin {
    pub fn new(options: ScheduleOptions) -> Self {
        Self { options }
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for SchedulePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("schedule")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(interface::ScheduleTool::new(self.options.clone())))?;
        context.register_command(Arc::new(interface::ScheduleCommand::new(
            self.options.clone(),
        )))
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
