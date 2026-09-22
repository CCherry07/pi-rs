//! Durable ownership of explicitly created Git worktrees, separate from workspace values.
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use git2::Repository;
use pi_core::{WorkspaceRootId, WorkspaceSpec};
use pi_sdk::projects::{realize_worktrees, Project, WorkspaceWorktreeMapping};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::git_targets::{self, GitTarget};
use crate::state::AppState;
use crate::storage::write_json_atomic;

mod operations;
pub(crate) use operations::{create, remove};
pub(crate) mod delivery;
mod delivery_apply;
pub(crate) mod delivery_execution;
mod delivery_git;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorktreeRequest {
    pub parent_id: String,
    pub thread_id: Option<String>,
    pub name: String,
    pub copy_agents_md: bool,
    pub execution_root_id: Option<WorkspaceRootId>,
    pub checkouts: Vec<CheckoutRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckoutRequest {
    pub target: GitTarget,
    pub branch: String,
    pub start_point: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorktreePlan {
    pub id: String,
    pub parent_id: String,
    pub name: String,
    pub source: WorkspaceSpec,
    pub workspace: WorkspaceSpec,
    pub checkouts: Vec<PlannedCheckout>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlannedCheckout {
    source_workdir: PathBuf,
    destination: PathBuf,
    branch: String,
    start_oid: String,
    root_ids: Vec<WorkspaceRootId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum GroupStatus {
    Prepared,
    Creating,
    Ready,
    CleanupRequired,
    Removing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum MemberProgress {
    Pending,
    Creating,
    Created,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Member {
    git_dir: PathBuf,
    common_dir: PathBuf,
    expected_git_dir: PathBuf,
    progress: MemberProgress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    version: u32,
    plan: WorktreePlan,
    thread_id: Option<String>,
    source_execution_git_dir: Option<PathBuf>,
    execution_root_id: Option<WorkspaceRootId>,
    group_dir: PathBuf,
    copy_agents_md: bool,
    setup_script: Option<String>,
    status: GroupStatus,
    errors: Vec<String>,
    members: Vec<Member>,
    #[serde(default)]
    deliveries: Vec<delivery_execution::DeliveryAttempt>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedWorktreeSummary {
    id: String,
    name: String,
    status: GroupStatus,
    errors: Vec<String>,
    member_count: usize,
}

fn canonical_source(source: &WorkspaceSpec) -> Result<WorkspaceSpec, String> {
    Project::from_workspace("source", "Source", source)?.resolve()
}

/// Resolve even a not-yet-created storage directory without creating it during preview.
fn future_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err("Worktree storage must be an absolute path without parent traversal".into());
    }
    if path.try_exists().map_err(|error| error.to_string())? {
        let path = path.canonicalize().map_err(|error| error.to_string())?;
        if !path.is_dir() {
            return Err("Worktree storage must be a directory".into());
        }
        return Ok(path);
    }
    let parent = path.parent().ok_or("Worktree storage has no parent")?;
    Ok(future_path(parent)?.join(path.file_name().ok_or("Invalid storage path")?))
}

fn checkout_identity(path: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let repo = Repository::discover(path).map_err(|error| error.to_string())?;
    let workdir = repo
        .workdir()
        .ok_or("Bare repositories cannot create workspace worktrees")?;
    let canonical = |path: &Path| path.canonicalize().map_err(|error| error.to_string());
    Ok((
        canonical(workdir)?,
        canonical(repo.path())?,
        canonical(repo.commondir())?,
    ))
}

fn ensure_new_branch(repo: &Repository, branch: &str) -> Result<(), String> {
    let reference = format!("refs/heads/{branch}");
    if branch == "HEAD"
        || branch.starts_with('-')
        || branch.contains('\0')
        || !git2::Reference::is_valid_name(&reference)
    {
        return Err(format!("Invalid new branch name: {branch}"));
    }
    for existing in repo
        .branches(Some(git2::BranchType::Local))
        .map_err(|error| error.to_string())?
    {
        let (existing, _) = existing.map_err(|error| error.to_string())?;
        let Some(name) = existing
            .get()
            .name()
            .and_then(|name| name.strip_prefix("refs/heads/"))
        else {
            continue;
        };
        if branch == name {
            return Err(format!(
                "Branch {branch} already exists; choose a new branch"
            ));
        }
        if conflicting_branches(branch, name) {
            return Err(format!(
                "Branch {branch} conflicts with existing branch {name}"
            ));
        }
    }
    Ok(())
}

fn conflicting_branches(first: &str, second: &str) -> bool {
    first == second
        || first.starts_with(&format!("{second}/"))
        || second.starts_with(&format!("{first}/"))
}

fn build_record(
    request: WorktreeRequest,
    source: WorkspaceSpec,
    storage_root: &Path,
    setup_script: Option<String>,
) -> Result<Record, String> {
    let source = canonical_source(&source)?;
    if request.name.trim().is_empty() || request.checkouts.is_empty() {
        return Err("A name and at least one checkout are required".into());
    }
    let id = Uuid::new_v4().to_string();
    let group_dir = future_path(storage_root)?.join(&id);
    let mut mappings = Vec::new();
    let mut checkouts = Vec::new();
    let mut members = Vec::new();
    let mut seen_checkouts = HashSet::new();
    let mut seen_branches: HashSet<(PathBuf, String)> = HashSet::new();
    let mut warnings = vec![
        "New worktrees start at the displayed commits; source working-directory changes are not copied. Created branches are retained when worktrees are removed.".into(),
    ];
    for (index, selected) in request.checkouts.iter().enumerate() {
        if !matches!(selected.target, GitTarget::Checkout { .. }) {
            return Err("Select a Git checkout before creating a worktree".into());
        }
        let workdir = git_targets::resolve(&source, Some(&selected.target))?;
        let (_, git_dir, common_dir) = checkout_identity(&workdir)?;
        if !seen_checkouts.insert(git_dir.clone()) {
            return Err("A checkout was selected more than once".into());
        }
        let branch = selected.branch.trim().to_string();
        let repo = Repository::open(&workdir).map_err(|error| error.to_string())?;
        ensure_new_branch(&repo, &branch)?;
        if seen_branches
            .iter()
            .any(|(common, name)| *common == common_dir && conflicting_branches(&branch, name))
        {
            return Err("Linked worktrees in one repository require different new branches without path conflicts".into());
        }
        seen_branches.insert((common_dir.clone(), branch.clone()));
        let start_point = selected.start_point.trim();
        let commit = repo
            .revparse_single(if start_point.is_empty() {
                "HEAD"
            } else {
                start_point
            })
            .and_then(|object| object.peel_to_commit())
            .map_err(|error| format!("Cannot resolve start point for {branch}: {error}"))?;
        let start_oid = commit.id().to_string();
        let root_ids: Vec<_> = source
            .roots()
            .iter()
            .filter(|root| {
                checkout_identity(&root.path).is_ok_and(|(_, candidate, _)| candidate == git_dir)
            })
            .map(|root| root.id.clone())
            .collect();
        if root_ids.is_empty() {
            return Err(
                "A nested repository must be added as a project root before isolation".into(),
            );
        }
        let contains_execution_dir =
            checkout_identity(source.cwd()).is_ok_and(|(_, candidate, _)| candidate == git_dir);
        let tree = commit.tree().map_err(|error| error.to_string())?;
        for path in source
            .roots()
            .iter()
            .filter(|root| root_ids.contains(&root.id))
            .map(|root| root.path.as_path())
            .chain(contains_execution_dir.then_some(source.cwd()))
        {
            let offset = path
                .strip_prefix(&workdir)
                .map_err(|error| error.to_string())?;
            if !offset.as_os_str().is_empty()
                && !tree
                    .get_path(offset)
                    .is_ok_and(|entry| entry.kind() == Some(git2::ObjectType::Tree))
            {
                return Err(format!(
                    "Directory {} is absent from the selected start commit",
                    path.display()
                ));
            }
        }
        // Including the UUID prevents Git's admin-name suffix fallback after an interrupted add.
        let member_name = format!("repo-{}-{id}", index + 1);
        let destination = group_dir.join(&member_name);
        let expected_git_dir = common_dir.join("worktrees").join(&member_name);
        if expected_git_dir
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err("Worktree administrative directory already exists".into());
        }
        mappings.push(WorkspaceWorktreeMapping {
            source_checkout: workdir.clone(),
            worktree_path: destination.clone(),
            roots: root_ids.clone(),
            contains_execution_dir,
        });
        checkouts.push(PlannedCheckout {
            source_workdir: workdir,
            destination,
            branch,
            start_oid,
            root_ids,
        });
        members.push(Member {
            git_dir,
            common_dir,
            expected_git_dir,
            progress: MemberProgress::Pending,
        });
    }
    let workspace = realize_worktrees(&source, &mappings, request.execution_root_id.as_ref())?;
    for root in source.roots() {
        if !mappings
            .iter()
            .any(|mapping| mapping.roots.contains(&root.id))
        {
            warnings.push(format!(
                "Shared directory: {} ({})",
                root.name,
                root.path.display()
            ));
        }
    }
    Ok(Record {
        version: 1,
        source_execution_git_dir: checkout_identity(source.cwd()).ok().map(|(_, git, _)| git),
        plan: WorktreePlan {
            id,
            parent_id: request.parent_id,
            name: request.name.trim().into(),
            source,
            workspace,
            checkouts,
            warnings,
        },
        thread_id: request.thread_id,
        execution_root_id: request.execution_root_id,
        group_dir,
        copy_agents_md: request.copy_agents_md,
        setup_script,
        status: GroupStatus::Prepared,
        errors: Vec::new(),
        members,
        deliveries: Vec::new(),
    })
}

pub(crate) async fn prepare(
    state: &AppState,
    data_dir: &Path,
    request: WorktreeRequest,
    source: WorkspaceSpec,
) -> Result<WorktreePlan, String> {
    let parent = state
        .workspaces
        .lock()
        .await
        .get(&request.parent_id)
        .cloned()
        .ok_or("Parent project not found")?;
    let storage_root = if let Some(folder) = &parent.settings.worktrees_folder {
        PathBuf::from(folder)
    } else if let Some(folder) = &state.app_settings.lock().await.global_worktrees_folder {
        PathBuf::from(folder).join(&parent.id)
    } else {
        data_dir.join("worktrees").join(&parent.id)
    };
    let record = build_record(
        request,
        source,
        &storage_root,
        parent.settings.worktree_setup_script,
    )?;
    let _lock = lock(&state.storage_path, &record.plan.id)?;
    save(&state.storage_path, &record)?;
    Ok(record.plan)
}

fn verify_source(record: &Record, source: &WorkspaceSpec) -> Result<(), String> {
    if canonical_source(source)? != record.plan.source {
        return Err("Source workspace changed; prepare a new worktree preview".into());
    }
    if checkout_identity(source.cwd()).ok().map(|(_, git, _)| git)
        != record.source_execution_git_dir
    {
        return Err("Execution directory Git membership changed; prepare a new preview".into());
    }
    if future_path(record.group_dir.parent().ok_or("Invalid group path")?)?
        != record.group_dir.parent().ok_or("Invalid group path")?
        || record
            .group_dir
            .try_exists()
            .map_err(|error| error.to_string())?
    {
        return Err("Worktree destination changed; prepare a new preview".into());
    }
    for (checkout, member) in record.plan.checkouts.iter().zip(&record.members) {
        let identity = checkout_identity(&checkout.source_workdir)?;
        if identity
            != (
                checkout.source_workdir.clone(),
                member.git_dir.clone(),
                member.common_dir.clone(),
            )
        {
            return Err("Source Git checkout changed; prepare a new preview".into());
        }
        let actual_roots: Vec<_> = source
            .roots()
            .iter()
            .filter(|root| {
                checkout_identity(&root.path).is_ok_and(|(_, git, _)| git == member.git_dir)
            })
            .map(|root| root.id.clone())
            .collect();
        if actual_roots != checkout.root_ids {
            return Err("Root Git membership changed; prepare a new preview".into());
        }
        let repo = Repository::open(&checkout.source_workdir).map_err(|error| error.to_string())?;
        ensure_new_branch(&repo, &checkout.branch)?;
        repo.find_commit(
            git2::Oid::from_str(&checkout.start_oid).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Prepared commit is no longer available: {error}"))?;
        if member
            .expected_git_dir
            .try_exists()
            .map_err(|error| error.to_string())?
            || checkout
                .destination
                .try_exists()
                .map_err(|error| error.to_string())?
        {
            return Err("Prepared worktree path is already in use".into());
        }
    }
    Ok(())
}

fn journal_dir(storage: &Path) -> PathBuf {
    storage.with_file_name("managed-worktrees")
}

fn record_path(storage: &Path, id: &str) -> Result<PathBuf, String> {
    let uuid = Uuid::parse_str(id).map_err(|_| "Invalid managed worktree ID")?;
    if uuid.to_string() != id {
        return Err("Invalid managed worktree ID".into());
    }
    Ok(journal_dir(storage).join(format!("{id}.json")))
}

pub(crate) struct GroupLock(File);

impl Drop for GroupLock {
    fn drop(&mut self) {
        // Closing alone can leave the lock held by a concurrently forked child's inherited
        // descriptor until exec. End the operation's lock lifetime explicitly.
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock(storage: &Path, id: &str) -> Result<GroupLock, String> {
    let path = record_path(storage, id)?.with_extension("lock");
    fs::create_dir_all(journal_dir(storage)).map_err(|error| error.to_string())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.try_lock_exclusive()
        .map_err(|_| "Worktree operation already in progress; retry after it finishes")?;
    Ok(GroupLock(file))
}

fn save(storage: &Path, record: &Record) -> Result<(), String> {
    write_json_atomic(&record_path(storage, &record.plan.id)?, record)?;
    #[cfg(unix)]
    File::open(journal_dir(storage))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot synchronize managed worktree journal: {error}"))?;
    Ok(())
}

fn load(storage: &Path, id: &str) -> Result<Record, String> {
    let record: Record = serde_json::from_slice(
        &fs::read(record_path(storage, id)?)
            .map_err(|error| format!("Cannot read managed worktree {id}: {error}"))?,
    )
    .map_err(|error| format!("Invalid managed worktree record {id}: {error}"))?;
    if !matches!(record.version, 1 | 2)
        || record.plan.id != id
        || record.plan.checkouts.is_empty()
        || record.members.len() != record.plan.checkouts.len()
        || record.group_dir.file_name().and_then(|name| name.to_str()) != Some(id)
    {
        return Err("Invalid or unsupported managed worktree record".into());
    }
    delivery_execution::validate_records(&record)?;
    if (record.status == GroupStatus::Prepared
        && record
            .members
            .iter()
            .any(|member| member.progress != MemberProgress::Pending))
        || (record.status == GroupStatus::Ready
            && record
                .members
                .iter()
                .any(|member| member.progress != MemberProgress::Created))
    {
        return Err("Invalid managed worktree progress".into());
    }
    let mut destinations = HashSet::new();
    for (checkout, member) in record.plan.checkouts.iter().zip(&record.members) {
        if checkout.destination.parent() != Some(record.group_dir.as_path())
            || !destinations.insert(&checkout.destination)
            || member.expected_git_dir
                != member.common_dir.join("worktrees").join(
                    checkout
                        .destination
                        .file_name()
                        .ok_or("Invalid member path")?,
                )
            || [
                &checkout.destination,
                &member.expected_git_dir,
                &member.common_dir,
                &member.git_dir,
            ]
            .iter()
            .any(|path| {
                !path.is_absolute() || path.components().any(|part| part == Component::ParentDir)
            })
        {
            return Err("Invalid managed worktree paths".into());
        }
    }
    Project::from_workspace(&record.plan.parent_id, "Source", &record.plan.source)?;
    let mappings: Vec<_> = record
        .plan
        .checkouts
        .iter()
        .zip(&record.members)
        .map(|(checkout, member)| WorkspaceWorktreeMapping {
            source_checkout: checkout.source_workdir.clone(),
            worktree_path: checkout.destination.clone(),
            roots: checkout.root_ids.clone(),
            contains_execution_dir: record.source_execution_git_dir.as_ref()
                == Some(&member.git_dir),
        })
        .collect();
    let expected = realize_worktrees(
        &record.plan.source,
        &mappings,
        record.execution_root_id.as_ref(),
    )?;
    if expected != record.plan.workspace {
        return Err("Saved workspace does not match the prepared checkout mappings".into());
    }
    Ok(record)
}

pub(crate) fn has_record(storage: &Path, id: &str) -> Result<bool, String> {
    let Ok(path) = record_path(storage, id) else {
        return Ok(false);
    };
    path.try_exists().map_err(|error| error.to_string())
}

pub(crate) fn source_context(storage: &Path, id: &str) -> Result<(String, Option<String>), String> {
    let record = load(storage, id)?;
    Ok((record.plan.parent_id, record.thread_id))
}

pub(crate) fn discard(storage: &Path, id: &str) -> Result<(), String> {
    let _lock = lock(storage, id)?;
    let record = load(storage, id)?;
    if record.status != GroupStatus::Prepared {
        return Err("Worktree creation already started; use managed cleanup".into());
    }
    fs::remove_file(record_path(storage, id)?).map_err(|error| error.to_string())
}

pub(crate) fn list(storage: &Path) -> Result<Vec<ManagedWorktreeSummary>, String> {
    let dir = journal_dir(storage);
    if !dir.try_exists().map_err(|error| error.to_string())? {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|name| name.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("Invalid journal filename")?;
        let record = load(storage, id)?;
        result.push(ManagedWorktreeSummary {
            id: record.plan.id,
            name: record.plan.name,
            status: record.status,
            errors: record.errors,
            member_count: record.members.len(),
        });
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

#[cfg(test)]
mod tests;
