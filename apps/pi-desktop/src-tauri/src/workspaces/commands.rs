use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use super::files::{
    list_workspace_files_inner, read_workspace_file_inner, WorkspaceFileListing,
    WorkspaceFileResponse,
};
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
use crate::shared::worktree_groups::delivery::{DeliveryOverview, DeliveryPreview};
use crate::shared::worktree_groups::delivery_execution::{
    DeliveryAttempt, DeliveryExecutionRequest,
};
use crate::shared::worktree_groups::{ManagedWorktreeSummary, WorktreePlan, WorktreeRequest};
use crate::shared::{workspaces_core, worktree_groups};
use crate::state::AppState;
use crate::storage::write_workspaces;
use crate::types::{WorkspaceEntry, WorkspaceInfo, WorkspaceSettings, WorktreeSetupStatus};
use crate::utils::normalize_windows_namespace_path;

#[tauri::command]
pub(crate) async fn read_workspace_file(
    workspace_id: String,
    path: String,
    root_id: Option<String>,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<WorkspaceFileResponse, String> {
    let workspace = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        read_workspace_file_inner(&workspace, root_id.as_deref(), &path)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn list_workspaces(
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceInfo>, String> {
    state.list_workspaces().await
}

#[tauri::command]
pub(crate) async fn is_workspace_path_dir(path: String) -> Result<bool, String> {
    Ok(workspaces_core::is_workspace_path_dir_core(&path))
}

#[tauri::command]
pub(crate) async fn create_workspace_project(
    name: String,
    paths: Vec<String>,
    primary_path: String,
    state: State<'_, AppState>,
) -> Result<WorkspaceInfo, String> {
    state
        .create_workspace_project(name, paths, primary_path)
        .await
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
    let source = state.project(&parent_id).await?.resolve()?;
    let parent = state
        .workspaces
        .lock()
        .await
        .get(&parent_id)
        .cloned()
        .ok_or("parent workspace not found")?;
    require_legacy_worktree_source(&source, &parent)?;
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

fn require_legacy_worktree_source(
    source: &pi_core::WorkspaceSpec,
    parent: &WorkspaceEntry,
) -> Result<(), String> {
    const USE_PREVIEW: &str =
        "Use the worktree preview flow to select repositories and preserve project directories";
    if source.roots().len() != 1 || source.cwd() != source.primary_root().path {
        return Err(USE_PREVIEW.into());
    }
    let legacy_path =
        std::fs::canonicalize(&parent.path).map_err(|error| format!("{USE_PREVIEW}: {error}"))?;
    if source.cwd() != legacy_path {
        return Err(USE_PREVIEW.into());
    }
    let repository = git2::Repository::discover(resolve_git_root(parent)?)
        .map_err(|error| format!("{USE_PREVIEW}: {error}"))?;
    let workdir = repository
        .workdir()
        .ok_or_else(|| format!("{USE_PREVIEW}: bare repositories have no working directory"))?
        .canonicalize()
        .map_err(|error| format!("{USE_PREVIEW}: {error}"))?;
    if workdir != legacy_path {
        return Err(USE_PREVIEW.into());
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn prepare_worktree_plan(
    request: WorktreeRequest,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
    app: AppHandle,
) -> Result<WorktreePlan, String> {
    let source = pi
        .workspace_spec(&state, &request.parent_id, request.thread_id.as_deref())
        .await?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve app data dir: {error}"))?;
    worktree_groups::prepare(&state, &data_dir, request, source).await
}

#[tauri::command]
pub(crate) async fn create_worktree_plan(
    plan_id: String,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<WorkspaceInfo, String> {
    let (parent_id, thread_id) = worktree_groups::source_context(&state.storage_path, &plan_id)?;
    let source = pi
        .workspace_spec(&state, &parent_id, thread_id.as_deref())
        .await?;
    worktree_groups::create(&state, &plan_id, source).await
}

#[tauri::command]
pub(crate) async fn discard_worktree_plan(
    plan_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    worktree_groups::discard(&state.storage_path, &plan_id)
}

#[tauri::command]
pub(crate) async fn list_managed_worktrees(
    state: State<'_, AppState>,
) -> Result<Vec<ManagedWorktreeSummary>, String> {
    worktree_groups::list(&state.storage_path)
}

#[tauri::command]
pub(crate) async fn get_worktree_delivery(
    workspace_id: String,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<DeliveryOverview, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let storage_path = state.storage_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree_groups::delivery::overview(&storage_path, &workspace_id, &spec)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn preview_worktree_delivery(
    workspace_id: String,
    thread_id: Option<String>,
    checkout_key: String,
    target_branch: String,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<DeliveryPreview, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let storage_path = state.storage_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree_groups::delivery::preview(
            &storage_path,
            &workspace_id,
            &spec,
            &checkout_key,
            &target_branch,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn execute_worktree_delivery(
    workspace_id: String,
    thread_id: Option<String>,
    request: DeliveryExecutionRequest,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<DeliveryAttempt, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let storage_path = state.storage_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree_groups::delivery_execution::execute(&storage_path, &workspace_id, &spec, request)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn inspect_worktree_delivery_attempt(
    workspace_id: String,
    thread_id: Option<String>,
    attempt_id: String,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<DeliveryAttempt, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let storage_path = state.storage_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree_groups::delivery_execution::inspect(
            &storage_path,
            &workspace_id,
            &spec,
            &attempt_id,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn finish_worktree_delivery_attempt(
    workspace_id: String,
    thread_id: Option<String>,
    attempt_id: String,
    state: State<'_, AppState>,
    pi: State<'_, crate::pi_runtime::PiRuntimeState>,
) -> Result<DeliveryAttempt, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let storage_path = state.storage_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree_groups::delivery_execution::finish(
            &storage_path,
            &workspace_id,
            &spec,
            &attempt_id,
        )
    })
    .await
    .map_err(|error| error.to_string())?
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
    let _delivery_guard =
        worktree_groups::delivery_execution::lock_for_detach(&state.storage_path, &id)?;
    workspaces_core::remove_workspace_core(id.clone(), &state.workspaces, &state.storage_path)
        .await?;
    state.project_store().remove(&id)
}

#[tauri::command]
pub(crate) async fn remove_worktree(
    id: String,
    force: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if worktree_groups::has_record(&state.storage_path, &id)? {
        return worktree_groups::remove(&state, &id, force.unwrap_or(false)).await;
    }
    if has_managed_worktree_marker(&state, &id).await {
        return Err("Managed worktree ownership record is missing; cleanup is unavailable".into());
    }
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
    require_legacy_worktree(&state, &id).await?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Failed to resolve app data dir: {err}"))?;

    let project = state.project(&id).await?;
    let previous = state
        .workspaces
        .lock()
        .await
        .get(&id)
        .ok_or("workspace not found")?
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
    let project = project_after_worktree_rename(project, &previous, &result)?;
    state.project_store().upsert(project.clone())?;
    result.with_project(project)
}

fn project_after_worktree_rename(
    mut project: pi_sdk::projects::Project,
    previous: &WorkspaceEntry,
    renamed: &WorkspaceInfo,
) -> Result<pi_sdk::projects::Project, String> {
    let old_branch = &previous
        .worktree
        .as_ref()
        .ok_or("worktree metadata missing")?
        .branch;
    let new_branch = &renamed
        .worktree
        .as_ref()
        .ok_or("renamed worktree metadata missing")?
        .branch;
    for root in &mut project.roots {
        if root.path == std::path::Path::new(&previous.path) {
            root.path = PathBuf::from(&renamed.path);
        }
    }
    // A label edited through Project settings can be newer than the legacy UI entry.
    if project.name.trim() == old_branch {
        project.name = new_branch.clone();
    }
    Ok(project)
}

#[tauri::command]
pub(crate) async fn rename_worktree_upstream(
    id: String,
    old_branch: String,
    new_branch: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    require_legacy_worktree(&state, &id).await?;
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
    require_legacy_worktree(&state, &workspace_id).await?;
    workspaces_core::apply_worktree_changes_core(&state.workspaces, workspace_id).await
}

async fn has_managed_worktree_marker(state: &AppState, id: &str) -> bool {
    state
        .workspaces
        .lock()
        .await
        .get(id)
        .and_then(|entry| entry.worktree.as_ref())
        .is_some_and(|worktree| worktree.managed)
}

async fn require_legacy_worktree(state: &AppState, id: &str) -> Result<(), String> {
    if worktree_groups::has_record(&state.storage_path, id)?
        || has_managed_worktree_marker(state, id).await
    {
        return Err("This operation is unavailable for managed multi-repository worktrees".into());
    }
    Ok(())
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
) -> Result<WorkspaceFileListing, String> {
    let workspace = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    tauri::async_runtime::spawn_blocking(move || list_workspace_files_inner(workspace))
        .await
        .map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_worktree_creation_requires_one_root_at_the_selected_checkout_base() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let repo_path = base.join("repo");
        git2::Repository::init(&repo_path).unwrap();
        std::fs::create_dir(repo_path.join("src")).unwrap();
        let mut parent = WorkspaceEntry {
            id: "parent".into(),
            name: "Parent".into(),
            path: repo_path.to_string_lossy().into_owned(),
            kind: crate::types::WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: Default::default(),
        };
        let source = pi_core::WorkspaceSpec::from_cwd(&repo_path);
        assert!(require_legacy_worktree_source(&source, &parent).is_ok());

        let mut multiple_roots = source.roots().to_vec();
        multiple_roots.push(pi_core::WorkspaceRoot::external(
            "other",
            "Other",
            base.join("other"),
        ));
        let multiple = pi_core::WorkspaceSpec::new(
            multiple_roots,
            source.primary_root_id().clone(),
            &repo_path,
        )
        .unwrap();
        let execution_below_root = pi_core::WorkspaceSpec::new(
            source.roots().to_vec(),
            source.primary_root_id().clone(),
            repo_path.join("src"),
        )
        .unwrap();
        let changed_root = pi_core::WorkspaceSpec::from_cwd(base.join("changed-project"));
        for source in [&multiple, &execution_below_root, &changed_root] {
            assert!(require_legacy_worktree_source(source, &parent)
                .unwrap_err()
                .contains("preview"));
        }

        parent.path = repo_path.join("src").to_string_lossy().into_owned();
        let subdirectory = pi_core::WorkspaceSpec::from_cwd(&parent.path);
        assert!(require_legacy_worktree_source(&subdirectory, &parent)
            .unwrap_err()
            .contains("preview"));

        parent.path = repo_path.to_string_lossy().into_owned();
        git2::Repository::init(repo_path.join("nested")).unwrap();
        parent.settings.git_root = Some("nested".into());
        assert!(require_legacy_worktree_source(&source, &parent)
            .unwrap_err()
            .contains("preview"));
    }

    #[test]
    fn legacy_worktree_rename_uses_current_project_label() {
        let directory = tempfile::tempdir().unwrap();
        let old_path = directory.path().join("old");
        let new_path = directory.path().join("new");
        let previous = WorkspaceEntry {
            id: "child".into(),
            name: "Stale legacy label".into(),
            path: old_path.to_string_lossy().into_owned(),
            kind: crate::types::WorkspaceKind::Worktree,
            parent_id: Some("parent".into()),
            worktree: Some(crate::types::WorktreeInfo {
                branch: "feature/old".into(),
                managed: false,
            }),
            settings: Default::default(),
        };
        let renamed = WorkspaceInfo {
            project: None,
            id: previous.id.clone(),
            name: previous.name.clone(),
            path: new_path.to_string_lossy().into_owned(),
            kind: previous.kind.clone(),
            parent_id: previous.parent_id.clone(),
            worktree: Some(crate::types::WorktreeInfo {
                branch: "feature/new".into(),
                managed: false,
            }),
            settings: previous.settings.clone(),
        };
        for (name, expected) in [
            ("Edited project label", "Edited project label"),
            ("feature/old", "feature/new"),
        ] {
            let mut project = pi_sdk::projects::Project::single_root(&previous.id, name, &old_path);
            let supplemental = pi_core::WorkspaceRoot::external(
                "supplemental",
                "Supplemental",
                directory.path().join("supplemental"),
            );
            project.roots.push(supplemental.clone());
            let original_workspace = project.spec().unwrap();
            let project = project_after_worktree_rename(project, &previous, &renamed).unwrap();
            assert_eq!(project.name, expected);
            assert_eq!(project.roots[0].path, new_path);
            assert_eq!(project.roots[1], supplemental);
            assert_eq!(project.primary_root, *original_workspace.primary_root_id());
            assert_eq!(original_workspace.cwd(), old_path);
            assert_eq!(
                renamed.clone().with_project(project).unwrap().name,
                expected
            );
        }
    }

    #[tokio::test]
    async fn managed_marker_blocks_legacy_operations_without_an_ownership_record() {
        let directory = tempfile::tempdir().unwrap();
        let entry = WorkspaceEntry {
            id: "managed".into(),
            name: "Managed".into(),
            path: directory.path().to_string_lossy().into_owned(),
            kind: crate::types::WorkspaceKind::Worktree,
            parent_id: Some("parent".into()),
            worktree: Some(crate::types::WorktreeInfo {
                branch: "feature/group".into(),
                managed: true,
            }),
            settings: Default::default(),
        };
        let state = AppState {
            workspaces: tokio::sync::Mutex::new(std::collections::HashMap::from([(
                entry.id.clone(),
                entry,
            )])),
            terminal_sessions: Default::default(),
            storage_path: directory.path().join("workspaces.json"),
            settings_path: directory.path().join("settings.json"),
            app_settings: Default::default(),
            dictation: tokio::sync::Mutex::new(crate::dictation::DictationState::default()),
        };
        assert!(!worktree_groups::has_record(&state.storage_path, "managed").unwrap());
        assert!(require_legacy_worktree(&state, "managed").await.is_err());
        state
            .workspaces
            .lock()
            .await
            .get_mut("managed")
            .unwrap()
            .worktree
            .as_mut()
            .unwrap()
            .managed = false;
        assert!(require_legacy_worktree(&state, "managed").await.is_ok());
    }
}
