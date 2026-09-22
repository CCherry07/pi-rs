use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::dictation::DictationState;
use crate::storage::{read_settings, read_workspaces, write_workspaces};
use crate::types::{AppSettings, WorkspaceEntry, WorkspaceInfo};

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
    pub(crate) async fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>, String> {
        let projects = self.projects().await?;
        let workspaces = self.workspaces.lock().await;
        workspaces
            .values()
            .cloned()
            .map(|entry| {
                let project = projects
                    .iter()
                    .find(|project| project.id == entry.id)
                    .ok_or_else(|| format!("unknown project: {}", entry.id))?;
                WorkspaceInfo {
                    project: None,
                    id: entry.id,
                    name: entry.name,
                    path: entry.path,
                    kind: entry.kind,
                    parent_id: entry.parent_id,
                    worktree: entry.worktree,
                    settings: entry.settings,
                }
                .with_project(project.clone())
            })
            .collect()
    }

    pub(crate) async fn create_workspace_project(
        &self,
        name: String,
        paths: Vec<String>,
        primary_path: String,
    ) -> Result<WorkspaceInfo, String> {
        let project = local_project(name, paths, primary_path)?;
        let entry = WorkspaceEntry {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project.spec()?.cwd().to_string_lossy().into_owned(),
            kind: crate::types::WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: Default::default(),
        };
        let info = WorkspaceInfo {
            project: Some(project.clone()),
            id: entry.id.clone(),
            name: entry.name.clone(),
            path: entry.path.clone(),
            kind: entry.kind.clone(),
            parent_id: None,
            worktree: None,
            settings: entry.settings.clone(),
        };

        let mut workspaces = self.workspaces.lock().await;
        let mut entries = workspaces.values().cloned().collect::<Vec<_>>();
        entries.push(entry.clone());
        let store = self.project_store();
        // Persist the complete definition before its UI association can be discovered.
        // Publishing memory last also keeps failed writes out of the current sidebar.
        store.upsert(project)?;
        if let Err(error) = write_workspaces(&self.storage_path, &entries) {
            return match store.remove(&entry.id) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "Could not save workspace: {error}. Could not remove unpublished project {}: {rollback_error}",
                    entry.id
                )),
            };
        }
        workspaces.insert(entry.id.clone(), entry);
        Ok(info)
    }

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
        let existing_projects = store
            .list()?
            .into_iter()
            .map(|project| (project.id.clone(), project))
            .collect::<HashMap<_, _>>();
        let mut worktrees = Vec::new();
        for entry in entries.values().filter(|entry| entry.kind.is_worktree()) {
            if existing_projects.contains_key(&entry.id) {
                continue;
            }
            if entry
                .worktree
                .as_ref()
                .is_some_and(|worktree| worktree.managed)
            {
                return Err(format!(
                    "Managed worktree project {} is missing; restore its saved Project definition before opening it",
                    entry.id
                ));
            }
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

fn local_project(
    name: String,
    paths: Vec<String>,
    primary_path: String,
) -> Result<pi_sdk::projects::Project, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Workspace name must not be empty.".into());
    }
    if paths.is_empty() {
        return Err("Select at least one workspace directory.".into());
    }
    let canonical_directory = |path: &str| -> Result<PathBuf, String> {
        if path.trim().is_empty() {
            return Err("Workspace directory must not be empty.".into());
        }
        let canonical = std::fs::canonicalize(path).map_err(|error| {
            format!("Workspace path must be an accessible folder ({path}): {error}")
        })?;
        if !canonical.is_dir() {
            return Err(format!("Workspace path must be a folder: {path}"));
        }
        Ok(PathBuf::from(
            crate::utils::normalize_windows_namespace_path(&canonical.to_string_lossy()),
        ))
    };
    let primary_path = canonical_directory(&primary_path)?;
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for path in paths {
        let path = canonical_directory(&path)?;
        if !seen.insert(path.clone()) {
            continue;
        }
        let root_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Folder")
            .to_string();
        roots.push(pi_core::WorkspaceRoot::external(
            uuid::Uuid::new_v4().to_string(),
            root_name,
            path,
        ));
    }
    let primary_root = roots
        .iter()
        .find(|root| root.path == primary_path)
        .ok_or("Primary directory must be one of the workspace directories.")?
        .id
        .clone();
    let project = pi_sdk::projects::Project {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        roots,
        primary_root,
        execution_dir: None,
    };
    project.validate()?;
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WorkspaceKind, WorktreeInfo};
    use pi_core::{WorkspaceRoot, WorkspaceRootId, WorkspaceSpec};
    use pi_sdk::projects::Project;

    fn state(directory: &std::path::Path, entries: Vec<WorkspaceEntry>) -> AppState {
        AppState {
            workspaces: Mutex::new(
                entries
                    .into_iter()
                    .map(|entry| (entry.id.clone(), entry))
                    .collect(),
            ),
            terminal_sessions: Default::default(),
            storage_path: directory.join("workspaces.json"),
            settings_path: directory.join("settings.json"),
            app_settings: Default::default(),
            dictation: Mutex::new(DictationState::default()),
        }
    }

    fn child_entry(path: &std::path::Path) -> WorkspaceEntry {
        WorkspaceEntry {
            id: "child".into(),
            name: "Child".into(),
            path: path.to_string_lossy().into_owned(),
            kind: WorkspaceKind::Worktree,
            parent_id: Some("parent".into()),
            worktree: Some(WorktreeInfo {
                branch: "feature/group".into(),
                managed: true,
            }),
            settings: Default::default(),
        }
    }

    fn normalized_directory(path: &std::path::Path) -> PathBuf {
        PathBuf::from(crate::utils::normalize_windows_namespace_path(
            &path.canonicalize().unwrap().to_string_lossy(),
        ))
    }

    #[tokio::test]
    async fn create_workspace_project_preserves_multiple_roots_and_primary_after_reload() {
        let directory = tempfile::tempdir().unwrap();
        let frontend = directory.path().join("frontend");
        let backend = directory.path().join("backend");
        std::fs::create_dir(&frontend).unwrap();
        std::fs::create_dir(&backend).unwrap();
        let previous = WorkspaceEntry {
            id: "previous".into(),
            name: "Previous".into(),
            path: directory.path().to_string_lossy().into_owned(),
            kind: WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: crate::types::WorkspaceSettings {
                group_id: Some("existing-group".into()),
                sort_order: Some(3),
                ..Default::default()
            },
        };
        let previous_project = Project::single_root(&previous.id, &previous.name, &previous.path);
        let app = state(directory.path(), vec![previous.clone()]);
        app.project_store()
            .upsert(previous_project.clone())
            .unwrap();
        write_workspaces(&app.storage_path, std::slice::from_ref(&previous)).unwrap();

        let info = app
            .create_workspace_project(
                "  Product  ".into(),
                vec![
                    frontend.to_string_lossy().into_owned(),
                    backend.to_string_lossy().into_owned(),
                ],
                backend.to_string_lossy().into_owned(),
            )
            .await
            .unwrap();
        let project = info.project.unwrap();
        assert_eq!(info.name, "Product");
        assert!(!info.kind.is_worktree());
        assert!(info.worktree.is_none());
        assert!(info.parent_id.is_none());
        assert_eq!(info.id, project.id);
        assert_eq!(project.name, "Product");
        assert_eq!(project.roots.len(), 2);
        assert_eq!(project.roots[0].path, normalized_directory(&frontend));
        assert_eq!(project.roots[1].path, normalized_directory(&backend));
        assert_eq!(project.roots[0].name, "frontend");
        assert_eq!(project.roots[1].name, "backend");
        assert_ne!(project.roots[0].id, project.roots[1].id);
        assert_eq!(project.primary_root, project.roots[1].id);
        assert!(project.execution_dir.is_none());
        assert_eq!(
            project.spec().unwrap().cwd(),
            normalized_directory(&backend)
        );
        assert_eq!(PathBuf::from(&info.path), project.roots[1].path);
        assert!(project
            .roots
            .iter()
            .all(|root| matches!(root.ownership, pi_core::WorkspaceRootOwnership::External)));

        let saved = read_workspaces(&app.storage_path).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(
            serde_json::to_value(&saved[&previous.id]).unwrap(),
            serde_json::to_value(&previous).unwrap()
        );
        assert_eq!(saved[&info.id].name, "Product");
        assert_eq!(saved[&info.id].path, info.path);
        assert_eq!(app.workspaces.lock().await.len(), 2);
        assert_eq!(app.project_store().list().unwrap().len(), 2);
        let reloaded = state(directory.path(), saved.into_values().collect());
        assert_eq!(reloaded.project(&info.id).await.unwrap(), project);
        assert_eq!(
            reloaded.project(&previous.id).await.unwrap(),
            previous_project
        );
    }

    #[tokio::test]
    async fn edited_project_name_is_listed_and_survives_reload() {
        let directory = tempfile::tempdir().unwrap();
        let app = state(directory.path(), vec![]);
        let path = directory.path().to_string_lossy().into_owned();
        let info = app
            .create_workspace_project("Original label".into(), vec![path.clone()], path)
            .await
            .unwrap();
        let mut project = info.project.unwrap();
        let original_workspace = project.spec().unwrap();
        project.name = "Edited label".into();
        app.project_store().upsert(project.clone()).unwrap();

        let listed = app.list_workspaces().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Edited label");
        assert_eq!(listed[0].project.as_ref(), Some(&project));
        assert_eq!(project.spec().unwrap(), original_workspace);

        let saved = read_workspaces(&app.storage_path).unwrap();
        assert_eq!(saved[&info.id].name, "Original label");
        let reloaded = state(directory.path(), saved.into_values().collect());
        let listed = reloaded.list_workspaces().await.unwrap();
        assert_eq!(listed[0].name, "Edited label");
        assert_eq!(listed[0].project.as_ref(), Some(&project));
    }

    #[tokio::test]
    async fn create_workspace_project_rejects_invalid_input_without_publishing() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let other = directory.path().join("other");
        let file = directory.path().join("file");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(&file, "not a directory").unwrap();
        let path = root.to_string_lossy().into_owned();
        let missing = directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned();
        let app = state(directory.path(), vec![]);
        for (name, paths, primary) in [
            (" ", vec![path.clone()], path.clone()),
            ("Product", vec![], path.clone()),
            ("Product", vec![path.clone(), missing], path.clone()),
            (
                "Product",
                vec![path.clone(), file.to_string_lossy().into_owned()],
                path.clone(),
            ),
            (
                "Product",
                vec![path.clone()],
                other.to_string_lossy().into_owned(),
            ),
            ("Product", vec![path], String::new()),
        ] {
            assert!(app
                .create_workspace_project(name.into(), paths, primary)
                .await
                .is_err());
            assert!(app.workspaces.lock().await.is_empty());
            assert!(!app.storage_path.exists());
            assert!(!directory.path().join("projects.json").exists());
        }
        assert!(root.is_dir());
        assert!(other.is_dir());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "not a directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_workspace_project_deduplicates_aliases_and_resolves_primary_alias() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let alias = directory.path().join("alias");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let app = state(directory.path(), vec![]);
        let info = app
            .create_workspace_project(
                "Product".into(),
                vec![
                    root.to_string_lossy().into_owned(),
                    alias.to_string_lossy().into_owned(),
                    root.join(".").to_string_lossy().into_owned(),
                ],
                alias.to_string_lossy().into_owned(),
            )
            .await
            .unwrap();
        let project = info.project.unwrap();
        assert_eq!(project.roots.len(), 1);
        assert_eq!(project.roots[0].path, normalized_directory(&root));
        assert_eq!(project.primary_root, project.roots[0].id);
        assert_eq!(app.workspaces.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn create_workspace_project_rolls_back_when_ui_storage_cannot_be_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let app = state(directory.path(), vec![]);
        let previous = Project::single_root("previous", "Previous", directory.path());
        app.project_store().upsert(previous.clone()).unwrap();
        std::fs::create_dir(&app.storage_path).unwrap();
        let sentinel = app.storage_path.join("keep");
        std::fs::write(&sentinel, "unchanged").unwrap();
        let path = directory.path().to_string_lossy().into_owned();

        assert!(app
            .create_workspace_project("Product".into(), vec![path.clone()], path)
            .await
            .is_err());
        assert!(app.workspaces.lock().await.is_empty());
        assert_eq!(app.project_store().list().unwrap(), vec![previous]);
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "unchanged");
    }

    #[tokio::test]
    async fn create_workspace_project_keeps_ui_storage_unchanged_when_project_write_fails() {
        let directory = tempfile::tempdir().unwrap();
        let app = state(directory.path(), vec![]);
        write_workspaces(&app.storage_path, &[]).unwrap();
        let previous = std::fs::read(&app.storage_path).unwrap();
        std::fs::create_dir(directory.path().join("projects.json")).unwrap();
        let path = directory.path().to_string_lossy().into_owned();

        assert!(app
            .create_workspace_project("Product".into(), vec![path.clone()], path)
            .await
            .is_err());
        assert!(app.workspaces.lock().await.is_empty());
        assert_eq!(std::fs::read(&app.storage_path).unwrap(), previous);
    }

    #[tokio::test]
    async fn saved_managed_project_survives_parent_edits_and_removal() {
        let directory = tempfile::tempdir().unwrap();
        let frontend = directory.path().join("worktrees/frontend");
        let backend = directory.path().join("worktrees/backend");
        let child_workspace = WorkspaceSpec::new(
            vec![
                WorkspaceRoot::external("frontend", "Frontend", &frontend),
                WorkspaceRoot::external("backend", "Backend", backend),
            ],
            WorkspaceRootId::new("frontend"),
            frontend.join("src"),
        )
        .unwrap();
        let parent = WorkspaceEntry {
            id: "parent".into(),
            name: "Parent".into(),
            path: directory
                .path()
                .join("original-parent")
                .to_string_lossy()
                .into_owned(),
            kind: WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: Default::default(),
        };
        let state = state(
            directory.path(),
            vec![parent, child_entry(child_workspace.cwd())],
        );
        let child = Project::from_workspace("child", "Child", &child_workspace).unwrap();
        state.project_store().upsert(child.clone()).unwrap();
        let edited_parent = Project::single_root(
            "parent",
            "Edited parent",
            directory.path().join("edited-parent"),
        );
        state.project_store().upsert(edited_parent.clone()).unwrap();

        assert_eq!(state.project("child").await.unwrap(), child);
        assert_eq!(state.project("parent").await.unwrap(), edited_parent);
        state.workspaces.lock().await.remove("parent");
        state.project_store().remove("parent").unwrap();
        assert_eq!(
            state.project("child").await.unwrap().spec().unwrap(),
            child_workspace
        );
    }

    #[tokio::test]
    async fn missing_managed_project_is_not_reconstructed_as_a_legacy_worktree() {
        let directory = tempfile::tempdir().unwrap();
        let state = state(directory.path(), vec![child_entry(directory.path())]);
        let error = state.projects().await.unwrap_err();
        assert!(error.contains("restore its saved Project definition"));
        assert!(state.project_store().list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn legacy_worktree_without_saved_project_still_migrates() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = child_entry(directory.path());
        child.worktree.as_mut().unwrap().managed = false;
        let state = state(directory.path(), vec![child]);
        assert_eq!(
            state.project("child").await.unwrap().spec().unwrap().cwd(),
            directory.path()
        );
    }
}
