//! Journal-independent, in-process Git application and read-only recovery classification.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use git2::{DiffOptions, ErrorCode, Index, Oid, Repository, RepositoryState};

use super::delivery_git::{self, MergeKind};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ApplyState {
    Completed,
    Unchanged,
    ReadyToFinish,
    NeedsAttention,
}

/// Native delivery deliberately supports only merge semantics that libgit2 can execute
/// without shell hooks, filters, signing helpers, or a sparse working tree.
pub(super) fn support_blockers(
    repo: &Repository,
    source: Oid,
    target: Oid,
) -> Result<Vec<String>, String> {
    let mut blockers = Vec::new();
    let Some(workdir) = repo.workdir() else {
        return Ok(vec!["Delivery requires a working checkout".into()]);
    };
    let mut trees = vec![
        repo.find_commit(source)
            .and_then(|c| c.tree())
            .map_err(git_error)?,
        repo.find_commit(target)
            .and_then(|c| c.tree())
            .map_err(git_error)?,
    ];
    match repo.merge_bases(source, target) {
        Ok(bases) => {
            for base in bases.iter() {
                trees.push(
                    repo.find_commit(*base)
                        .and_then(|c| c.tree())
                        .map_err(git_error)?,
                );
            }
        }
        Err(error) if error.code() == ErrorCode::NotFound => {}
        Err(error) => return Err(git_error(error)),
    }
    if delivery_git::attribute_semantics_warning(repo, &trees.iter().collect::<Vec<_>>())?.is_some()
    {
        blockers.push("Native delivery does not support Git attributes, custom merge drivers, or renormalization".into());
    }
    let mut attribute_dirs = BTreeSet::from([workdir.to_path_buf()]);
    let mut submodules = false;
    for tree in &trees {
        tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            attribute_dirs.insert(workdir.join(root));
            submodules |= entry.filemode() == 0o160000;
            git2::TreeWalkResult::Ok
        })
        .map_err(git_error)?;
    }
    if submodules {
        blockers.push("Native delivery does not support submodule entries".into());
    }
    for directory in attribute_dirs {
        if nonempty_file(&directory.join(".gitattributes"))? {
            blockers
                .push("Native delivery does not support working-directory Git attributes".into());
            break;
        }
    }

    let config = repo.config().map_err(git_error)?;
    // Git resolves a relative hooksPath from the checkout root for these hooks.
    let hooks = match config.get_path("core.hookspath") {
        Ok(path) if path.is_absolute() => path,
        Ok(path) => workdir.join(path),
        Err(error) if error.code() == ErrorCode::NotFound => repo.commondir().join("hooks"),
        Err(error) => return Err(git_error(error)),
    };
    for name in [
        "pre-commit",
        "post-commit",
        "pre-merge-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-merge",
        "post-checkout",
        "reference-transaction",
    ] {
        if executable_file(&hooks.join(name))? {
            blockers.push(format!("Native delivery cannot run the active {name} hook"));
        }
    }
    for name in [
        "commit.gpgsign",
        "merge.gpgsign",
        "merge.verifysignatures",
        "merge.autostash",
        "core.sparsecheckout",
        "core.sparsecheckoutcone",
        "index.sparse",
        "core.splitindex",
        "core.ignorestat",
    ] {
        match config.get_bool(name) {
            Ok(true) => blockers.push(format!("Native delivery does not support {name}")),
            Ok(false) => {}
            Err(error) if error.code() == ErrorCode::NotFound => {}
            Err(error) => blockers.push(format!("Cannot interpret {name}: {error}")),
        }
    }
    let mut entries = config.entries(None).map_err(git_error)?;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(git_error)?;
        let Some(name) = entry.name() else { continue };
        let value = entry.value().unwrap_or_default();
        let unsupported = (name.starts_with("filter.")
            && [".clean", ".smudge", ".process", ".required"]
                .iter()
                .any(|end| name.ends_with(end)))
            || (name.starts_with("branch.")
                && name.ends_with(".mergeoptions")
                && !value.is_empty())
            || match name {
                "core.autocrlf" | "core.fsmonitor" => !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "false" | "no" | "off" | "0"
                ),
                "core.eol" => !value.is_empty(),
                "merge.ff" | "merge.renames" => !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "true" | "yes" | "on" | "1"
                ),
                "merge.renamelimit" => value != "1000",
                "merge.directoryrenames" | "merge.strategy" | "merge.log" => !value.is_empty(),
                _ => false,
            };
        if unsupported {
            blockers.push(format!(
                "Native delivery does not support configured {name}"
            ));
        }
    }
    // The shared preview helper handles absolute/global paths. Resolve relative
    // attribute files against this checkout as Git does, not the app process cwd.
    if let Ok(path) = config.get_path("core.attributesfile") {
        if path.is_relative() && nonempty_file(&workdir.join(path))? {
            blockers.push("Native delivery does not support core.attributesfile".into());
        }
    }
    if unsupported_index(&read_index(repo)?) {
        blockers.push("Native delivery does not support sparse, assume-unchanged, or intent-to-add index entries".into());
    }
    blockers.sort();
    blockers.dedup();
    Ok(blockers)
}

/// Only writes Git objects. The caller journals this OID before checkout begins.
pub(super) fn prepare(repo: &Repository, source: Oid, target: Oid) -> Result<Oid, String> {
    require_supported(repo, source, target)?;
    let comparison = delivery_git::compare(repo, source, target)?;
    match comparison.kind {
        MergeKind::UpToDate => Ok(target),
        MergeKind::FastForward => Ok(source),
        MergeKind::Mergeable => {
            let source = repo.find_commit(source).map_err(git_error)?;
            let target = repo.find_commit(target).map_err(git_error)?;
            let base = Oid::from_str(&comparison.merge_base_oids[0]).map_err(git_error)?;
            let base = repo
                .find_commit(base)
                .and_then(|c| c.tree())
                .map_err(git_error)?;
            let mut options = git2::MergeOptions::new();
            options
                .find_renames(true)
                .target_limit(1_000)
                .no_recursive(true);
            let mut index = repo
                .merge_trees(
                    &base,
                    &target.tree().map_err(git_error)?,
                    &source.tree().map_err(git_error)?,
                    Some(&options),
                )
                .map_err(git_error)?;
            if index.has_conflicts() {
                return Err("The pinned commits have merge conflicts".into());
            }
            let signature = repo.signature().map_err(git_error)?;
            let tree = index.write_tree_to(repo).map_err(git_error)?;
            let tree = repo.find_tree(tree).map_err(git_error)?;
            repo.commit(
                None,
                &signature,
                &signature,
                &format!("Merge managed worktree commit {}", source.id()),
                &tree,
                &[&target, &source],
            )
            .map_err(git_error)
        }
        kind => Err(format!("The pinned commits cannot be delivered: {kind:?}")),
    }
}

pub(super) fn apply(
    repo: &Repository,
    branch: &str,
    target: Oid,
    result: Oid,
) -> Result<(), String> {
    apply_with_checkout(
        repo,
        branch,
        target,
        result,
        git2::build::CheckoutBuilder::new(),
    )
}

fn apply_with_checkout(
    repo: &Repository,
    branch: &str,
    target: Oid,
    result: Oid,
    mut checkout: git2::build::CheckoutBuilder<'_>,
) -> Result<(), String> {
    let reference = branch_reference(branch)?;
    require_supported(repo, result, target)?;
    require_result_descends(repo, result, target)?;
    // Resolve identity before touching the checkout: a missing reflog identity
    // must not leave an otherwise avoidable interrupted application.
    let signature = repo.signature().map_err(git_error)?;
    let mut transaction = repo.transaction().map_err(git_error)?;
    transaction.lock_ref("HEAD").map_err(git_error)?;
    transaction.lock_ref(&reference).map_err(git_error)?;
    let mut index_transaction = IndexTransaction::acquire(repo)?;
    require_checkout(repo, &reference, target, target, &index_transaction)?;
    let commit = repo.find_commit(result).map_err(git_error)?;
    let result_index = index_transaction.prepare(repo, &commit.tree().map_err(git_error)?)?;
    checkout
        .safe()
        .overwrite_ignored(false)
        .remove_untracked(false)
        .remove_ignored(false)
        .update_index(false);
    repo.checkout_tree(commit.as_object(), Some(&mut checkout))
        .map_err(git_error)?;
    // A checkout error or interruption intentionally leaves its state for explicit
    // recovery. Never force-reset files or remove an operator's changes.
    if !workdir_matches(repo, &result_index)? {
        return Err(
            "The working directory changed during delivery; its index and branch were not updated"
                .into(),
        );
    }
    index_transaction.publish(repo)?;
    require_checkout(repo, &reference, target, result, &index_transaction)?;
    transaction
        .set_target(
            &reference,
            result,
            Some(&signature),
            "managed worktree delivery",
        )
        .map_err(git_error)?;
    transaction.commit().map_err(git_error)
}

/// Finish a journaled checkout without re-running checkout or overwriting files.
pub(super) fn finish(
    repo: &Repository,
    branch: &str,
    target: Oid,
    result: Oid,
) -> Result<(), String> {
    let reference = branch_reference(branch)?;
    require_supported(repo, result, target)?;
    require_result_descends(repo, result, target)?;
    let signature = repo.signature().map_err(git_error)?;
    let mut transaction = repo.transaction().map_err(git_error)?;
    transaction.lock_ref("HEAD").map_err(git_error)?;
    transaction.lock_ref(&reference).map_err(git_error)?;
    let index_transaction = IndexTransaction::acquire(repo)?;
    require_checkout(repo, &reference, target, result, &index_transaction)?;
    transaction
        .set_target(
            &reference,
            result,
            Some(&signature),
            "finish managed worktree delivery",
        )
        .map_err(git_error)?;
    transaction.commit().map_err(git_error)
}

/// This path never takes a Git lock, refreshes the disk index, or writes objects.
pub(super) fn classify(
    repo: &Repository,
    branch: &str,
    target: Oid,
    result: Option<Oid>,
) -> Result<ApplyState, String> {
    let reference = branch_reference(branch)?;
    if repo.state() != RepositoryState::Clean
        || has_locks(repo, &reference, None)
        || !support_blockers(repo, result.unwrap_or(target), target)?.is_empty()
    {
        return Ok(ApplyState::NeedsAttention);
    }
    let Some(head) = branch_head(repo, &reference)? else {
        return Ok(ApplyState::NeedsAttention);
    };
    if let Some(result) = result {
        if head == result && checkout_matches(repo, result)? {
            return Ok(ApplyState::Completed);
        }
        if head == target && checkout_matches(repo, result)? {
            return Ok(ApplyState::ReadyToFinish);
        }
    }
    if head == target && checkout_matches(repo, target)? {
        Ok(ApplyState::Unchanged)
    } else {
        Ok(ApplyState::NeedsAttention)
    }
}

fn require_supported(repo: &Repository, source: Oid, target: Oid) -> Result<(), String> {
    let blockers = support_blockers(repo, source, target)?;
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(blockers.join("; "))
    }
}

fn require_result_descends(repo: &Repository, result: Oid, target: Oid) -> Result<(), String> {
    if result == target
        || repo
            .graph_descendant_of(result, target)
            .map_err(git_error)?
    {
        Ok(())
    } else {
        Err("The prepared result does not preserve the pinned target history".into())
    }
}

fn require_checkout(
    repo: &Repository,
    reference: &str,
    head: Oid,
    tree: Oid,
    index_transaction: &IndexTransaction,
) -> Result<(), String> {
    if repo.state() != RepositoryState::Clean || has_locks(repo, reference, Some(index_transaction))
    {
        return Err("The target checkout has a Git operation or lock in progress".into());
    }
    if branch_head(repo, reference)? != Some(head) {
        return Err("The target HEAD or branch moved after delivery was prepared".into());
    }
    if !checkout_matches(repo, tree)? {
        return Err(
            "The complete target index and working directory do not match the expected commit"
                .into(),
        );
    }
    Ok(())
}

fn branch_reference(branch: &str) -> Result<String, String> {
    let reference = format!("refs/heads/{branch}");
    if branch.is_empty() || branch.contains('\0') || !git2::Reference::is_valid_name(&reference) {
        return Err("Delivery requires a valid local branch name".into());
    }
    Ok(reference)
}

fn branch_head(repo: &Repository, reference: &str) -> Result<Option<Oid>, String> {
    let head = repo.find_reference("HEAD").map_err(git_error)?;
    if head.symbolic_target() != Some(reference) {
        return Ok(None);
    }
    match repo.find_reference(reference) {
        Ok(branch) => Ok(branch.target()),
        Err(error) if error.code() == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(git_error(error)),
    }
}

fn has_locks(repo: &Repository, reference: &str, owned: Option<&IndexTransaction>) -> bool {
    let mut paths = vec![
        repo.path().join("locked"),
        repo.commondir().join("packed-refs.lock"),
    ];
    if owned.is_none() {
        paths.extend([
            repo.path().join("index.lock"),
            repo.path().join("HEAD.lock"),
            repo.commondir().join(format!("{reference}.lock")),
            repo.path().join(format!("{reference}.lock")),
        ]);
    }
    paths
        .into_iter()
        .any(|path| path.symlink_metadata().is_ok())
}

/// Git's checkout only locks the index when it finally writes it. Keep our own
/// standard Git lock across validation, checkout, index publication, and ref update.
struct IndexTransaction {
    lock_path: PathBuf,
    lock: Option<File>,
    temporary: Option<PathBuf>,
}

impl IndexTransaction {
    fn acquire(repo: &Repository) -> Result<Self, String> {
        let lock_path = repo.path().join("index.lock");
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| format!("Cannot lock the target index: {error}"))?;
        Ok(Self {
            lock_path,
            lock: Some(lock),
            temporary: None,
        })
    }

    fn prepare(&mut self, repo: &Repository, tree: &git2::Tree<'_>) -> Result<Index, String> {
        let directory = repo
            .path()
            .join(format!(".pi-delivery-index-{}", uuid::Uuid::new_v4()));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder
            .create(&directory)
            .map_err(|error| error.to_string())?;
        self.temporary = Some(directory.clone());
        let path = directory.join("index");
        let mut index = Index::open(&path).map_err(git_error)?;
        index.read_tree(tree).map_err(git_error)?;
        index.write().map_err(git_error)?;
        match fs::metadata(repo.path().join("index")) {
            Ok(metadata) => fs::set_permissions(&path, metadata.permissions())
                .map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(index)
    }

    fn publish(&self, repo: &Repository) -> Result<(), String> {
        let directory = self
            .temporary
            .as_ref()
            .ok_or("The delivery index was not prepared")?;
        // Rename a separate file, retaining index.lock until the ref transaction ends.
        fs::rename(directory.join("index"), repo.path().join("index"))
            .map_err(|error| format!("Cannot publish the delivery index: {error}"))?;
        #[cfg(unix)]
        File::open(repo.path())
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("Cannot synchronize the delivery index: {error}"))?;
        Ok(())
    }
}

impl Drop for IndexTransaction {
    fn drop(&mut self) {
        if let Some(directory) = self.temporary.take() {
            let _ = fs::remove_dir_all(directory);
        }
        drop(self.lock.take());
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn read_index(repo: &Repository) -> Result<Index, String> {
    let mut index = repo.index().map_err(git_error)?;
    index.read(true).map_err(git_error)?;
    Ok(index)
}

fn unsupported_index(index: &Index) -> bool {
    index.iter().any(|entry| {
        entry.flags & git2::IndexEntryFlag::VALID.bits() != 0
            || entry.flags_extended
                & (git2::IndexEntryExtendedFlag::SKIP_WORKTREE
                    | git2::IndexEntryExtendedFlag::INTENT_TO_ADD)
                    .bits()
                != 0
            || entry.mode == 0o040000
            || entry.mode == 0o160000
    })
}

fn checkout_matches(repo: &Repository, oid: Oid) -> Result<bool, String> {
    let tree = repo
        .find_commit(oid)
        .and_then(|c| c.tree())
        .map_err(git_error)?;
    let index = read_index(repo)?;
    if index.has_conflicts() || unsupported_index(&index) {
        return Ok(false);
    }
    let mut options = DiffOptions::new();
    options
        .update_index(false)
        .include_typechange(true)
        .ignore_submodules(false);
    if repo
        .diff_tree_to_index(Some(&tree), Some(&index), Some(&mut options))
        .map_err(git_error)?
        .deltas()
        .len()
        != 0
    {
        return Ok(false);
    }
    workdir_matches(repo, &index)
}

fn workdir_matches(repo: &Repository, index: &Index) -> Result<bool, String> {
    let mut options = DiffOptions::new();
    options
        .update_index(false)
        .include_typechange(true)
        .ignore_submodules(false)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unreadable(true);
    Ok(repo
        .diff_index_to_workdir(Some(index), Some(&mut options))
        .map_err(git_error)?
        .deltas()
        .len()
        == 0)
}

fn nonempty_file(path: &Path) -> Result<bool, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes.iter().any(|byte| !byte.is_ascii_whitespace())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Cannot inspect {}: {error}", path.display())),
    }
}

fn executable_file(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                Ok(metadata.is_file())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Cannot inspect hook {}: {error}", path.display())),
    }
}

fn git_error(error: git2::Error) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "delivery_apply_tests.rs"]
mod tests;
