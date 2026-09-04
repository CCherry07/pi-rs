//! Project identity and legacy project-memory migration.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{ENTRY_DELIMITER, MEMORY_FILE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectInfo {
    pub(crate) name: Option<String>,
    pub(crate) root: Option<PathBuf>,
}

/// Resolve the active checkout that may own trusted repository-local skills.
///
/// Like Hermes, only an enclosing Git checkout creates project scope. The
/// checkout itself remains the root for worktrees; project state is never
/// redirected into the user's profile directory.
pub(crate) fn detect_project(cwd: &Path) -> ProjectInfo {
    let resolved = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|path| fs::canonicalize(&path).ok().or(Some(path)));
    if resolved.parent().is_none() || home.as_ref().is_some_and(|home| *home == resolved) {
        return ProjectInfo {
            name: None,
            root: None,
        };
    }
    let Some(root) = find_git_repo_root(&resolved) else {
        return ProjectInfo {
            name: None,
            root: None,
        };
    };
    let Some(name) = root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    else {
        return ProjectInfo {
            name: None,
            root: None,
        };
    };
    ProjectInfo {
        name: Some(name.to_string()),
        root: Some(root),
    }
}

pub(crate) fn find_git_repo_root(start: &Path) -> Option<PathBuf> {
    for current in start.ancestors() {
        let dot_git = current.join(".git");
        if dot_git.is_dir() || dot_git.is_file() {
            return Some(current.to_path_buf());
        }
    }
    None
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ProjectMigrationResult {
    pub(crate) scanned: usize,
    pub(crate) copied: usize,
    pub(crate) merged: usize,
    pub(crate) skipped: usize,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn migrate_legacy_project_memory_dirs(
    agent_dir: &Path,
    projects_dir: &str,
) -> ProjectMigrationResult {
    let mut result = ProjectMigrationResult::default();
    let Ok(children) = fs::read_dir(agent_dir) else {
        return result;
    };
    let target_root = agent_dir.join(projects_dir);
    for child in children.flatten() {
        let name = child.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "memory" | "pi-hermes-memory" | "skills")
            || name == projects_dir
            || name.starts_with('.')
            || !child.path().is_dir()
        {
            continue;
        }
        let source = child.path().join(MEMORY_FILE);
        if !source.is_file() {
            continue;
        }
        result.scanned += 1;
        let target = target_root.join(&name).join(MEMORY_FILE);
        let operation = (|| -> std::io::Result<&'static str> {
            let source_entries = read_entries(&source)?;
            if source_entries.is_empty() {
                return Ok("skip");
            }
            if !target.exists() {
                write_entries(&target, &source_entries)?;
                return Ok("copy");
            }
            let target_entries = read_entries(&target)?;
            let mut merged = target_entries.clone();
            let mut seen = target_entries.into_iter().collect::<BTreeSet<_>>();
            for entry in source_entries {
                if seen.insert(entry.clone()) {
                    merged.push(entry);
                }
            }
            if merged.len() == seen.len() && read_entries(&target)?.len() == merged.len() {
                return Ok("skip");
            }
            write_entries(&target, &merged)?;
            Ok("merge")
        })();
        match operation {
            Ok("copy") => result.copied += 1,
            Ok("merge") => result.merged += 1,
            Ok(_) => result.skipped += 1,
            Err(error) => result.warnings.push(format!("{name}: {error}")),
        }
    }
    result
}

fn read_entries(path: &Path) -> std::io::Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(raw
            .split(ENTRY_DELIMITER)
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn write_entries(path: &Path, entries: &[String]) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().expect("project memory has a parent"))?;
    fs::write(path, entries.join(ENTRY_DELIMITER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_worktree_uses_the_checkout_as_its_repo_local_root() {
        let directory = tempfile::tempdir().unwrap();
        let main = directory.path().join("main-repo");
        let linked = directory.path().join("feature-worktree");
        let git_dir = main.join(".git/worktrees/feature");
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        fs::write(git_dir.join("commondir"), "../..\n").unwrap();
        assert_eq!(find_git_repo_root(&linked), Some(linked));
    }
}
