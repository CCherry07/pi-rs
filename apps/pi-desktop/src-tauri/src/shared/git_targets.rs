//! Session-scoped checkout discovery and target validation for every desktop Git operation.
use std::path::{Path, PathBuf};

use git2::Repository;
use pi_core::{WorkspaceRootId, WorkspaceSpec};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCheckout {
    /// Canonical per-worktree git directory. Linked worktrees have different keys.
    pub key: String,
    pub workdir: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub root_ids: Vec<WorkspaceRootId>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum GitTarget {
    Checkout {
        key: String,
        workdir: PathBuf,
    },
    /// A root without a repository, used by the initialization flow.
    Directory {
        root_id: WorkspaceRootId,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiscoveryError {
    root_id: WorkspaceRootId,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitInventory {
    pub workspace: WorkspaceSpec,
    pub checkouts: Vec<GitCheckout>,
    pub errors: Vec<GitDiscoveryError>,
    pub directory_root_ids: Vec<WorkspaceRootId>,
    pub default_checkout_key: Option<String>,
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn checkout(path: &Path) -> Result<GitCheckout, String> {
    let repo = Repository::discover(path).map_err(|error| error.to_string())?;
    let workdir = canonical(
        repo.workdir()
            .ok_or("Bare repositories have no working directory")?,
    )?;
    let git_dir = canonical(repo.path())?;
    Ok(GitCheckout {
        key: git_dir.to_string_lossy().into_owned(),
        workdir,
        common_dir: canonical(repo.commondir())?,
        git_dir,
        root_ids: Vec::new(),
    })
}

pub(crate) fn target_at(path: &Path) -> Result<GitTarget, String> {
    let checkout = checkout(path)?;
    Ok(GitTarget::Checkout {
        key: checkout.key,
        workdir: checkout.workdir,
    })
}

pub(crate) fn discover(workspace: WorkspaceSpec, depth: Option<usize>) -> GitInventory {
    let mut checkouts: Vec<GitCheckout> = Vec::new();
    let mut errors = Vec::new();
    let mut directory_root_ids = Vec::new();
    let default_checkout_key = checkout(workspace.cwd()).ok().map(|item| item.key);
    for root in workspace.roots() {
        let mut paths = vec![root.path.clone()];
        if !root.path.is_dir() {
            errors.push(GitDiscoveryError {
                root_id: root.id.clone(),
                message: format!("Directory unavailable: {}", root.path.display()),
            });
            continue;
        }
        if Repository::discover(&root.path)
            .is_err_and(|error| error.code() == git2::ErrorCode::NotFound)
        {
            directory_root_ids.push(root.id.clone());
        }
        if let Some(depth) = depth {
            paths.extend(
                crate::git_utils::list_git_roots(&root.path, depth.clamp(1, 6), 200)
                    .into_iter()
                    .map(|path| root.path.join(path)),
            );
        }
        for path in paths {
            match checkout(&path) {
                Ok(mut candidate) => {
                    if let Some(existing) =
                        checkouts.iter_mut().find(|item| item.key == candidate.key)
                    {
                        if !existing.root_ids.contains(&root.id) {
                            existing.root_ids.push(root.id.clone());
                        }
                    } else {
                        candidate.root_ids.push(root.id.clone());
                        checkouts.push(candidate);
                    }
                }
                Err(message) => {
                    // A plain directory is a valid workspace root. A broken .git is an error.
                    if path.join(".git").exists() {
                        errors.push(GitDiscoveryError {
                            root_id: root.id.clone(),
                            message,
                        });
                    }
                }
            }
        }
    }
    GitInventory {
        workspace,
        checkouts,
        errors,
        directory_root_ids,
        default_checkout_key,
    }
}

/// Legacy gitRoot may seed the view, but never routes a later operation.
pub(crate) fn prefer_path(inventory: &mut GitInventory, path: &Path) {
    let Ok(mut candidate) = checkout(path) else {
        return;
    };
    let target = GitTarget::Checkout {
        key: candidate.key.clone(),
        workdir: candidate.workdir.clone(),
    };
    if resolve(&inventory.workspace, Some(&target)).is_err() {
        return;
    }
    inventory.default_checkout_key = Some(candidate.key.clone());
    if !inventory
        .checkouts
        .iter()
        .any(|item| item.key == candidate.key)
    {
        candidate.root_ids = inventory
            .workspace
            .roots()
            .iter()
            .filter(|root| {
                canonical(&root.path).is_ok_and(|path| candidate.workdir.starts_with(path))
                    || checkout(&root.path).is_ok_and(|other| other.key == candidate.key)
            })
            .map(|root| root.id.clone())
            .collect();
        inventory.checkouts.push(candidate);
    }
}

pub(crate) fn resolve(
    workspace: &WorkspaceSpec,
    target: Option<&GitTarget>,
) -> Result<PathBuf, String> {
    match target {
        Some(GitTarget::Checkout { key, workdir }) => {
            let current = checkout(workdir)?;
            if current.key != *key || current.workdir != canonical(workdir)? {
                return Err("Git checkout changed; refresh the repository selection".into());
            }
            let associated = workspace.roots().iter().any(|root| {
                let Ok(root_path) = canonical(&root.path) else {
                    return false;
                };
                current.workdir.starts_with(&root_path)
                    || checkout(&root_path).is_ok_and(|candidate| candidate.key == current.key)
            });
            if !associated {
                return Err("Git checkout does not belong to this session's workspace".into());
            }
            Ok(current.workdir)
        }
        Some(GitTarget::Directory { root_id }) => {
            let root = workspace
                .roots()
                .iter()
                .find(|root| root.id == *root_id)
                .ok_or("Unknown workspace root")?;
            if checkout(&root.path).is_ok() {
                return Err(
                    "A Git repository now exists here; refresh the repository selection".into(),
                );
            }
            canonical(&root.path)
        }
        None => {
            let inventory = discover(workspace.clone(), None);
            match inventory.checkouts.as_slice() {
                [only] => Ok(only.workdir.clone()),
                [] if workspace.roots().len() == 1 => canonical(workspace.cwd()),
                _ => Err("Select an explicit Git checkout for this workspace".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::WorkspaceRoot;

    fn spec(paths: &[&Path]) -> WorkspaceSpec {
        WorkspaceSpec::new(
            paths
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    WorkspaceRoot::external(i.to_string(), i.to_string(), path.to_path_buf())
                })
                .collect(),
            WorkspaceRootId::new("0"),
            paths[0],
        )
        .unwrap()
    }
    fn target(checkout: &GitCheckout) -> GitTarget {
        GitTarget::Checkout {
            key: checkout.key.clone(),
            workdir: checkout.workdir.clone(),
        }
    }
    fn init(path: &Path) -> Repository {
        let repo = Repository::init(path).unwrap();
        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config()
            .unwrap()
            .set_str("user.email", "test@example.com")
            .unwrap();
        let oid = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(oid).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        repo
    }

    #[test]
    fn deduplicates_roots_but_preserves_linked_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        let repo = init(&path);
        let sub = path.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let linked = temp.path().join("linked");
        repo.worktree("test-linked", &linked, None).unwrap();
        let workspace = spec(&[&path, &sub, &linked]);
        let inventory = discover(workspace.clone(), None);
        assert_eq!(inventory.checkouts.len(), 2);
        let first = &inventory.checkouts[0];
        let second = &inventory.checkouts[1];
        assert_eq!(first.root_ids.len(), 2);
        assert_eq!(first.common_dir, second.common_dir);
        assert_ne!(first.key, second.key);
        assert_eq!(
            resolve(&workspace, Some(&target(second))).unwrap(),
            linked.canonicalize().unwrap()
        );
        assert!(resolve(&workspace, None).is_err());
    }

    #[test]
    fn discovers_nested_repositories_only_on_explicit_scan_and_rejects_stale_targets() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let nested = parent.join("nested");
        init(&nested);
        let workspace = spec(&[&parent]);
        assert!(discover(workspace.clone(), None).checkouts.is_empty());
        let inventory = discover(workspace.clone(), Some(2));
        assert_eq!(inventory.checkouts.len(), 1);
        let selected = target(&inventory.checkouts[0]);
        assert_eq!(
            resolve(&workspace, Some(&selected)).unwrap(),
            nested.canonicalize().unwrap()
        );
        let outside = temp.path().join("outside");
        init(&outside);
        assert!(resolve(&spec(&[&outside]), Some(&selected)).is_err());
        let invalid = GitTarget::Checkout {
            key: "stale".into(),
            workdir: nested,
        };
        assert!(resolve(&workspace, Some(&invalid)).is_err());
    }

    #[tokio::test]
    async fn stage_and_commit_touch_only_selected_checkout_including_files_above_a_root() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let repo_a = init(&a);
        let repo_b = init(&b);
        let sub = a.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(a.join("same.txt"), "a").unwrap();
        std::fs::write(b.join("same.txt"), "b").unwrap();
        let old_b = repo_b.head().unwrap().target().unwrap();
        let workspace = spec(&[&sub, &b]);
        let selected = target(&discover(workspace.clone(), None).checkouts[0]);
        let resolved = resolve(&workspace, Some(&selected)).unwrap();
        crate::shared::git_ui_core::stage_git_all_core(resolved.clone())
            .await
            .unwrap();
        let mut index = repo_a.index().unwrap();
        index.read(true).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(repo_b.index().unwrap().len(), 0);
        crate::shared::git_ui_core::commit_git_core(resolved, "selected checkout".into())
            .await
            .unwrap();
        assert_eq!(
            repo_a.head().unwrap().peel_to_commit().unwrap().message(),
            Some("selected checkout\n")
        );
        assert_eq!(repo_b.head().unwrap().target(), Some(old_b));
    }

    #[test]
    fn initialization_target_cannot_silently_become_another_checkout() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = spec(&[temp.path()]);
        let selected = GitTarget::Directory {
            root_id: WorkspaceRootId::new("0"),
        };
        assert!(resolve(&workspace, Some(&selected)).is_ok());
        init(temp.path());
        assert!(resolve(&workspace, Some(&selected)).is_err());
    }
}
