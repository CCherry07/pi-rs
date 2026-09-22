//! Durable, checkout-scoped delivery. Recovery observes effects and never replays a merge.
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::path::Path;

use chrono::Utc;
use fs2::FileExt;
use git2::{Oid, Repository};
use pi_core::WorkspaceSpec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::delivery::{managed_repository, origin_repository, preview_record, ready_record};
use super::delivery_apply::{self, ApplyState};
use super::delivery_git::MergeKind;
use super::{journal_dir, load, lock, save, GroupLock, Member, PlannedCheckout, Record};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryExecutionRequest {
    pub attempt_id: String,
    pub checkout_key: String,
    pub target_branch: String,
    pub source_oid: String,
    pub target_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DeliveryAttemptStatus {
    Preparing,
    Applying,
    Completed,
    Unchanged,
    ReadyToFinish,
    NeedsAttention,
}

impl DeliveryAttemptStatus {
    fn unresolved(&self) -> bool {
        !matches!(self, Self::Completed | Self::Unchanged)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryAttempt {
    pub id: String,
    pub checkout_key: String,
    pub source_oid: String,
    pub target_oid: String,
    pub target_branch: String,
    pub result_oid: Option<String>,
    pub status: DeliveryAttemptStatus,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
}

pub(crate) fn execute(
    storage: &Path,
    id: &str,
    workspace: &WorkspaceSpec,
    request: DeliveryExecutionRequest,
) -> Result<DeliveryAttempt, String> {
    validate_request(&request)?;
    let _group_lock = lock(storage, id)?;
    let mut record = ready_record(storage, id)?;
    let (checkout, member) = selected_member(&record, &request.checkout_key)?;
    managed_repository(workspace, checkout, member)?;
    let _target_lock = target_lock(storage, &member.common_dir)?;
    let origin = origin_repository(checkout, member)?;

    if let Some(index) = record
        .deliveries
        .iter()
        .position(|item| item.id == request.attempt_id)
    {
        let previous = &record.deliveries[index];
        if previous.checkout_key != request.checkout_key
            || previous.source_oid != request.source_oid
            || previous.target_oid != request.target_oid
            || previous.target_branch != request.target_branch
        {
            return Err("This delivery attempt ID belongs to a different merge".into());
        }
        return observe_and_save(storage, &mut record, index, None);
    }
    ensure_target_available(storage, &record, &member.common_dir, None)?;
    let preview = preview_record(
        &record,
        id,
        workspace,
        &request.checkout_key,
        &request.target_branch,
    )?;
    if preview.source.oid != request.source_oid || preview.target.oid != request.target_oid {
        return Err("The source or target commit changed; create a new merge preview".into());
    }
    if !preview.blockers.is_empty() {
        return Err(preview.blockers.join("\n"));
    }
    if !matches!(
        preview.comparison.kind,
        MergeKind::FastForward | MergeKind::Mergeable
    ) {
        return Err(
            "Only a supported, conflict-free merge with incoming commits can be executed".into(),
        );
    }

    let now = Utc::now().to_rfc3339();
    record.deliveries.push(DeliveryAttempt {
        id: request.attempt_id,
        checkout_key: request.checkout_key,
        source_oid: request.source_oid,
        target_oid: request.target_oid,
        target_branch: request.target_branch,
        result_oid: None,
        status: DeliveryAttemptStatus::Preparing,
        created_at: now.clone(),
        updated_at: now,
        error: None,
    });
    // Older versions must reject a journal whose resource lifetime includes delivery recovery.
    record.version = 2;
    save(storage, &record)?;
    let index = record.deliveries.len() - 1;
    let attempt = &record.deliveries[index];
    let source = oid(&attempt.source_oid)?;
    let target = oid(&attempt.target_oid)?;
    let result = match delivery_apply::prepare(&origin, source, target) {
        Ok(result) => result,
        Err(error) => return observe_and_save(storage, &mut record, index, Some(error)),
    };
    record.deliveries[index].result_oid = Some(result.to_string());
    record.deliveries[index].status = DeliveryAttemptStatus::Applying;
    record.deliveries[index].updated_at = Utc::now().to_rfc3339();
    // This result identifies exactly what may be written, even if checkout or final save fails.
    save(storage, &record)?;
    let error = (|| {
        let attempt = &record.deliveries[index];
        let (checkout, member) = selected_member(&record, &attempt.checkout_key)?;
        let source_repo = managed_repository(workspace, checkout, member)?;
        if source_repo
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(|error| error.to_string())?
            .id()
            != source
        {
            return Err(
                "The source commit changed during preparation; create a new merge preview".into(),
            );
        }
        let origin = origin_repository(checkout, member)?;
        delivery_apply::apply(&origin, &attempt.target_branch, target, result)
    })()
    .err();
    observe_and_save(storage, &mut record, index, error)
}

pub(crate) fn inspect(
    storage: &Path,
    id: &str,
    workspace: &WorkspaceSpec,
    attempt_id: &str,
) -> Result<DeliveryAttempt, String> {
    recover(storage, id, workspace, attempt_id, false)
}

pub(crate) fn finish(
    storage: &Path,
    id: &str,
    workspace: &WorkspaceSpec,
    attempt_id: &str,
) -> Result<DeliveryAttempt, String> {
    recover(storage, id, workspace, attempt_id, true)
}

fn recover(
    storage: &Path,
    id: &str,
    workspace: &WorkspaceSpec,
    attempt_id: &str,
    finish: bool,
) -> Result<DeliveryAttempt, String> {
    let _group_lock = lock(storage, id)?;
    let mut record = ready_record(storage, id)?;
    let index = record
        .deliveries
        .iter()
        .position(|attempt| attempt.id == attempt_id)
        .ok_or("Unknown delivery attempt")?;
    let attempt = record.deliveries[index].clone();
    let (checkout, member) = selected_member(&record, &attempt.checkout_key)?;
    managed_repository(workspace, checkout, member)?;
    let _target_lock = target_lock(storage, &member.common_dir)?;
    let origin = origin_repository(checkout, member)?;
    if !finish || !attempt.status.unresolved() {
        return observe_and_save(storage, &mut record, index, None);
    }
    ensure_target_available(storage, &record, &member.common_dir, Some(attempt_id))?;
    let target = oid(&attempt.target_oid)?;
    let result = attempt
        .result_oid
        .as_deref()
        .map(oid)
        .transpose()?
        .ok_or("This attempt has no prepared merge result; inspect its state")?;
    validate_result(&origin, &attempt)?;
    let blockers = delivery_apply::support_blockers(&origin, oid(&attempt.source_oid)?, target)?;
    if !blockers.is_empty() {
        return Err(blockers.join("\n"));
    }
    if !matches!(
        delivery_apply::classify(&origin, &attempt.target_branch, target, Some(result))?,
        ApplyState::ReadyToFinish
    ) {
        return Err(
            "The original checkout is not ready to complete this merge; inspect its state again"
                .into(),
        );
    }
    record.deliveries[index].status = DeliveryAttemptStatus::Applying;
    record.deliveries[index].updated_at = Utc::now().to_rfc3339();
    save(storage, &record)?;
    let error = (|| {
        let (checkout, member) = selected_member(&record, &attempt.checkout_key)?;
        managed_repository(workspace, checkout, member)?;
        let origin = origin_repository(checkout, member)?;
        delivery_apply::finish(&origin, &attempt.target_branch, target, result)
    })()
    .err();
    observe_and_save(storage, &mut record, index, error)
}

fn observe_and_save(
    storage: &Path,
    record: &mut Record,
    index: usize,
    execution_error: Option<String>,
) -> Result<DeliveryAttempt, String> {
    let attempt = &record.deliveries[index];
    if !attempt.status.unresolved() {
        return Ok(attempt.clone());
    }
    let observation = (|| {
        let (checkout, member) = selected_member(record, &attempt.checkout_key)?;
        let origin = origin_repository(checkout, member)?;
        validate_result(&origin, attempt)?;
        delivery_apply::classify(
            &origin,
            &attempt.target_branch,
            oid(&attempt.target_oid)?,
            attempt.result_oid.as_deref().map(oid).transpose()?,
        )
    })();
    let (status, error) = match observation {
        Ok(ApplyState::Completed) => (DeliveryAttemptStatus::Completed, execution_error),
        Ok(ApplyState::Unchanged) => (DeliveryAttemptStatus::Unchanged, execution_error.or_else(|| attempt.error.clone())),
        Ok(ApplyState::ReadyToFinish) => (DeliveryAttemptStatus::ReadyToFinish, execution_error.or_else(|| Some("The planned files are present, but the target branch has not been updated. Complete this attempt explicitly.".into()))),
        Ok(ApplyState::NeedsAttention) => (DeliveryAttemptStatus::NeedsAttention, execution_error.or_else(|| Some("The original checkout differs from the saved merge. Inspect its files, branch, and Git locks; no changes were discarded.".into()))),
        Err(error) => (DeliveryAttemptStatus::NeedsAttention, Some(match execution_error {
            Some(execution_error) => format!("{execution_error}\n{error}"),
            None => error,
        })),
    };
    let attempt = &mut record.deliveries[index];
    attempt.status = status;
    attempt.error = error;
    attempt.updated_at = Utc::now().to_rfc3339();
    let result = attempt.clone();
    save(storage, record).map_err(|error| format!(
        "Could not save delivery result: {error}. Inspect attempt {} before starting another merge.", result.id
    ))?;
    Ok(result)
}

fn selected_member<'a>(
    record: &'a Record,
    key: &str,
) -> Result<(&'a PlannedCheckout, &'a Member), String> {
    record
        .plan
        .checkouts
        .iter()
        .zip(&record.members)
        .find(|(_, member)| member.expected_git_dir.to_string_lossy() == key)
        .ok_or_else(|| "Selected checkout is not a member of this managed worktree group".into())
}

fn oid(value: &str) -> Result<Oid, String> {
    let oid = Oid::from_str(value).map_err(|error| error.to_string())?;
    if oid.to_string() != value {
        return Err("A full, canonical commit ID is required".into());
    }
    Ok(oid)
}

fn validate_request(request: &DeliveryExecutionRequest) -> Result<(), String> {
    let id = Uuid::parse_str(&request.attempt_id).map_err(|error| error.to_string())?;
    if id.to_string() != request.attempt_id
        || request.checkout_key.is_empty()
        || request.target_branch.is_empty()
        || request.target_branch.contains('\0')
        || request.target_branch.trim() != request.target_branch
        || !git2::Reference::is_valid_name(&format!("refs/heads/{}", request.target_branch))
    {
        return Err("Invalid delivery attempt identity or target branch".into());
    }
    oid(&request.source_oid)?;
    oid(&request.target_oid)?;
    Ok(())
}

fn validate_result(repo: &Repository, attempt: &DeliveryAttempt) -> Result<(), String> {
    let Some(result) = attempt.result_oid.as_deref() else {
        return Ok(());
    };
    let source = oid(&attempt.source_oid)?;
    let target = oid(&attempt.target_oid)?;
    let result = oid(result)?;
    let commit = repo
        .find_commit(result)
        .map_err(|error| error.to_string())?;
    if result == source
        && repo
            .graph_descendant_of(source, target)
            .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    if commit.parent_count() == 2
        && commit.parent_id(0).ok() == Some(target)
        && commit.parent_id(1).ok() == Some(source)
    {
        return Ok(());
    }
    Err("The saved merge result does not match its source and target commits".into())
}

pub(super) fn validate_records(record: &Record) -> Result<(), String> {
    if record.version == 1 && !record.deliveries.is_empty() {
        return Err("Delivery recovery requires a version 2 managed worktree record".into());
    }
    let mut ids = HashSet::new();
    for attempt in &record.deliveries {
        if !ids.insert(&attempt.id) {
            return Err("Duplicate delivery attempt ID".into());
        }
        validate_request(&DeliveryExecutionRequest {
            attempt_id: attempt.id.clone(),
            checkout_key: attempt.checkout_key.clone(),
            target_branch: attempt.target_branch.clone(),
            source_oid: attempt.source_oid.clone(),
            target_oid: attempt.target_oid.clone(),
        })?;
        selected_member(record, &attempt.checkout_key)?;
        if let Some(result) = &attempt.result_oid {
            oid(result)?;
        }
        if matches!(
            attempt.status,
            DeliveryAttemptStatus::Applying
                | DeliveryAttemptStatus::Completed
                | DeliveryAttemptStatus::ReadyToFinish
        ) && attempt.result_oid.is_none()
        {
            return Err("Delivery progress is missing its prepared merge result".into());
        }
    }
    Ok(())
}

pub(super) fn ensure_cleanup_allowed(record: &Record) -> Result<(), String> {
    if record
        .deliveries
        .iter()
        .any(|attempt| attempt.status.unresolved())
    {
        return Err("Inspect and resolve pending worktree delivery attempts before cleanup; force discard cannot bypass delivery recovery".into());
    }
    Ok(())
}

pub(crate) fn lock_for_detach(storage: &Path, id: &str) -> Result<Option<GroupLock>, String> {
    if !super::has_record(storage, id)? {
        return Ok(None);
    }
    let guard = lock(storage, id)?;
    ensure_cleanup_allowed(&load(storage, id)?)?;
    Ok(Some(guard))
}

pub(super) fn lock_for_cleanup(storage: &Path, record: &Record) -> Result<Vec<GroupLock>, String> {
    ensure_cleanup_allowed(record)?;
    let common_dirs: std::collections::BTreeSet<_> = record
        .members
        .iter()
        .map(|member| &member.common_dir)
        .collect();
    let mut guards = Vec::new();
    for common in common_dirs {
        guards.push(target_lock(storage, common)?);
        // Another group may deliver back into one of the worktrees this group owns.
        // Keep the repository lock through cleanup so no new attempt can begin meanwhile.
        ensure_target_available(storage, record, common, None)?;
    }
    Ok(guards)
}

fn target_lock(storage: &Path, common: &Path) -> Result<GroupLock, String> {
    let directory = journal_dir(storage).join("delivery-locks");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let key = format!(
        "{:x}",
        Sha256::digest(common.as_os_str().as_encoded_bytes())
    );
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{key}.lock")))
        .map_err(|error| error.to_string())?;
    file.try_lock_exclusive()
        .map_err(|_| "Another delivery operation is using this Git repository")?;
    Ok(GroupLock(file))
}

fn ensure_target_available(
    storage: &Path,
    current: &Record,
    common: &Path,
    except_attempt: Option<&str>,
) -> Result<(), String> {
    for entry in fs::read_dir(journal_dir(storage)).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|part| part.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|part| part.to_str())
            .ok_or("Invalid worktree journal name")?;
        let other;
        let record = if id == current.plan.id {
            current
        } else {
            other = load(storage, id)?;
            &other
        };
        for attempt in &record.deliveries {
            if !attempt.status.unresolved()
                || (id == current.plan.id && Some(attempt.id.as_str()) == except_attempt)
            {
                continue;
            }
            let (_, member) = selected_member(record, &attempt.checkout_key)?;
            if member.common_dir == common {
                return Err(format!("Inspect unresolved delivery {} in managed worktree {} before operating on this repository", attempt.id, id));
            }
        }
    }
    Ok(())
}
