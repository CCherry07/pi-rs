use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{EvalError, WorkspaceChange, WorkspaceChangeKind};

const MAX_CAPTURED_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileSnapshot {
    sha256: String,
    text: Option<String>,
}

pub(crate) type WorkspaceSnapshot = BTreeMap<String, FileSnapshot>;

pub(crate) fn snapshot_workspace(root: &Path) -> Result<WorkspaceSnapshot, EvalError> {
    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot)?;
    Ok(snapshot)
}

fn visit(root: &Path, directory: &Path, snapshot: &mut WorkspaceSnapshot) -> Result<(), EvalError> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        EvalError::Fixture(format!("cannot read {}: {error}", directory.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| EvalError::Fixture(error.to_string()))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| EvalError::Fixture(error.to_string()))?;
        if relative
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == ".git")
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            EvalError::Fixture(format!("cannot inspect {}: {error}", path.display()))
        })?;
        if metadata.is_dir() {
            if ignored_generated_directory(&path) {
                continue;
            }
            visit(root, &path, snapshot)?;
        } else if metadata.is_file() {
            let bytes = std::fs::read(&path).map_err(|error| {
                EvalError::Fixture(format!("cannot read {}: {error}", path.display()))
            })?;
            snapshot.insert(normalize_relative(relative), file_snapshot(&bytes));
        } else if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&path).map_err(|error| {
                EvalError::Fixture(format!("cannot read symlink {}: {error}", path.display()))
            })?;
            let bytes = format!("symlink:{}", target.display()).into_bytes();
            snapshot.insert(normalize_relative(relative), file_snapshot(&bytes));
        }
    }
    Ok(())
}

fn file_snapshot(bytes: &[u8]) -> FileSnapshot {
    let text = (bytes.len() <= MAX_CAPTURED_TEXT_BYTES)
        .then(|| std::str::from_utf8(bytes).ok().map(str::to_string))
        .flatten();
    FileSnapshot {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        text,
    }
}

pub(crate) fn changes(
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
) -> Vec<WorkspaceChange> {
    let paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    paths
        .into_iter()
        .filter_map(|path| {
            let before = before.get(&path);
            let after = after.get(&path);
            if before == after {
                return None;
            }
            Some(WorkspaceChange {
                path,
                kind: match (before, after) {
                    (None, Some(_)) => WorkspaceChangeKind::Added,
                    (Some(_), None) => WorkspaceChangeKind::Removed,
                    (Some(_), Some(_)) => WorkspaceChangeKind::Modified,
                    (None, None) => unreachable!(),
                },
                before_sha256: before.map(|file| file.sha256.clone()),
                after_sha256: after.map(|file| file.sha256.clone()),
                before_text: before.and_then(|file| file.text.clone()),
                after_text: after.and_then(|file| file.text.clone()),
            })
        })
        .collect()
}

pub(crate) fn copy_fixture(source: &Path, destination: &Path) -> Result<(), EvalError> {
    if !source.is_dir() {
        return Err(EvalError::Fixture(format!(
            "fixture directory does not exist: {}",
            source.display()
        )));
    }
    copy_directory(source, destination)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), EvalError> {
    std::fs::create_dir_all(destination).map_err(|error| {
        EvalError::Fixture(format!("cannot create {}: {error}", destination.display()))
    })?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| EvalError::Fixture(format!("cannot read {}: {error}", source.display())))?
    {
        let entry = entry.map_err(|error| EvalError::Fixture(error.to_string()))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from).map_err(|error| {
            EvalError::Fixture(format!("cannot inspect {}: {error}", from.display()))
        })?;
        if metadata.is_dir() {
            if ignored_generated_directory(&from) {
                continue;
            }
            copy_directory(&from, &to)?;
        } else if metadata.is_file() {
            std::fs::copy(&from, &to).map_err(|error| {
                EvalError::Fixture(format!(
                    "cannot copy {} to {}: {error}",
                    from.display(),
                    to.display()
                ))
            })?;
        } else {
            return Err(EvalError::Fixture(format!(
                "fixture contains unsupported symlink or special file: {}",
                from.display()
            )));
        }
    }
    Ok(())
}

fn ignored_generated_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "node_modules" | "target"))
}

fn normalize_relative(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn copy_bootstrap_file(
    source_directory: &Path,
    destination_directory: &Path,
    name: &str,
) -> Result<(), EvalError> {
    let source = source_directory.join(name);
    if !source.exists() {
        return Ok(());
    }
    let destination = destination_directory.join(name);
    std::fs::copy(&source, &destination).map_err(|error| {
        EvalError::Fixture(format!(
            "cannot copy bootstrap file {} to {}: {error}",
            source.display(),
            destination.display()
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_added_modified_and_removed_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("same"), "same").unwrap();
        std::fs::write(directory.path().join("changed"), "before").unwrap();
        std::fs::write(directory.path().join("removed"), "gone").unwrap();
        let before = snapshot_workspace(directory.path()).unwrap();
        std::fs::write(directory.path().join("changed"), "after").unwrap();
        std::fs::remove_file(directory.path().join("removed")).unwrap();
        std::fs::write(directory.path().join("added"), "new").unwrap();
        let after = snapshot_workspace(directory.path()).unwrap();
        let changes = changes(&before, &after);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].kind, WorkspaceChangeKind::Added);
        assert_eq!(changes[1].kind, WorkspaceChangeKind::Modified);
        assert_eq!(changes[2].kind, WorkspaceChangeKind::Removed);
    }

    #[test]
    fn snapshot_ignores_generated_dependency_and_build_directories() {
        let directory = tempfile::tempdir().unwrap();
        for name in [".git", "node_modules", "target"] {
            std::fs::create_dir_all(directory.path().join(name)).unwrap();
            std::fs::write(directory.path().join(name).join("generated"), "large").unwrap();
        }
        std::fs::write(directory.path().join("source.rs"), "source").unwrap();
        let snapshot = snapshot_workspace(directory.path()).unwrap();
        assert_eq!(snapshot.keys().collect::<Vec<_>>(), ["source.rs"]);
    }
}
