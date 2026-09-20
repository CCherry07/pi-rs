use std::collections::HashMap;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::dictation::DictationState;
use crate::storage::{read_settings, read_workspaces};
use crate::types::{AppSettings, WorkspaceEntry};

pub(crate) struct AppState {
    pub(crate) workspaces: Mutex<HashMap<String, WorkspaceEntry>>,
    pub(crate) terminal_sessions:
        Mutex<HashMap<String, std::sync::Arc<crate::terminal::TerminalSession>>>,
    pub(crate) storage_path: PathBuf,
    pub(crate) settings_path: PathBuf,
    pub(crate) app_settings: Mutex<AppSettings>,
    pub(crate) dictation: Mutex<DictationState>,
}

impl AppState {
    pub(crate) fn load(app: &AppHandle) -> Result<Self, String> {
        let data_dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        let storage_path = data_dir.join("workspaces.json");
        let settings_path = data_dir.join("settings.json");
        let workspaces = read_workspaces(&storage_path)?;
        let app_settings = read_settings(&settings_path).unwrap_or_default();
        Ok(Self {
            workspaces: Mutex::new(workspaces),
            terminal_sessions: Mutex::new(HashMap::new()),
            storage_path,
            settings_path,
            app_settings: Mutex::new(app_settings),
            dictation: Mutex::new(DictationState::default()),
        })
    }
}

impl AppState {
    pub(crate) async fn project_info(
        &self,
        info: crate::types::WorkspaceInfo,
    ) -> Result<crate::types::WorkspaceInfo, String> {
        let project = self.project(&info.id).await?;
        info.with_project(project)
    }

    pub(crate) fn project_store(&self) -> pi_sdk::projects::ProjectStore {
        pi_sdk::projects::ProjectStore::at_path(self.storage_path.with_file_name("projects.json"))
    }

    pub(crate) async fn projects(&self) -> Result<Vec<pi_sdk::projects::Project>, String> {
        let entries = self.workspaces.lock().await;
        let store = self.project_store();
        store.import_missing(
            entries
                .values()
                .filter(|entry| !entry.kind.is_worktree())
                .map(|entry| {
                    pi_sdk::projects::Project::single_root(&entry.id, &entry.name, &entry.path)
                })
                .collect(),
        )?;
        let mut worktrees = Vec::new();
        for entry in entries.values().filter(|entry| entry.kind.is_worktree()) {
            let project = match entry.parent_id.as_ref().and_then(|id| entries.get(id)) {
                Some(parent) => store.get(&parent.id)?.with_worktree_from(
                    std::path::Path::new(&parent.path),
                    &entry.id,
                    &entry.name,
                    &entry.path,
                )?,
                None => pi_sdk::projects::Project::single_root(&entry.id, &entry.name, &entry.path),
            };
            worktrees.push(project);
        }
        store.import_missing(worktrees)?;
        Ok(self
            .project_store()
            .list()?
            .into_iter()
            .filter(|project| entries.contains_key(&project.id))
            .collect())
    }

    pub(crate) async fn project(&self, id: &str) -> Result<pi_sdk::projects::Project, String> {
        self.projects()
            .await?
            .into_iter()
            .find(|project| project.id == id)
            .ok_or_else(|| format!("unknown project: {id}"))
    }
}
