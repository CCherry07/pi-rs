use std::fs;
use std::io::Write;
use std::path::Path;

use pi_core::WorkspaceSpec;
use pi_sdk::projects::Project;

use super::{
    checkout_identity, load, lock, record_path, save, verify_source, GroupStatus, Member,
    MemberProgress, PlannedCheckout, Record,
};
use crate::shared::git_core;
use crate::state::AppState;
use crate::storage::write_workspaces;
use crate::types::{WorkspaceEntry, WorkspaceInfo, WorkspaceKind, WorkspaceSettings, WorktreeInfo};

pub(crate) async fn create(
    state: &AppState,
    id: &str,
    current_source: WorkspaceSpec,
) -> Result<WorkspaceInfo, String> {
    let _lock = lock(&state.storage_path, id)?;
    let mut record = load(&state.storage_path, id)?;
    if record.status != GroupStatus::Prepared {
        return Err(
            "Worktree creation already started; inspect or clean up the saved operation".into(),
        );
    }
    verify_source(&record, &current_source)?;
    if state.workspaces.lock().await.contains_key(id)
        || state
            .project_store()
            .list()?
            .iter()
            .any(|project| project.id == id)
    {
        return Err("The managed project identity is already in use".into());
    }
    record.status = GroupStatus::Creating;
    save(&state.storage_path, &record)?;

    let result = create_inner(state, &mut record).await;
    match result {
        Ok(info) => Ok(info),
        Err(error) => {
            record.status = GroupStatus::CleanupRequired;
            record.errors = vec![error.clone()];
            if let Err(journal_error) = save(&state.storage_path, &record) {
                record
                    .errors
                    .push(format!("Could not save cleanup state: {journal_error}"));
            }
            // The journal already exists before the first Git mutation. Even when a newer
            // journal write fails, identity checks can reconcile the interrupted add.
            match cleanup_members(state, &mut record, false).await {
                Ok(()) => {
                    if let Err(cleanup_error) = unpublish(state, &record).await {
                        record.errors.push(cleanup_error);
                    }
                    if let Err(cleanup_error) = remove_empty_group(&record) {
                        record.errors.push(cleanup_error);
                    }
                }
                Err(cleanup_error) => record.errors.push(cleanup_error),
            }
            record.status = GroupStatus::CleanupRequired;
            let detail = format!(
                "{}. Created branches were retained. Inspect managed cleanup for {}.",
                record.errors.join("; "),
                record.plan.id
            );
            if let Err(journal_error) = save(&state.storage_path, &record) {
                return Err(format!(
                    "{detail} Could not save cleanup state: {journal_error}"
                ));
            }
            Err(detail)
        }
    }
}

async fn create_inner(state: &AppState, record: &mut Record) -> Result<WorkspaceInfo, String> {
    let parent = record
        .group_dir
        .parent()
        .ok_or("Invalid worktree group path")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::create_dir(&record.group_dir)
        .map_err(|error| format!("Cannot reserve worktree group directory: {error}"))?;
    for index in 0..record.members.len() {
        record.members[index].progress = MemberProgress::Creating;
        save(&state.storage_path, record)?;
        let checkout = &record.plan.checkouts[index];
        let destination = path_argument(&checkout.destination)?;
        git_core::run_git_command(
            &checkout.source_workdir,
            &[
                "worktree",
                "add",
                "-b",
                &checkout.branch,
                "--",
                destination,
                &checkout.start_oid,
            ],
        )
        .await
        .map_err(|error| format!("Could not create {}: {error}", checkout.branch))?;
        if inspect_member(checkout, &record.members[index], true).await? != MemberState::Present {
            return Err(format!(
                "Created worktree is missing: {}",
                checkout.destination.display()
            ));
        }
        record.members[index].progress = MemberProgress::Created;
        save(&state.storage_path, record)?;
    }
    let project =
        Project::from_workspace(&record.plan.id, &record.plan.name, &record.plan.workspace)?;
    if project.resolve()? != record.plan.workspace {
        return Err("Created workspace directories differ from the prepared paths".into());
    }
    if record.copy_agents_md {
        for checkout in &record.plan.checkouts {
            if let Err(error) = copy_agents_file(checkout) {
                record.plan.warnings.push(error);
            }
        }
    }
    let entry = WorkspaceEntry {
        id: record.plan.id.clone(),
        name: record.plan.name.clone(),
        path: record.plan.workspace.cwd().to_string_lossy().into_owned(),
        kind: WorkspaceKind::Worktree,
        parent_id: Some(record.plan.parent_id.clone()),
        worktree: Some(WorktreeInfo {
            branch: record.plan.checkouts[0].branch.clone(),
            managed: true,
        }),
        settings: WorkspaceSettings {
            worktree_setup_script: record.setup_script.clone(),
            ..Default::default()
        },
    };
    let mut current = state.workspaces.lock().await;
    if current.contains_key(&entry.id) {
        return Err("The managed project identity is already in use".into());
    }
    let mut next = current.clone();
    next.insert(entry.id.clone(), entry.clone());
    state.project_store().upsert(project.clone())?;
    write_workspaces(
        &state.storage_path,
        &next.values().cloned().collect::<Vec<_>>(),
    )?;
    record.status = GroupStatus::Ready;
    save(&state.storage_path, record)?;
    *current = next;
    Ok(WorkspaceInfo {
        project: Some(project),
        id: entry.id,
        name: entry.name,
        path: entry.path,
        kind: entry.kind,
        parent_id: entry.parent_id,
        worktree: entry.worktree,
        settings: entry.settings,
    })
}

pub(crate) async fn remove(state: &AppState, id: &str, force: bool) -> Result<(), String> {
    let _lock = lock(&state.storage_path, id)?;
    let mut record = load(&state.storage_path, id)?;
    super::delivery_execution::ensure_cleanup_allowed(&record)?;
    if record.status == GroupStatus::Prepared {
        return fs::remove_file(record_path(&state.storage_path, id)?)
            .map_err(|error| error.to_string());
    }
    let _delivery_guards =
        super::delivery_execution::lock_for_cleanup(&state.storage_path, &record)?;
    let result = async {
        cleanup_members(state, &mut record, force).await?;
        unpublish(state, &record).await?;
        remove_empty_group(&record)?;
        fs::remove_file(record_path(&state.storage_path, id)?).map_err(|error| error.to_string())
    }
    .await;
    if let Err(error) = result {
        record.status = GroupStatus::CleanupRequired;
        record.errors = vec![error.clone()];
        return match save(&state.storage_path, &record) {
            Ok(()) => Err(error),
            Err(journal_error) => Err(format!(
                "{error}; could not save cleanup state: {journal_error}"
            )),
        };
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum MemberState {
    Absent,
    Present,
    MissingDirectory,
}

fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Cannot inspect {}: {error}", path.display())),
    }
}

fn path_argument(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("Git path is not valid UTF-8: {}", path.display()))
}

async fn inspect_member(
    checkout: &PlannedCheckout,
    member: &Member,
    force: bool,
) -> Result<MemberState, String> {
    let destination_exists = exists(&checkout.destination)?;
    let admin_exists = exists(&member.expected_git_dir)?;
    if !destination_exists && !admin_exists {
        return Ok(MemberState::Absent);
    }
    let changed = || {
        format!(
            "Worktree identity changed at {}; contents were preserved",
            checkout.destination.display()
        )
    };
    if !admin_exists
        || member
            .expected_git_dir
            .canonicalize()
            .map_err(|_| changed())?
            != member.expected_git_dir
        || member.common_dir.canonicalize().map_err(|_| changed())? != member.common_dir
    {
        return Err(changed());
    }
    if exists(&member.expected_git_dir.join("locked"))? {
        return Err(format!(
            "Worktree is locked: {}",
            checkout.destination.display()
        ));
    }
    let registration =
        fs::read_to_string(member.expected_git_dir.join("gitdir")).map_err(|_| changed())?;
    if Path::new(registration.trim_end_matches(['\r', '\n'])) != checkout.destination.join(".git") {
        return Err(changed());
    }
    // Check the common directory even if the checkout directory has already disappeared.
    let common =
        fs::read_to_string(member.expected_git_dir.join("commondir")).map_err(|_| changed())?;
    if member
        .expected_git_dir
        .join(common.trim_end_matches(['\r', '\n']))
        .canonicalize()
        .map_err(|_| changed())?
        != member.common_dir
    {
        return Err(changed());
    }
    if !destination_exists {
        return Ok(MemberState::MissingDirectory);
    }
    if fs::symlink_metadata(&checkout.destination)
        .map_err(|_| changed())?
        .file_type()
        .is_symlink()
        || checkout_identity(&checkout.destination).map_err(|_| changed())?
            != (
                checkout.destination.clone(),
                member.expected_git_dir.clone(),
                member.common_dir.clone(),
            )
    {
        return Err(changed());
    }
    if !force {
        let status = git_core::run_git_command_bytes(
            &checkout.destination,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ],
        )
        .await?;
        if !status.is_empty() {
            return Err(format!(
                "Worktree has tracked, untracked, or ignored changes: {}. Keep it, or explicitly choose to discard these files.",
                checkout.destination.display()
            ));
        }
    }
    Ok(MemberState::Present)
}

async fn cleanup_members(state: &AppState, record: &mut Record, force: bool) -> Result<(), String> {
    // Preflight the entire group before removing any member. Pending paths were never
    // claimed by this operation and must not be interpreted as owned directories.
    for (checkout, member) in record.plan.checkouts.iter().zip(&record.members) {
        if matches!(
            member.progress,
            MemberProgress::Creating | MemberProgress::Created
        ) {
            inspect_member(checkout, member, force).await?;
        }
    }
    record.status = GroupStatus::Removing;
    save(&state.storage_path, record)?;
    for index in (0..record.members.len()).rev() {
        if !matches!(
            record.members[index].progress,
            MemberProgress::Creating | MemberProgress::Created
        ) {
            continue;
        }
        let checkout = &record.plan.checkouts[index];
        let member = &record.members[index];
        // Recheck immediately before mutation as other Git clients do not share our lock.
        if inspect_member(checkout, member, force).await? != MemberState::Absent {
            let common = path_argument(&member.common_dir)?;
            let destination = path_argument(&checkout.destination)?;
            let mut args = vec!["--git-dir", common, "worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.extend(["--", destination]);
            git_core::run_git_command(&member.common_dir, &args)
                .await
                .map_err(|error| {
                    format!(
                        "Could not remove {}: {error}",
                        checkout.destination.display()
                    )
                })?;
        }
        record.members[index].progress = MemberProgress::Removed;
        save(&state.storage_path, record)?;
    }
    Ok(())
}

async fn unpublish(state: &AppState, record: &Record) -> Result<(), String> {
    let mut current = state.workspaces.lock().await;
    if let Some(entry) = current.get(&record.plan.id) {
        if !entry
            .worktree
            .as_ref()
            .is_some_and(|worktree| worktree.managed)
            || Path::new(&entry.path) != record.plan.workspace.cwd()
        {
            return Err(
                "Managed project association changed; the current association was preserved".into(),
            );
        }
    }
    let mut next = current.clone();
    next.remove(&record.plan.id);
    write_workspaces(
        &state.storage_path,
        &next.values().cloned().collect::<Vec<_>>(),
    )?;
    *current = next;
    // An orphan Project is hidden by the workspace list and can be retried from the
    // journal. A workspace entry whose Project has disappeared is not recoverable.
    state.project_store().remove(&record.plan.id)
}

fn remove_empty_group(record: &Record) -> Result<(), String> {
    if !record
        .members
        .iter()
        .any(|member| member.progress == MemberProgress::Removed)
        || !exists(&record.group_dir)?
    {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&record.group_dir).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    if fs::read_dir(&record.group_dir)
        .map_err(|error| error.to_string())?
        .next()
        .is_some()
    {
        return Ok(());
    }
    fs::remove_dir(&record.group_dir).map_err(|error| error.to_string())
}

fn copy_agents_file(checkout: &PlannedCheckout) -> Result<(), String> {
    let source = checkout.source_workdir.join("AGENTS.md");
    let contents = match fs::read(&source) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Could not read {}: {error}", source.display())),
    };
    let destination = checkout.destination.join("AGENTS.md");
    let mut output = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Could not create {}: {error}",
                destination.display()
            ))
        }
    };
    output
        .write_all(&contents)
        .map_err(|error| format!("Could not copy {}: {error}", destination.display()))
}

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;
