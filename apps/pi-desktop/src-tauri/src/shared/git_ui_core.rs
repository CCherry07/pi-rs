//! Git mechanics operate on an already resolved checkout, never mutable Project settings.
#[path = "git_ui_core/commands.rs"]
mod commands;
#[path = "git_ui_core/diff.rs"]
mod diff;
#[path = "git_ui_core/github.rs"]
mod github;
#[path = "git_ui_core/log.rs"]
mod log;
#[cfg(test)]
#[path = "git_ui_core/tests.rs"]
mod tests;
pub(crate) use commands::checkout_git_branch_inner as checkout_git_branch_core;
pub(crate) use commands::commit_git_inner as commit_git_core;
pub(crate) use commands::create_git_branch_inner as create_git_branch_core;
pub(crate) use commands::create_github_repo_inner as create_github_repo_core;
pub(crate) use commands::fetch_git_inner as fetch_git_core;
pub(crate) use commands::init_git_repo_inner as init_git_repo_core;
pub(crate) use commands::list_git_branches_inner as list_git_branches_core;
pub(crate) use commands::pull_git_inner as pull_git_core;
pub(crate) use commands::push_git_inner as push_git_core;
pub(crate) use commands::revert_git_all_inner as revert_git_all_core;
pub(crate) use commands::revert_git_file_inner as revert_git_file_core;
pub(crate) use commands::stage_git_all_inner as stage_git_all_core;
pub(crate) use commands::stage_git_file_inner as stage_git_file_core;
pub(crate) use commands::sync_git_inner as sync_git_core;
pub(crate) use commands::unstage_git_file_inner as unstage_git_file_core;
pub(crate) use diff::collect_workspace_diff as collect_workspace_diff_core;
pub(crate) use diff::get_git_commit_diff_inner as get_git_commit_diff_core;
pub(crate) use diff::get_git_diffs_inner as get_git_diffs_core;
pub(crate) use diff::get_git_status_inner as get_git_status_core;
pub(crate) use github::checkout_github_pull_request_inner as checkout_github_pull_request_core;
pub(crate) use github::get_github_issues_inner as get_github_issues_core;
pub(crate) use github::get_github_pull_request_comments_inner as get_github_pull_request_comments_core;
pub(crate) use github::get_github_pull_request_diff_inner as get_github_pull_request_diff_core;
pub(crate) use github::get_github_pull_requests_inner as get_github_pull_requests_core;
pub(crate) use log::get_git_log_inner as get_git_log_core;
pub(crate) use log::get_git_remote_inner as get_git_remote_core;
