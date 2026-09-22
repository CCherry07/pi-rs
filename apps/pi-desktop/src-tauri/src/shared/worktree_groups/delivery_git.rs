//! Commit-only delivery analysis. Git merge output is confined to a private memory ODB.

use std::collections::{BTreeSet, HashSet};
use std::fs;

use git2::{
    Delta, DiffFindOptions, DiffOptions, ErrorCode, ObjectType, Odb, Oid, Repository, Sort,
};
use serde::Serialize;

const MAX_COMMITS: usize = 200;
const MAX_FILES: usize = 1_000;
const RENAME_LIMIT: usize = 1_000;
const MAX_MERGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MERGE_OBJECTS: usize = 100_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeComparison {
    pub(crate) source_oid: String,
    pub(crate) target_oid: String,
    pub(crate) merge_base_oids: Vec<String>,
    pub(crate) ahead: usize,
    pub(crate) behind: usize,
    pub(crate) kind: MergeKind,
    pub(crate) commits: Vec<DeliveryCommit>,
    pub(crate) commits_truncated: bool,
    pub(crate) files: Vec<DeliveryFile>,
    pub(crate) files_truncated: bool,
    pub(crate) conflicts: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MergeKind {
    UpToDate,
    FastForward,
    Mergeable,
    Conflicts,
    Unrelated,
    Unsupported,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryCommit {
    pub(crate) oid: String,
    pub(crate) summary: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryFile {
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) old_path: Option<String>,
}

pub(super) fn compare(
    repo: &Repository,
    source: Oid,
    target: Oid,
) -> Result<MergeComparison, String> {
    compare_bounded(
        repo,
        source,
        target,
        MAX_COMMITS,
        MAX_FILES,
        MAX_MERGE_BYTES,
    )
}

fn compare_bounded(
    repo: &Repository,
    source: Oid,
    target: Oid,
    commit_limit: usize,
    file_limit: usize,
    memory_limit: usize,
) -> Result<MergeComparison, String> {
    let source_commit = repo.find_commit(source).map_err(git_error)?;
    let target_commit = repo.find_commit(target).map_err(git_error)?;
    let (ahead, behind) = repo.graph_ahead_behind(source, target).map_err(git_error)?;
    let bases: Vec<_> = match repo.merge_bases(source, target) {
        Ok(bases) => bases.iter().copied().collect(),
        Err(error) if error.code() == ErrorCode::NotFound => Vec::new(),
        Err(error) => return Err(git_error(error)),
    };
    let mut comparison = MergeComparison {
        source_oid: source.to_string(), target_oid: target.to_string(),
        merge_base_oids: bases.iter().map(ToString::to_string).collect(), ahead, behind,
        kind: MergeKind::Unsupported, commits: Vec::new(), commits_truncated: false,
        files: Vec::new(), files_truncated: false, conflicts: Vec::new(),
        warnings: vec!["Advisory preview only; execution must revalidate both commit IDs and repository state.".into()],
    };
    let mut walk = repo.revwalk().map_err(git_error)?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(git_error)?;
    walk.push(source).map_err(git_error)?;
    walk.hide(target).map_err(git_error)?;
    for oid in walk.take(commit_limit.saturating_add(1)) {
        let oid = oid.map_err(git_error)?;
        if comparison.commits.len() == commit_limit {
            comparison.commits_truncated = true;
            break;
        }
        let commit = repo.find_commit(oid).map_err(git_error)?;
        comparison.commits.push(DeliveryCommit {
            oid: oid.to_string(),
            summary: String::from_utf8_lossy(commit.summary_bytes().unwrap_or_default())
                .into_owned(),
        });
    }
    if repo.is_shallow() && ahead > 0 && behind > 0 {
        comparison.warnings.push("Shallow history may omit common ancestors and incoming commits; counts cover available history and mergeability is not classified.".into());
        return Ok(comparison);
    }
    if bases.is_empty() {
        comparison.kind = MergeKind::Unrelated;
        comparison.warnings.push("The commits have no merge base; no merge-base file comparison or merge result is available.".into());
        return Ok(comparison);
    }
    if bases.len() != 1 {
        comparison.warnings.push("Multiple merge bases require recursive or criss-cross merge semantics; this preview does not choose an arbitrary base.".into());
        return Ok(comparison);
    }
    let base_tree = repo
        .find_commit(bases[0])
        .and_then(|commit| commit.tree())
        .map_err(git_error)?;
    let source_tree = source_commit.tree().map_err(git_error)?;
    let target_tree = target_commit.tree().map_err(git_error)?;
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    let mut incoming = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&source_tree), Some(&mut options))
        .map_err(git_error)?;
    let incoming_limited = rename_candidates_exceed_limit(&incoming);
    let mut find = DiffFindOptions::new();
    find.renames(true).rename_limit(RENAME_LIMIT);
    incoming.find_similar(Some(&mut find)).map_err(git_error)?;
    comparison.files_truncated = incoming.deltas().len() > file_limit;
    for delta in incoming.deltas().take(file_limit) {
        let old = delta.old_file();
        let new = delta.new_file();
        let old_path = old.path_bytes();
        let new_path = new.path_bytes();
        let path = if delta.status() == Delta::Deleted {
            old_path
        } else {
            new_path
        }
        .unwrap_or_default();
        comparison.files.push(DeliveryFile {
            path: display_path(path, &mut comparison.warnings),
            status: delta_status(delta.status()).into(),
            old_path: if matches!(delta.status(), Delta::Renamed | Delta::Copied) {
                old_path.map(|path| display_path(path, &mut comparison.warnings))
            } else {
                None
            },
        });
    }
    let attribute_warning =
        attribute_semantics_warning(repo, &[&base_tree, &source_tree, &target_tree])?;
    if let Some(warning) = attribute_warning.as_ref() {
        comparison.warnings.push(warning.clone());
    }
    if incoming_limited {
        comparison.warnings.push("The incoming change set exceeds the rename-detection limit; the file list may show additions and deletions separately.".into());
    }
    if source == target || ahead == 0 {
        comparison.kind = MergeKind::UpToDate;
        return Ok(comparison);
    }
    if behind == 0 {
        comparison.kind = MergeKind::FastForward;
        return Ok(comparison);
    }
    let outgoing = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&target_tree), None)
        .map_err(git_error)?;
    if incoming_limited || rename_candidates_exceed_limit(&outgoing) {
        comparison.warnings.push("The divergent merge exceeds the rename-detection limit; mergeability is not classified.".into());
        return Ok(comparison);
    }
    if attribute_warning.is_some() {
        return Ok(comparison);
    }
    let changed_blobs = changed_blob_ids(&incoming)
        .chain(changed_blob_ids(&outgoing))
        .collect::<Vec<_>>();
    let memory_repo = match memory_repository(
        repo,
        &[base_tree.id(), target_tree.id(), source_tree.id()],
        &changed_blobs,
        memory_limit,
    )? {
        Some(repo) => repo,
        None => {
            comparison.warnings.push(format!("The changed blobs and tree metadata exceed the preview memory budget ({memory_limit} bytes or {MAX_MERGE_OBJECTS} objects); mergeability is not classified."));
            return Ok(comparison);
        }
    };
    let base = memory_repo.find_tree(base_tree.id()).map_err(git_error)?;
    let ours = memory_repo.find_tree(target_tree.id()).map_err(git_error)?;
    let theirs = memory_repo.find_tree(source_tree.id()).map_err(git_error)?;
    let mut merge_options = git2::MergeOptions::new();
    merge_options
        .find_renames(true)
        .target_limit(RENAME_LIMIT as u32)
        .no_recursive(true);
    // libgit2 can write merged blobs while producing an in-memory index. The detached
    // repository has only a memory backend, so those writes cannot reach real objects.
    let index = memory_repo
        .merge_trees(&base, &ours, &theirs, Some(&merge_options))
        .map_err(git_error)?;
    let mut conflicts = BTreeSet::new();
    for conflict in index.conflicts().map_err(git_error)? {
        let conflict = conflict.map_err(git_error)?;
        for entry in [conflict.ancestor, conflict.our, conflict.their]
            .into_iter()
            .flatten()
        {
            conflicts.insert(display_path(&entry.path, &mut comparison.warnings));
        }
    }
    comparison.conflicts = conflicts.into_iter().collect();
    comparison.kind = if index.has_conflicts() {
        MergeKind::Conflicts
    } else {
        MergeKind::Mergeable
    };
    Ok(comparison)
}

fn memory_repository(
    repo: &Repository,
    trees: &[Oid],
    blobs: &[Oid],
    limit: usize,
) -> Result<Option<Repository>, String> {
    let source = repo.odb().map_err(git_error)?;
    let memory = Odb::new().map_err(git_error)?;
    memory.add_new_mempack_backend(1000).map_err(git_error)?;
    let mut seen = HashSet::new();
    let mut pending = trees.to_vec();
    pending.extend_from_slice(blobs);
    let mut bytes = 0_usize;
    while let Some(oid) = pending.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let (size, kind) = source.read_header(oid).map_err(git_error)?;
        bytes = match bytes.checked_add(size) {
            Some(bytes) if bytes <= limit && seen.len() <= MAX_MERGE_OBJECTS => bytes,
            _ => return Ok(None),
        };
        let object = source.read(oid).map_err(git_error)?;
        memory.write(kind, object.data()).map_err(git_error)?;
        if kind == ObjectType::Tree {
            let tree = repo.find_tree(oid).map_err(git_error)?;
            pending.extend(
                tree.iter()
                    .filter(|entry| entry.kind() == Some(ObjectType::Tree))
                    .map(|entry| entry.id()),
            );
        }
    }
    Repository::from_odb(memory).map(Some).map_err(git_error)
}

fn changed_blob_ids<'a>(diff: &'a git2::Diff<'a>) -> impl Iterator<Item = Oid> + 'a {
    diff.deltas().flat_map(|delta| {
        [delta.old_file(), delta.new_file()]
            .into_iter()
            .filter(|file| {
                !file.id().is_zero()
                    && matches!(
                        file.mode(),
                        git2::FileMode::Blob
                            | git2::FileMode::BlobGroupWritable
                            | git2::FileMode::BlobExecutable
                            | git2::FileMode::Link
                    )
            })
            .map(|file| file.id())
    })
}

fn rename_candidates_exceed_limit(diff: &git2::Diff<'_>) -> bool {
    let added = diff
        .deltas()
        .filter(|delta| delta.status() == Delta::Added)
        .count();
    let deleted = diff
        .deltas()
        .filter(|delta| delta.status() == Delta::Deleted)
        .count();
    added > 0 && deleted > 0 && (added > RENAME_LIMIT || deleted > RENAME_LIMIT)
}

pub(super) fn attribute_semantics_warning(
    repo: &Repository,
    trees: &[&git2::Tree<'_>],
) -> Result<Option<String>, String> {
    let mut attributes = false;
    for tree in trees {
        tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
            if entry.name_bytes() == b".gitattributes" {
                attributes = true;
            }
            git2::TreeWalkResult::Ok
        })
        .map_err(git_error)?;
    }
    let config = repo.config().map_err(git_error)?;
    let mut entries = config.entries(None).map_err(git_error)?;
    let mut custom = false;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(git_error)?;
        if let Some(name) = entry.name() {
            custom |= name.starts_with("merge.")
                && (name.ends_with(".driver")
                    || name.ends_with(".recursive")
                    || name == "merge.default");
        }
    }
    match config.get_bool("merge.renormalize") {
        Ok(value) => custom |= value,
        Err(error) if error.code() == ErrorCode::NotFound => {}
        Err(error) => {
            return Ok(Some(format!(
                "Cannot interpret merge.renormalize: {error}; mergeability is not classified."
            )))
        }
    }
    let mut paths = vec![
        repo.commondir().join("info/attributes"),
        repo.path().join("info/attributes"),
    ];
    match config.get_path("core.attributesfile") {
        Ok(path) => paths.push(path),
        Err(error) if error.code() == ErrorCode::NotFound => {
            let configuration = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| crate::agent_paths::home_dir().map(|path| path.join(".config")));
            if let Some(configuration) = configuration {
                paths.push(configuration.join("git/attributes"));
            }
        }
        Err(error) => {
            return Ok(Some(format!(
                "Cannot resolve core.attributesfile: {error}; mergeability is not classified."
            )))
        }
    }
    if std::env::var_os("GIT_ATTR_NOSYSTEM").is_none() {
        if let Ok(config) = git2::Config::find_system() {
            if let Some(parent) = config.parent() {
                paths.push(parent.join("gitattributes"));
            }
        }
    }
    for path in paths {
        match fs::read(&path) {
            Ok(bytes) => attributes |= bytes.iter().any(|byte| !byte.is_ascii_whitespace()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Ok(Some(format!(
                    "Cannot inspect attributes at {}: {error}; mergeability is not classified.",
                    path.display()
                )))
            }
        }
    }
    Ok((attributes || custom).then(|| "Git attributes, custom merge drivers, and merge renormalization are not evaluated by this preview. Divergent merges using these settings are left unclassified.".into()))
}

fn display_path(bytes: &[u8], warnings: &mut Vec<String>) -> String {
    match std::str::from_utf8(bytes) {
        Ok(path) => path.into(),
        Err(_) => {
            let warning = "Some paths are not UTF-8 and are displayed with replacement characters.";
            if !warnings.iter().any(|item| item == warning) {
                warnings.push(warning.into());
            }
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

fn delta_status(status: Delta) -> &'static str {
    match status {
        Delta::Unmodified => "unmodified",
        Delta::Added => "added",
        Delta::Deleted => "deleted",
        Delta::Modified => "modified",
        Delta::Renamed => "renamed",
        Delta::Copied => "copied",
        Delta::Ignored => "ignored",
        Delta::Untracked => "untracked",
        Delta::Typechange => "typeChanged",
        Delta::Unreadable => "unreadable",
        Delta::Conflicted => "conflicted",
    }
}

fn git_error(error: git2::Error) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "delivery_git_tests.rs"]
mod tests;
