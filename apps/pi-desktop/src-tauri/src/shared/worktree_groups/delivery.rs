//! Read-only delivery observations. A preview is not an executable merge authorization.
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, RepositoryState, Status, StatusOptions};
use pi_core::{WorkspaceRoot, WorkspaceRootId, WorkspaceSpec};
use serde::Serialize;

use super::delivery_execution::DeliveryAttempt;
use super::delivery_git::{compare, MergeComparison};
use super::{
    checkout_identity, load, lock, GroupStatus, Member, MemberProgress, PlannedCheckout, Record,
};
use crate::shared::git_targets::{self, GitTarget};

const CHANGE_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryHead {
    pub oid: String,
    pub branch: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BranchTip {
    pub name: String,
    pub oid: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryChange {
    pub path: String,
    pub index_status: String,
    pub worktree_status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryCheckout {
    pub key: String,
    pub workdir: PathBuf,
    pub origin_workdir: PathBuf,
    pub root_ids: Vec<WorkspaceRootId>,
    pub created_branch: String,
    pub start_oid: String,
    pub head: Option<DeliveryHead>,
    pub changes: Vec<DeliveryChange>,
    pub changes_truncated: bool,
    pub target_branches: Vec<BranchTip>,
    pub default_target_branch: Option<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryOverview {
    pub workspace_id: String,
    pub name: String,
    pub checkouts: Vec<DeliveryCheckout>,
    pub shared_roots: Vec<WorkspaceRoot>,
    pub attempts: Vec<DeliveryAttempt>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryTarget {
    pub oid: String,
    pub branch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryPreview {
    pub workspace_id: String,
    pub checkout_key: String,
    pub source_workdir: PathBuf,
    pub target_workdir: PathBuf,
    pub source: DeliveryHead,
    pub target: DeliveryTarget,
    pub source_changes: Vec<DeliveryChange>,
    pub target_changes: Vec<DeliveryChange>,
    pub source_changes_truncated: bool,
    pub target_changes_truncated: bool,
    pub comparison: MergeComparison,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn overview(
    storage: &Path,
    id: &str,
    workspace: &WorkspaceSpec,
) -> Result<DeliveryOverview, String> {
    let _lock = lock(storage, id)?;
    let record = ready_record(storage, id)?;
    let checkouts = record
        .plan
        .checkouts
        .iter()
        .zip(&record.members)
        .map(|(checkout, member)| {
            let mut result = DeliveryCheckout {
                key: member.expected_git_dir.to_string_lossy().into_owned(),
                workdir: checkout.destination.clone(),
                origin_workdir: checkout.source_workdir.clone(),
                root_ids: checkout.root_ids.clone(),
                created_branch: checkout.branch.clone(),
                start_oid: checkout.start_oid.clone(),
                head: None,
                changes: Vec::new(),
                changes_truncated: false,
                target_branches: Vec::new(),
                default_target_branch: None,
                warnings: record.errors.clone(),
                error: None,
            };
            if let Err(error) = observe_checkout(workspace, checkout, member, &mut result) {
                result.error = Some(error);
            }
            result
        })
        .collect();
    let shared_roots = workspace
        .roots()
        .iter()
        .filter(|root| {
            !record.plan.checkouts.iter().any(|checkout| {
                checkout.root_ids.contains(&root.id)
                    && record
                        .plan
                        .workspace
                        .roots()
                        .iter()
                        .any(|planned| planned.id == root.id && planned.path == root.path)
            })
        })
        .cloned()
        .collect();
    Ok(DeliveryOverview {
        workspace_id: id.into(),
        name: record.plan.name,
        checkouts,
        shared_roots,
        attempts: record.deliveries,
    })
}

pub(crate) fn preview(
    storage: &Path,
    id: &str,
    workspace: &WorkspaceSpec,
    checkout_key: &str,
    target_branch: &str,
) -> Result<DeliveryPreview, String> {
    let _lock = lock(storage, id)?;
    let record = ready_record(storage, id)?;
    preview_record(&record, id, workspace, checkout_key, target_branch)
}

pub(super) fn preview_record(
    record: &Record,
    id: &str,
    workspace: &WorkspaceSpec,
    checkout_key: &str,
    target_branch: &str,
) -> Result<DeliveryPreview, String> {
    let (checkout, member) = record
        .plan
        .checkouts
        .iter()
        .zip(&record.members)
        .find(|(_, member)| member.expected_git_dir.to_string_lossy() == checkout_key)
        .ok_or("Selected checkout is not a member of this managed worktree group")?;
    let repo = managed_repository(workspace, checkout, member)?;
    let origin = origin_repository(checkout, member)?;
    let source = head(&repo)?;
    let origin_head = head(&origin)?;
    let target = local_branch(&repo, target_branch)?;
    let source_oid = git2::Oid::from_str(&source.oid).map_err(|error| error.to_string())?;
    let target_oid = git2::Oid::from_str(&target.oid).map_err(|error| error.to_string())?;
    let source_status = working_changes(&repo)?;
    let target_status = working_changes(&origin)?;
    let target_ignores_case = origin
        .config()
        .map_err(|error| error.to_string())?
        .get_bool("core.ignorecase")
        .unwrap_or(false);
    let comparison = compare(&repo, source_oid, target_oid)?;
    let mut blockers = operation_blockers(&repo, "Worktree");
    blockers.extend(operation_blockers(&origin, "Original checkout"));
    blockers.extend(super::delivery_apply::support_blockers(
        &origin, source_oid, target_oid,
    )?);
    if origin_head.branch.as_deref() != Some(target_branch) {
        blockers.push(format!(
            "The original checkout must be on target branch {target_branch} before merging"
        ));
    }
    if source.branch.as_deref() == Some(target_branch) {
        blockers.push("The source and target are the same branch".into());
    }
    if target_status.has_nonignored {
        blockers.push("The original checkout has staged, unstaged, or untracked changes".into());
    }
    let mut warnings = vec![
        "This preview describes the displayed commits. Refresh it after either repository changes."
            .into(),
    ];
    if source_status.has_nonignored {
        warnings.push("Uncommitted worktree changes are excluded; commit them before delivering those changes".into());
    }
    if source_status.has_ignored || target_status.has_ignored {
        warnings.push("Ignored files are excluded from the commit comparison".into());
    }
    if target_status.has_ignored
        && (target_status.truncated
            || comparison.files_truncated
            || comparison.files.iter().any(|file| {
                target_status
                    .entries
                    .iter()
                    .filter(|entry| entry.worktree_status == "!")
                    .any(|entry| {
                        ignored_path_overlaps(
                            &checkout.source_workdir,
                            &entry.path,
                            &file.path,
                            target_ignores_case,
                        )
                    })
            }))
    {
        blockers.push("Incoming files may overlap ignored files in the original checkout; inspect them before merging".into());
    }
    // Avoid returning a result composed from different branch selections while an agent or
    // another Git client is writing. Working-directory statuses remain point-in-time observations.
    if head(&repo)? != source
        || head(&origin)? != origin_head
        || local_branch(&repo, target_branch)?.oid != target.oid
    {
        return Err("Repository branches changed during preview; refresh and try again".into());
    }
    verified_repository(
        &checkout.destination,
        &member.expected_git_dir,
        &member.common_dir,
    )?;
    verified_repository(
        &checkout.source_workdir,
        &member.git_dir,
        &member.common_dir,
    )?;
    Ok(DeliveryPreview {
        workspace_id: id.into(),
        checkout_key: checkout_key.into(),
        source_workdir: checkout.destination.clone(),
        target_workdir: checkout.source_workdir.clone(),
        source,
        target: DeliveryTarget {
            oid: target.oid,
            branch: target.name,
        },
        source_changes: source_status.entries,
        target_changes: target_status.entries,
        source_changes_truncated: source_status.truncated,
        target_changes_truncated: target_status.truncated,
        comparison,
        blockers,
        warnings,
    })
}

pub(super) fn ready_record(storage: &Path, id: &str) -> Result<Record, String> {
    let record = load(storage, id)?;
    let cleanup_left_every_member = record.status == GroupStatus::CleanupRequired
        && record
            .members
            .iter()
            .all(|member| member.progress == MemberProgress::Created);
    if record.status != GroupStatus::Ready && !cleanup_left_every_member {
        return Err(
            "Finish worktree creation or resolve pending cleanup before reviewing delivery".into(),
        );
    }
    Ok(record)
}

fn observe_checkout(
    workspace: &WorkspaceSpec,
    checkout: &PlannedCheckout,
    member: &Member,
    result: &mut DeliveryCheckout,
) -> Result<(), String> {
    let repo = managed_repository(workspace, checkout, member)?;
    let initial_head = head(&repo)?;
    let changes = working_changes(&repo)?;
    result.changes = changes.entries;
    result.changes_truncated = changes.truncated;
    for branch in repo
        .branches(Some(BranchType::Local))
        .map_err(|error| error.to_string())?
    {
        let (branch, _) = branch.map_err(|error| error.to_string())?;
        let Some(name) = branch.name().map_err(|error| error.to_string())? else {
            continue;
        };
        let oid = branch
            .get()
            .peel_to_commit()
            .map_err(|error| error.to_string())?
            .id();
        result.target_branches.push(BranchTip {
            name: name.into(),
            oid: oid.to_string(),
        });
    }
    result.target_branches.sort_by(|a, b| a.name.cmp(&b.name));
    match origin_repository(checkout, member).and_then(|repo| head(&repo)) {
        Ok(origin_head) => {
            result.default_target_branch = origin_head.branch;
            if result.default_target_branch.is_none() {
                result.warnings.push(
                    "The original checkout has a detached HEAD; select a local target branch"
                        .into(),
                );
            }
        }
        Err(error) => result.warnings.push(error),
    }
    result
        .warnings
        .extend(operation_blockers(&repo, "Worktree"));
    if head(&repo)? != initial_head {
        return Err("Worktree branch changed during inspection; refresh and try again".into());
    }
    result.head = Some(initial_head);
    Ok(())
}

pub(super) fn managed_repository(
    workspace: &WorkspaceSpec,
    checkout: &PlannedCheckout,
    member: &Member,
) -> Result<Repository, String> {
    if member.progress != MemberProgress::Created {
        return Err("This worktree is not available for delivery".into());
    }
    let target = GitTarget::Checkout {
        key: member.expected_git_dir.to_string_lossy().into_owned(),
        workdir: checkout.destination.clone(),
    };
    git_targets::resolve(workspace, Some(&target))?;
    verified_repository(
        &checkout.destination,
        &member.expected_git_dir,
        &member.common_dir,
    )
}

pub(super) fn origin_repository(
    checkout: &PlannedCheckout,
    member: &Member,
) -> Result<Repository, String> {
    verified_repository(
        &checkout.source_workdir,
        &member.git_dir,
        &member.common_dir,
    )
    .map_err(|error| format!("Original checkout is unavailable: {error}"))
}

fn verified_repository(
    path: &Path,
    git_dir: &Path,
    common_dir: &Path,
) -> Result<Repository, String> {
    if checkout_identity(path)?
        != (
            path.to_path_buf(),
            git_dir.to_path_buf(),
            common_dir.to_path_buf(),
        )
    {
        return Err(format!(
            "Git checkout identity changed at {}",
            path.display()
        ));
    }
    Repository::open(path).map_err(|error| error.to_string())
}

fn head(repo: &Repository) -> Result<DeliveryHead, String> {
    let head = repo.head().map_err(|error| error.to_string())?;
    Ok(DeliveryHead {
        oid: head
            .peel_to_commit()
            .map_err(|error| error.to_string())?
            .id()
            .to_string(),
        branch: if head.is_branch() {
            head.shorthand().map(str::to_owned)
        } else {
            None
        },
    })
}

fn local_branch(repo: &Repository, name: &str) -> Result<BranchTip, String> {
    if name.trim() != name
        || name.is_empty()
        || name.contains('\0')
        || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
    {
        return Err("Select a local target branch".into());
    }
    let branch = repo
        .find_branch(name, BranchType::Local)
        .map_err(|error| format!("Target branch {name} is unavailable: {error}"))?;
    Ok(BranchTip {
        name: name.into(),
        oid: branch
            .get()
            .peel_to_commit()
            .map_err(|error| error.to_string())?
            .id()
            .to_string(),
    })
}

fn operation_blockers(repo: &Repository, label: &str) -> Vec<String> {
    let mut blockers = Vec::new();
    if repo.state() != RepositoryState::Clean {
        blockers.push(format!(
            "{label} has an unfinished Git operation: {:?}",
            repo.state()
        ));
    }
    if repo.path().join("locked").exists() || repo.path().join("index.lock").exists() {
        blockers.push(format!(
            "{label} is locked by Git or another worktree operation"
        ));
    }
    blockers
}

struct WorkingChanges {
    entries: Vec<DeliveryChange>,
    truncated: bool,
    has_nonignored: bool,
    has_ignored: bool,
}

fn working_changes(repo: &Repository) -> Result<WorkingChanges, String> {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .include_unreadable(true)
        .no_refresh(true)
        .update_index(false)
        .sort_case_sensitively(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|error| error.to_string())?;
    let has_nonignored = statuses
        .iter()
        .any(|entry| !entry.status().contains(Status::IGNORED));
    let has_ignored = statuses
        .iter()
        .any(|entry| entry.status().contains(Status::IGNORED));
    let entries = statuses
        .iter()
        .take(CHANGE_LIMIT)
        .map(|entry| {
            let status = entry.status();
            let (index, worktree) = if status.contains(Status::CONFLICTED) {
                ("U", "U")
            } else if status.contains(Status::IGNORED) {
                ("!", "!")
            } else if status.contains(Status::WT_NEW)
                && !status.intersects(
                    Status::INDEX_NEW
                        | Status::INDEX_RENAMED
                        | Status::INDEX_MODIFIED
                        | Status::INDEX_DELETED
                        | Status::INDEX_TYPECHANGE,
                )
            {
                ("?", "?")
            } else {
                (status_letter(status, true), status_letter(status, false))
            };
            let path = entry
                .index_to_workdir()
                .and_then(|delta| {
                    delta
                        .new_file()
                        .path()
                        .map(|path| path.to_string_lossy().into_owned())
                })
                .or_else(|| {
                    entry.head_to_index().and_then(|delta| {
                        delta
                            .new_file()
                            .path()
                            .map(|path| path.to_string_lossy().into_owned())
                    })
                })
                .unwrap_or_else(|| String::from_utf8_lossy(entry.path_bytes()).into_owned());
            DeliveryChange {
                path,
                index_status: index.into(),
                worktree_status: worktree.into(),
            }
        })
        .collect();
    Ok(WorkingChanges {
        entries,
        truncated: statuses.len() > CHANGE_LIMIT,
        has_nonignored,
        has_ignored,
    })
}

fn status_letter(status: Status, index: bool) -> &'static str {
    let flags = if index {
        [
            (Status::INDEX_NEW, "A"),
            (Status::INDEX_DELETED, "D"),
            (Status::INDEX_RENAMED, "R"),
            (Status::INDEX_TYPECHANGE, "T"),
            (Status::INDEX_MODIFIED, "M"),
        ]
    } else {
        [
            (Status::WT_NEW, "A"),
            (Status::WT_DELETED, "D"),
            (Status::WT_RENAMED, "R"),
            (Status::WT_TYPECHANGE, "T"),
            (Status::WT_MODIFIED, "M"),
        ]
    };
    flags
        .into_iter()
        .find(|(flag, _)| status.contains(*flag))
        .map(|(_, value)| value)
        .unwrap_or(if status.contains(Status::WT_UNREADABLE) && !index {
            "!"
        } else {
            " "
        })
}

fn overlaps(first: &str, second: &str) -> bool {
    let first = first.trim_end_matches('/');
    let second = second.trim_end_matches('/');
    first == second
        || first.starts_with(&format!("{second}/"))
        || second.starts_with(&format!("{first}/"))
}

fn ignored_path_overlaps(root: &Path, ignored: &str, incoming: &str, ignores_case: bool) -> bool {
    if overlaps(ignored, incoming) {
        return true;
    }
    let folded_overlap = overlaps(&ignored.to_lowercase(), &incoming.to_lowercase());
    if ignores_case && folded_overlap {
        return true;
    }
    if !folded_overlap && ignored.is_ascii() && incoming.is_ascii() {
        return false;
    }
    // Git's setting can be manually disabled on a case-insensitive filesystem. Compare the
    // existing ancestors at the same depth; the incoming leaf need not exist. This also lets
    // filesystem lookup handle Unicode case/normalization aliases without creating probes.
    let ignored = Path::new(ignored);
    let incoming = Path::new(incoming);
    let depth = ignored
        .components()
        .count()
        .min(incoming.components().count());
    let prefix = |path: &Path| root.join(path.components().take(depth).collect::<PathBuf>());
    let (first, second) = match (
        prefix(ignored).canonicalize(),
        prefix(incoming).canonicalize(),
    ) {
        (Ok(first), Ok(second)) => (first, second),
        (Err(error), _) | (_, Err(error)) => {
            return error.kind() != std::io::ErrorKind::NotFound;
        }
    };
    if first == second {
        return true;
    }
    // macOS realpath retains the requested case, even when both spellings resolve to one inode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (first.metadata(), second.metadata()) {
            (Ok(first), Ok(second)) => first.dev() == second.dev() && first.ino() == second.ino(),
            _ => true,
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
