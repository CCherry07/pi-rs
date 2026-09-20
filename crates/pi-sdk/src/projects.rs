//! Durable project definitions. Sessions own resolved copies of their directories.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use pi_core::{WorkspaceRoot, WorkspaceRootId, WorkspaceSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const PROJECT_METADATA_KEY: &str = "pi-rs.project";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<WorkspaceRoot>,
    pub primary_root: WorkspaceRootId,
}

impl Project {
    pub fn single_root(
        id: impl Into<String>,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        let name = name.into();
        Self {
            id: id.into(),
            roots: vec![WorkspaceRoot::external("primary", name.clone(), path)],
            name,
            primary_root: WorkspaceRootId::new("primary"),
        }
    }

    pub fn with_worktree(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let source = self.spec()?.primary_root().path.clone();
        self.with_worktree_from(&source, id, name, path)
    }

    /// Realizes the selected repository as a worktree, retaining every other root.
    /// The new worktree becomes the child's primary root, even if the parent executes elsewhere.
    pub fn with_worktree_from(
        &self,
        source: &Path,
        id: impl Into<String>,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        self.validate()?;
        let mut project = self.clone();
        project.id = id.into();
        project.name = name.into();
        let source_path = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        let source_index = project.roots.iter().position(|root| {
            fs::canonicalize(&root.path).unwrap_or_else(|_| root.path.clone()) == source_path
        });
        let index = match source_index {
            Some(index) => index,
            None => {
                // Legacy Git associations may outlive a removed Project root.
                let mut root_id = "worktree".to_string();
                while project.roots.iter().any(|root| root.id.as_str() == root_id) {
                    root_id.push('_');
                }
                project
                    .roots
                    .push(WorkspaceRoot::external(root_id, &project.name, source));
                project.roots.len() - 1
            }
        };
        let primary = &mut project.roots[index];
        project.primary_root = primary.id.clone();
        primary.ownership = pi_core::WorkspaceRootOwnership::ManagedWorktree {
            source_root: source.to_path_buf(),
        };
        primary.path = path.into();
        project.validate()?;
        Ok(project)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err("project identity and name must be nonempty".into());
        }
        if self.roots.iter().any(|root| !root.path.is_absolute()) {
            return Err("project roots must use absolute paths".into());
        }
        self.spec().map(|_| ())
    }

    /// Pure description, also usable when an external directory is temporarily offline.
    pub fn spec(&self) -> Result<WorkspaceSpec, String> {
        let primary = self
            .roots
            .iter()
            .find(|root| root.id == self.primary_root)
            .ok_or("project primary root is missing")?;
        WorkspaceSpec::new(self.roots.clone(), self.primary_root.clone(), &primary.path)
            .map_err(|error| error.to_string())
    }

    /// Resolves actual paths before constructing a new session. No resources are loaded here.
    pub fn resolve(&self) -> Result<WorkspaceSpec, String> {
        self.validate()?;
        let mut roots = self.roots.clone();
        for root in &mut roots {
            root.path = fs::canonicalize(&root.path)
                .map_err(|error| format!("cannot access root {}: {error}", root.path.display()))?;
            if !root.path.is_dir() {
                return Err(format!("root is not a directory: {}", root.path.display()));
            }
        }
        let primary = roots
            .iter()
            .find(|root| root.id == self.primary_root)
            .expect("validated project")
            .path
            .clone();
        WorkspaceSpec::new(roots, self.primary_root.clone(), primary)
            .map_err(|error| error.to_string())
    }

    pub fn session_metadata(&self) -> Map<String, Value> {
        Map::from_iter([(PROJECT_METADATA_KEY.into(), Value::String(self.id.clone()))])
    }
}

pub fn session_project_id(metadata: Option<&Map<String, Value>>) -> Option<&str> {
    metadata?.get(PROJECT_METADATA_KEY)?.as_str()
}

/// Compatibility association for sessions without a saved Project ID. Only an exact root
/// match counts, and shared roots cannot identify a Project. Offline paths remain comparable.
pub fn project_for_legacy_cwd<'a>(projects: &'a [Project], cwd: &Path) -> Option<&'a Project> {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut matches = projects.iter().filter(|project| {
        project
            .roots
            .iter()
            .any(|root| fs::canonicalize(&root.path).unwrap_or_else(|_| root.path.clone()) == cwd)
    });
    let project = matches.next()?;
    matches.next().is_none().then_some(project)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectDocument {
    schema_version: u32,
    projects: Vec<Project>,
}

/// Each transaction rereads under an OS lock and replaces the document atomically.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    path: PathBuf,
}

impl ProjectStore {
    pub fn new(agent_dir: impl AsRef<Path>) -> Self {
        Self::at_path(agent_dir.as_ref().join("projects.json"))
    }
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn list(&self) -> Result<Vec<Project>, String> {
        self.transaction(|projects| Ok((projects.clone(), false)))
    }
    pub fn get(&self, id: &str) -> Result<Project, String> {
        self.list()?
            .into_iter()
            .find(|project| project.id == id)
            .ok_or_else(|| format!("unknown project: {id}"))
    }
    pub fn upsert(&self, project: Project) -> Result<(), String> {
        project.validate()?;
        self.transaction(move |projects| {
            if let Some(existing) = projects
                .iter_mut()
                .find(|existing| existing.id == project.id)
            {
                *existing = project;
            } else {
                projects.push(project);
            }
            Ok(((), true))
        })
    }
    /// Idempotent migration preserves IDs and never overwrites an edited project.
    pub fn import_missing(&self, legacy: Vec<Project>) -> Result<(), String> {
        for project in &legacy {
            project.validate()?;
        }
        self.transaction(move |projects| {
            let mut changed = false;
            for project in legacy {
                if !projects.iter().any(|existing| existing.id == project.id) {
                    projects.push(project);
                    changed = true;
                }
            }
            Ok(((), changed))
        })
    }
    /// Removes only the association. This module never deletes project directories.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.transaction(|projects| {
            let before = projects.len();
            projects.retain(|project| project.id != id);
            Ok(((), projects.len() != before))
        })
    }

    fn transaction<T>(
        &self,
        update: impl FnOnce(&mut Vec<Project>) -> Result<(T, bool), String>,
    ) -> Result<T, String> {
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))
            .map_err(|error| error.to_string())?;
        lock.lock_exclusive().map_err(|error| error.to_string())?;
        let mut document = match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<ProjectDocument>(&bytes).map_err(|error| {
                format!("invalid project store {}: {error}", self.path.display())
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProjectDocument {
                schema_version: 1,
                projects: vec![],
            },
            Err(error) => return Err(error.to_string()),
        };
        if document.schema_version != 1 {
            return Err(format!(
                "unsupported project schema {}",
                document.schema_version
            ));
        }
        let mut ids = HashSet::new();
        for project in &document.projects {
            project.validate()?;
            if !ids.insert(&project.id) {
                return Err(format!("duplicate project identity {}", project.id));
            }
        }
        let (result, changed) = update(&mut document.projects)?;
        if changed {
            let mut temporary =
                tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
            serde_json::to_writer_pretty(&mut temporary, &document)
                .map_err(|error| error.to_string())?;
            temporary
                .write_all(b"\n")
                .map_err(|error| error.to_string())?;
            temporary
                .as_file()
                .sync_all()
                .map_err(|error| error.to_string())?;
            temporary
                .persist(&self.path)
                .map_err(|error| error.to_string())?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_mapping_tracks_its_source_after_primary_changes_or_removal() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("repo");
        let shared = directory.path().join("shared");
        let branch = directory.path().join("branch");
        let mut project = Project::single_root("project", "project", &source);
        project
            .roots
            .push(WorkspaceRoot::external("shared", "shared", &shared));
        project.primary_root = WorkspaceRootId::new("shared");
        for source_present in [true, false] {
            if !source_present {
                project.roots.retain(|root| root.path != source);
            }
            let child = project
                .with_worktree_from(&source, "child", "branch", &branch)
                .unwrap();
            assert_eq!(child.spec().unwrap().cwd(), branch);
            assert_eq!(child.roots.len(), 2);
            assert!(child.roots.iter().any(|root| root.path == shared));
            assert_eq!(
                child.spec().unwrap().primary_root().ownership,
                pi_core::WorkspaceRootOwnership::ManagedWorktree {
                    source_root: source.clone()
                }
            );
        }
    }

    #[test]
    fn legacy_sessions_match_any_unique_root_after_primary_changes() {
        let directory = tempfile::tempdir().unwrap();
        let old_root = directory.path().join("old");
        let new_root = directory.path().join("new");
        fs::create_dir_all(old_root.join("nested")).unwrap();
        fs::create_dir(&new_root).unwrap();
        let mut project = Project::single_root("first", "first", &old_root);
        project
            .roots
            .push(WorkspaceRoot::external("new", "new", &new_root));
        project.primary_root = WorkspaceRootId::new("new");
        let mut projects = vec![project];
        let saved_cwd = fs::canonicalize(&old_root).unwrap();
        assert_eq!(
            project_for_legacy_cwd(&projects, &saved_cwd).unwrap().id,
            "first"
        );
        assert!(project_for_legacy_cwd(&projects, &old_root.join("nested")).is_none());
        projects.push(Project::single_root("second", "second", &old_root));
        assert!(project_for_legacy_cwd(&projects, &saved_cwd).is_none());
        assert_eq!(
            project_for_legacy_cwd(&projects, &new_root).unwrap().id,
            "first"
        );
    }

    #[test]
    fn migration_is_idempotent_and_removal_keeps_directories() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(root.path());
        let mut project = Project::single_root("old-id", "app", root.path());
        store.import_missing(vec![project.clone()]).unwrap();
        project.name = "edited".into();
        store.upsert(project.clone()).unwrap();
        store
            .import_missing(vec![Project::single_root(
                "old-id",
                "old name",
                root.path(),
            )])
            .unwrap();
        assert_eq!(store.get("old-id").unwrap(), project);
        store.remove("old-id").unwrap();
        assert!(root.path().is_dir());
        fs::write(root.path().join("projects.json"), "broken").unwrap();
        assert!(store.upsert(project).is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("projects.json")).unwrap(),
            "broken"
        );
    }

    #[test]
    fn concurrent_updates_do_not_lose_projects() {
        let root = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for index in 0..8 {
                let path = root.path();
                scope.spawn(move || {
                    ProjectStore::new(path)
                        .upsert(Project::single_root(index.to_string(), "project", path))
                        .unwrap()
                });
            }
        });
        assert_eq!(ProjectStore::new(root.path()).list().unwrap().len(), 8);
    }
}
