use serde_json::Value;
use tauri::State;

use crate::pi_runtime::PiRuntimeState;
use crate::shared::{
    git_targets::{self, GitInventory, GitTarget},
    git_ui_core,
};
use crate::state::AppState;
use crate::types::{
    GitCommitDiff, GitFileDiff, GitHubIssuesResponse, GitHubPullRequestComment,
    GitHubPullRequestDiff, GitHubPullRequestsResponse, GitLogResponse,
};

#[tauri::command]
pub(crate) async fn get_git_status(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_git_status_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn init_git_repo(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    branch: String,
    force: Option<bool>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    let mut response =
        git_ui_core::init_git_repo_core(repo_root.clone(), branch, force.unwrap_or(false)).await?;
    if matches!(
        response["status"].as_str(),
        Some("initialized" | "already_initialized")
    ) {
        response["target"] = serde_json::to_value(git_targets::target_at(&repo_root)?)
            .map_err(|error| error.to_string())?;
    }
    Ok(response)
}

// Tauri injects both states; the remaining parameters preserve the existing IPC fields.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn create_github_repo(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    repo: String,
    visibility: String,
    branch: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::create_github_repo_core(repo_root, repo, visibility, branch).await
}

#[tauri::command]
pub(crate) async fn stage_git_file(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    path: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::stage_git_file_core(repo_root, path).await
}

#[tauri::command]
pub(crate) async fn stage_git_all(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::stage_git_all_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn unstage_git_file(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    path: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::unstage_git_file_core(repo_root, path).await
}

#[tauri::command]
pub(crate) async fn revert_git_file(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    path: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::revert_git_file_core(repo_root, path).await
}

#[tauri::command]
pub(crate) async fn revert_git_all(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::revert_git_all_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn commit_git(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    message: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::commit_git_core(repo_root, message).await
}

#[tauri::command]
pub(crate) async fn generate_commit_message(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    commit_message_model_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<String, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    let diff = git_ui_core::collect_workspace_diff_core(&repo_root)?;
    let template = state
        .app_settings
        .lock()
        .await
        .commit_message_prompt
        .clone();
    let prompt = crate::shared::ai_tasks_core::build_commit_message_prompt(&diff, &template)?;
    crate::pi_runtime::run_background_prompt_in_cwd(
        &pi,
        &repo_root,
        prompt,
        commit_message_model_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn push_git(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::push_git_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn pull_git(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::pull_git_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn fetch_git(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::fetch_git_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn sync_git(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::sync_git_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn get_git_diffs(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Vec<GitFileDiff>, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_git_diffs_core(repo_root, &state.app_settings).await
}

#[tauri::command]
pub(crate) async fn get_git_log(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    limit: Option<usize>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<GitLogResponse, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_git_log_core(repo_root, limit).await
}

#[tauri::command]
pub(crate) async fn get_git_commit_diff(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    sha: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Vec<GitCommitDiff>, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_git_commit_diff_core(repo_root, &state.app_settings, sha).await
}

#[tauri::command]
pub(crate) async fn get_git_remote(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Option<String>, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_git_remote_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn get_github_issues(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<GitHubIssuesResponse, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_github_issues_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn get_github_pull_requests(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<GitHubPullRequestsResponse, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_github_pull_requests_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn get_github_pull_request_diff(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    pr_number: u64,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Vec<GitHubPullRequestDiff>, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_github_pull_request_diff_core(repo_root, pr_number).await
}

#[tauri::command]
pub(crate) async fn get_github_pull_request_comments(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    pr_number: u64,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Vec<GitHubPullRequestComment>, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::get_github_pull_request_comments_core(repo_root, pr_number).await
}

#[tauri::command]
pub(crate) async fn checkout_github_pull_request(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    pr_number: u64,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::checkout_github_pull_request_core(repo_root, pr_number).await
}

#[tauri::command]
pub(crate) async fn list_git_branches(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::list_git_branches_core(repo_root).await
}

#[tauri::command]
pub(crate) async fn checkout_git_branch(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    name: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::checkout_git_branch_core(repo_root, name).await
}

#[tauri::command]
pub(crate) async fn create_git_branch(
    workspace_id: String,
    thread_id: Option<String>,
    target: Option<GitTarget>,
    name: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<(), String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let repo_root = git_targets::resolve(&spec, target.as_ref())?;
    git_ui_core::create_git_branch_core(repo_root, name).await
}

#[tauri::command]
pub(crate) async fn list_git_checkouts(
    workspace_id: String,
    thread_id: Option<String>,
    depth: Option<usize>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<GitInventory, String> {
    let spec = pi
        .workspace_spec(&state, &workspace_id, thread_id.as_deref())
        .await?;
    let preferred = state
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .filter(|entry| entry.settings.git_root.is_some())
        .and_then(|entry| crate::git_utils::resolve_git_root(entry).ok());
    tokio::task::spawn_blocking(move || {
        let mut inventory = git_targets::discover(spec, depth);
        if let Some(path) = preferred {
            git_targets::prefer_path(&mut inventory, &path);
        }
        inventory
    })
    .await
    .map_err(|error| error.to_string())
}
