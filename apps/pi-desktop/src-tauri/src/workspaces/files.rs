use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use pi_core::{WorkspaceRootId, WorkspaceSpec};
use serde::{Deserialize, Serialize};

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "dist" | "target" | "release-artifacts"
    )
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceFileRef {
    root_id: WorkspaceRootId,
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceRootError {
    root_id: WorkspaceRootId,
    message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceFileListing {
    pub workspace: WorkspaceSpec,
    pub files: Vec<WorkspaceFileRef>,
    pub errors: Vec<WorkspaceRootError>,
}

pub(crate) fn list_workspace_files_inner(workspace: WorkspaceSpec) -> WorkspaceFileListing {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for root in workspace.roots() {
        let (paths, error) = list_root_files(&root.path);
        files.extend(paths.into_iter().map(|path| WorkspaceFileRef {
            root_id: root.id.clone(),
            path,
        }));
        if let Some(message) = error {
            errors.push(WorkspaceRootError {
                root_id: root.id.clone(),
                message,
            });
        }
    }
    WorkspaceFileListing {
        workspace,
        files,
        errors,
    }
}

fn list_root_files(root: &PathBuf) -> (Vec<String>, Option<String>) {
    if let Err(error) = std::fs::read_dir(root) {
        return (Vec::new(), Some(error.to_string()));
    }
    let mut results = Vec::new();
    let mut scan_error = None;
    let walker = WalkBuilder::new(root)
        // Allow hidden entries.
        .hidden(false)
        // Avoid crawling symlink targets.
        .follow_links(false)
        // Don't require git to be present to apply to apply git-related ignore rules.
        .require_git(false)
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy();
                return !should_skip_dir(&name);
            }
            true
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                scan_error.get_or_insert_with(|| error.to_string());
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if let Ok(rel_path) = entry.path().strip_prefix(root) {
            let normalized = rel_path
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if !normalized.is_empty() {
                results.push(normalized);
            }
        }
    }

    results.sort();
    (results, scan_error)
}

const MAX_WORKSPACE_FILE_BYTES: u64 = 400_000;

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WorkspaceFileResponse {
    content: String,
    truncated: bool,
}

pub(crate) fn read_workspace_file_inner(
    workspace: &WorkspaceSpec,
    root_id: Option<&str>,
    relative_path: &str,
) -> Result<WorkspaceFileResponse, String> {
    let root = match root_id {
        Some(id) => workspace
            .roots()
            .iter()
            .find(|root| root.id.as_str() == id)
            .map(|root| root.path.as_path())
            .ok_or_else(|| format!("Unknown workspace root: {id}"))?,
        None => workspace.cwd(),
    };
    if Path::new(relative_path).is_absolute() {
        return Err("Expected a root-relative file path".into());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|err| format!("Failed to resolve workspace root: {err}"))?;
    let candidate = canonical_root.join(relative_path);
    let canonical_path = candidate
        .canonicalize()
        .map_err(|err| format!("Failed to open file: {err}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("Invalid file path".to_string());
    }
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|err| format!("Failed to read file metadata: {err}"))?;
    if !metadata.is_file() {
        return Err("Path is not a file".to_string());
    }

    let file = File::open(&canonical_path).map_err(|err| format!("Failed to open file: {err}"))?;
    let mut buffer = Vec::new();
    file.take(MAX_WORKSPACE_FILE_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|err| format!("Failed to read file: {err}"))?;

    let truncated = buffer.len() > MAX_WORKSPACE_FILE_BYTES as usize;
    if truncated {
        buffer.truncate(MAX_WORKSPACE_FILE_BYTES as usize);
    }

    let content = String::from_utf8(buffer).map_err(|_| "File is not valid UTF-8".to_string())?;
    Ok(WorkspaceFileResponse { content, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::WorkspaceRoot;

    #[test]
    fn multi_root_listing_and_preview_keep_duplicate_paths_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = Vec::new();
        for name in ["app", "shared"] {
            let path = temp.path().join(name);
            std::fs::create_dir_all(path.join("src")).unwrap();
            std::fs::write(path.join("src/index.ts"), name).unwrap();
            std::fs::write(path.join("README.md"), name).unwrap();
            std::fs::create_dir_all(path.join("node_modules")).unwrap();
            std::fs::write(path.join("node_modules/ignored"), "ignored").unwrap();
            roots.push(WorkspaceRoot::external(name, name, path));
        }
        let workspace = WorkspaceSpec::new(
            roots,
            WorkspaceRootId::new("app"),
            temp.path().join("app/src"),
        )
        .unwrap();
        let listing = list_workspace_files_inner(workspace.clone());
        assert!(listing.errors.is_empty());
        assert_eq!(listing.files.len(), 4);
        for id in ["app", "shared"] {
            assert!(listing.files.contains(&WorkspaceFileRef {
                root_id: WorkspaceRootId::new(id),
                path: "src/index.ts".into()
            }));
            assert_eq!(
                read_workspace_file_inner(&workspace, Some(id), "src/index.ts")
                    .unwrap()
                    .content,
                id
            );
        }
        // Legacy callers still resolve from executionDir, which may be below the primary root.
        assert_eq!(
            read_workspace_file_inner(&workspace, None, "index.ts")
                .unwrap()
                .content,
            "app"
        );
        assert!(read_workspace_file_inner(&workspace, Some("missing"), "src/index.ts").is_err());
        assert!(
            read_workspace_file_inner(&workspace, Some("app"), "../shared/src/index.ts").is_err()
        );
        assert!(read_workspace_file_inner(
            &workspace,
            Some("app"),
            &temp.path().join("shared/src/index.ts").to_string_lossy()
        )
        .is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path().join("shared"), temp.path().join("app/link"))
                .unwrap();
            assert!(
                read_workspace_file_inner(&workspace, Some("app"), "link/src/index.ts").is_err()
            );
        }
    }

    #[test]
    fn unavailable_roots_report_errors_without_hiding_available_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("README.md"), "available").unwrap();
        let workspace = WorkspaceSpec::new(
            vec![
                WorkspaceRoot::external("app", "app", temp.path()),
                WorkspaceRoot::external("offline", "offline", temp.path().join("missing")),
            ],
            WorkspaceRootId::new("app"),
            temp.path(),
        )
        .unwrap();
        let listing = list_workspace_files_inner(workspace);
        assert_eq!(listing.files.len(), 1);
        assert_eq!(listing.errors.len(), 1);
        assert_eq!(listing.errors[0].root_id.as_str(), "offline");
    }
}
