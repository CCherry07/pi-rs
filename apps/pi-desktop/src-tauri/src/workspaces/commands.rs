use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use super::files::{list_workspace_files_inner, read_workspace_file_inner, WorkspaceFileResponse};
use super::git::{
    git_branch_exists, git_find_remote_for_branch, git_remote_branch_exists, git_remote_exists,
    is_missing_worktree_error, run_git_command_owned, unique_branch_name,
};
#[cfg(target_os = "macos")]
use super::macos::get_open_app_icon_inner;
use super::settings::apply_workspace_settings_update;
use super::worktree::{
    sanitize_worktree_name, unique_worktree_path, unique_worktree_path_for_rename,
};

use crate::git_utils::resolve_git_root;
use crate::shared::workspaces_core;
use crate::state::AppState;
use crate::storage::write_workspaces;
use crate::types::{WorkspaceEntry, WorkspaceInfo, WorkspaceSettings, WorktreeSetupStatus};
use crate::utils::normalize_windows_namespace_path;

#[tauri::command]
pub(crate) async fn read_workspace_file(
    workspace_id: String,
    path: String,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<WorkspaceFileResponse, String> {
    let root = pi
        .execution_directory(&state, &workspace_id, thread_id.as_deref())
        .await?;
    read_workspace_file_inner(&root, &path)
}

#[tauri::command]
pub(crate) async fn list_workspaces(
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceInfo>, String> {
    let projects = state.projects().await?;
    let workspaces = state.workspaces.lock().await;
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

#[tauri::command]
pub(crate) async fn is_workspace_path_dir(path: String) -> Result<bool, String> {
    Ok(workspaces_core::is_workspace_path_dir_core(&path))
}

#[tauri::command]
pub(crate) async fn add_workspace(
    path: String,
    state: State<'_, AppState>,
) -> Result<WorkspaceInfo, String> {
    let normalized = std::fs::canonicalize(path.trim())
        .map_err(|error| format!("Workspace path must be an accessible folder: {error}"))?;
    if !normalized.is_dir() {
        return Err("Workspace path must be a folder.".to_string());
    }
    let path = normalize_windows_namespace_path(&normalized.to_string_lossy());
    let name = normalized
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Workspace")
        .to_string();
    let entry = WorkspaceEntry {
        id: Uuid::new_v4().to_string(),
        name,
        path,
        kind: crate::types::WorkspaceKind::Main,
        parent_id: None,
        worktree: None,
        settings: WorkspaceSettings::default(),
    };
    {
        let mut workspaces = state.workspaces.lock().await;
        workspaces.insert(entry.id.clone(), entry.clone());
        write_workspaces(
            &state.storage_path,
            &workspaces.values().cloned().collect::<Vec<_>>(),
        )?;
    }
    state
        .project_info(WorkspaceInfo {
            project: None,
            id: entry.id,
            name: entry.name,
            path: entry.path,
            kind: entry.kind,
            parent_id: entry.parent_id,
            worktree: entry.worktree,
            settings: entry.settings,
        })
        .await
}

#[tauri::command]
pub(crate) async fn add_workspace_from_git_url(
    url: String,
    destination_path: String,
    target_folder_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<WorkspaceInfo, String> {
    let info = workspaces_core::add_workspace_from_git_url_core(
        url,
        destination_path,
        target_folder_name,
        &state.workspaces,
        &state.storage_path,
    )
    .await?;
    state.project_info(info).await
}

#[tauri::command]
pub(crate) async fn add_clone(
    source_workspace_id: String,
    copy_name: String,
    copies_folder: String,
    state: State<'_, AppState>,
) -> Result<WorkspaceInfo, String> {
    let info = workspaces_core::add_clone_core(
        source_workspace_id,
        copy_name,
        copies_folder,
        &state.workspaces,
        &state.storage_path,
    )
    .await?;
    state.project_info(info).await
}

#[tauri::command]
pub(crate) async fn add_worktree(
    parent_id: String,
    branch: String,
    name: Option<String>,
    copy_agents_md: Option<bool>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<WorkspaceInfo, String> {
    let copy_agents_md = copy_agents_md.unwrap_or(true);
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Failed to resolve app data dir: {err}"))?;

    let info = workspaces_core::add_worktree_core(
        parent_id,
        branch,
        name,
        copy_agents_md,
        &data_dir,
        &state.workspaces,
        &state.app_settings,
        &state.storage_path,
        sanitize_worktree_name,
        |root, name| Ok(unique_worktree_path(root, name)),
        |root, branch| {
            let root = root.clone();
            let branch = branch.to_string();
            async move { git_branch_exists(&root, &branch).await }
        },
        None::<fn(&PathBuf, &str) -> std::future::Ready<Result<Option<String>, String>>>,
        |root, args| {
            workspaces_core::run_git_command_unit(root, args, |repo, args_owned| {
                run_git_command_owned(repo, args_owned)
            })
        },
    )
    .await?;
    state.project_info(info).await
}

#[tauri::command]
pub(crate) async fn worktree_setup_status(
    workspace_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<WorktreeSetupStatus, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Failed to resolve app data dir: {err}"))?;
    workspaces_core::worktree_setup_status_core(&state.workspaces, &workspace_id, &data_dir).await
}

#[tauri::command]
pub(crate) async fn worktree_setup_mark_ran(
    workspace_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Failed to resolve app data dir: {err}"))?;
    workspaces_core::worktree_setup_mark_ran_core(&state.workspaces, &workspace_id, &data_dir).await
}

#[tauri::command]
pub(crate) async fn remove_workspace(id: String, state: State<'_, AppState>) -> Result<(), String> {
    workspaces_core::remove_workspace_core(id.clone(), &state.workspaces, &state.storage_path)
        .await?;
    state.project_store().remove(&id)
}

#[tauri::command]
pub(crate) async fn remove_worktree(id: String, state: State<'_, AppState>) -> Result<(), String> {
    workspaces_core::remove_worktree_core(
        id,
        &state.workspaces,
        &state.storage_path,
        |root, args| {
            workspaces_core::run_git_command_unit(root, args, |repo, args_owned| {
                run_git_command_owned(repo, args_owned)
            })
        },
        is_missing_worktree_error,
        |path| {
            std::fs::remove_dir_all(path)
                .map_err(|err| format!("Failed to remove worktree folder: {err}"))
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn rename_worktree(
    id: String,
    branch: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<WorkspaceInfo, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Failed to resolve app data dir: {err}"))?;

    let mut project = state.project(&id).await?;
    let old_path = state
        .workspaces
        .lock()
        .await
        .get(&id)
        .ok_or("workspace not found")?
        .path
        .clone();
    let result = workspaces_core::rename_worktree_core(
        id,
        branch,
        &data_dir,
        &state.workspaces,
        &state.app_settings,
        &state.storage_path,
        resolve_git_root,
        |root, name| {
            let root = root.clone();
            let name = name.to_string();
            async move {
                unique_branch_name(&root, &name, None)
                    .await
                    .map(|(branch, _was_suffixed)| branch)
            }
        },
        sanitize_worktree_name,
        unique_worktree_path_for_rename,
        |root, args| {
            workspaces_core::run_git_command_unit(root, args, |repo, args_owned| {
                run_git_command_owned(repo, args_owned)
            })
        },
    )
    .await?;
    for root in &mut project.roots {
        if root.path == PathBuf::from(&old_path) {
            root.path = PathBuf::from(&result.path);
        }
    }
    project.name = result.name.clone();
    state.project_store().upsert(project.clone())?;
    result.with_project(project)
}

#[tauri::command]
pub(crate) async fn rename_worktree_upstream(
    id: String,
    old_branch: String,
    new_branch: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    workspaces_core::rename_worktree_upstream_core(
        id,
        old_branch,
        new_branch,
        &state.workspaces,
        resolve_git_root,
        |root, branch| {
            let root = root.clone();
            let branch = branch.to_string();
            async move { git_branch_exists(&root, &branch).await }
        },
        |root, branch| {
            let root = root.clone();
            let branch = branch.to_string();
            async move { git_find_remote_for_branch(&root, &branch).await }
        },
        |root, remote| {
            let root = root.clone();
            let remote = remote.to_string();
            async move { git_remote_exists(&root, &remote).await }
        },
        |root, remote, branch| {
            let root = root.clone();
            let remote = remote.to_string();
            let branch = branch.to_string();
            async move { git_remote_branch_exists(&root, &remote, &branch).await }
        },
        |root, args| {
            workspaces_core::run_git_command_unit(root, args, |repo, args_owned| {
                run_git_command_owned(repo, args_owned)
            })
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn apply_worktree_changes(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    workspaces_core::apply_worktree_changes_core(&state.workspaces, workspace_id).await
}

#[tauri::command]
pub(crate) async fn update_workspace_settings(
    id: String,
    settings: WorkspaceSettings,
    state: State<'_, AppState>,
) -> Result<WorkspaceInfo, String> {
    let info = workspaces_core::update_workspace_settings_core(
        id,
        settings,
        &state.workspaces,
        &state.storage_path,
        |workspaces, workspace_id, next_settings| {
            apply_workspace_settings_update(workspaces, workspace_id, next_settings)
        },
    )
    .await?;
    state.project_info(info).await
}

#[tauri::command]
pub(crate) async fn list_workspace_files(
    workspace_id: String,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<Vec<String>, String> {
    let root = pi
        .execution_directory(&state, &workspace_id, thread_id.as_deref())
        .await?;
    Ok(list_workspace_files_inner(&root, usize::MAX))
}

#[tauri::command]
pub(crate) async fn open_workspace_in(
    path: String,
    app: Option<String>,
    args: Vec<String>,
    command: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<(), String> {
    workspaces_core::open_workspace_in_core(path, app, args, command, line, column).await
}

#[tauri::command]
pub(crate) async fn get_open_app_icon(app_name: String) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        return workspaces_core::get_open_app_icon_core(app_name, |name| {
            get_open_app_icon_inner(name)
        })
        .await;
    }

    #[cfg(not(target_os = "macos"))]
    {
        workspaces_core::get_open_app_icon_core(app_name, |_name| None).await
    }
}

#[tauri::command]
pub(crate) async fn get_workspace_project(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<pi_sdk::projects::Project, String> {
    state.project(&workspace_id).await
}

#[tauri::command]
pub(crate) async fn update_workspace_project(
    project: pi_sdk::projects::Project,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<pi_sdk::projects::Project, String> {
    let previous = state.project(&project.id).await?;
    project.resolve()?;
    // Refresh only an unclaimed draft; settled sessions keep their saved scope.
    pi.refresh_project_draft(&project).await?;
    if let Err(error) = state.project_store().upsert(project.clone()) {
        let _ = pi.refresh_project_draft(&previous).await;
        return Err(error);
    }
    Ok(project)
}
