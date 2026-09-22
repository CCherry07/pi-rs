//! Durable project definitions. Sessions own resolved copies of their directories.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

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
    /// Omitted legacy values execute at the primary root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_dir: Option<PathBuf>,
}

/// A checkout selected by the caller's Git discovery, before or after worktree creation.
///
/// Paths must be canonical absolute descriptions. `roots` contains every workspace root whose
/// actual Git checkout is `source_checkout`; lexical containment alone cannot establish that
/// membership because a nested directory may belong to a different repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceWorktreeMapping {
    pub source_checkout: PathBuf,
    pub worktree_path: PathBuf,
    pub roots: Vec<WorkspaceRootId>,
    /// Git discovery must classify the execution directory separately from its primary root.
    pub contains_execution_dir: bool,
}

/// Applies explicitly discovered checkout membership without reading or changing the filesystem.
///
/// Root IDs, names, order, and unselected roots are retained. Selected roots and a selected
/// execution directory keep their offsets within the checkout. An explicit `execution_root`
/// starts at that root's resulting path; otherwise a mapped execution directory is required.
/// The returned value owns no checkout lifecycle and grants no permission to delete directories.
pub fn realize_worktrees(
    source: &WorkspaceSpec,
    mappings: &[WorkspaceWorktreeMapping],
    execution_root: Option<&WorkspaceRootId>,
) -> Result<WorkspaceSpec, String> {
    let mut roots = source.roots().to_vec();
    let mut source_checkouts = HashSet::new();
    let mut destinations = HashSet::new();
    let mut mapped_roots = HashSet::new();
    let mut mapped_execution_dir = None;
    for mapping in mappings {
        for path in [&mapping.source_checkout, &mapping.worktree_path] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|component| component == Component::ParentDir)
            {
                return Err("worktree mappings require canonical absolute checkout paths".into());
            }
        }
        if !source_checkouts.insert(&mapping.source_checkout) {
            return Err(format!(
                "duplicate source checkout {}",
                mapping.source_checkout.display()
            ));
        }
        if !destinations.insert(&mapping.worktree_path) {
            return Err(format!(
                "duplicate worktree destination {}",
                mapping.worktree_path.display()
            ));
        }
        if mapping.source_checkout == mapping.worktree_path {
            return Err("worktree destination must differ from its source checkout".into());
        }
        for root_id in &mapping.roots {
            if !mapped_roots.insert(root_id) {
                return Err(format!(
                    "root {} is mapped more than once",
                    root_id.as_str()
                ));
            }
            let root = roots
                .iter_mut()
                .find(|root| root.id == *root_id)
                .ok_or_else(|| format!("unknown worktree root {}", root_id.as_str()))?;
            root.path = map_checkout_path(&root.path, mapping)?;
            root.ownership = pi_core::WorkspaceRootOwnership::ManagedWorktree {
                source_root: mapping.source_checkout.clone(),
            };
        }
        if mapping.contains_execution_dir {
            if mapped_execution_dir.is_some() {
                return Err("execution directory is assigned to multiple checkouts".into());
            }
            mapped_execution_dir = Some(map_checkout_path(source.cwd(), mapping)?);
        }
    }

    let (primary_root, execution_dir) = if let Some(root_id) = execution_root {
        let root = roots
            .iter()
            .find(|root| root.id == *root_id)
            .ok_or_else(|| format!("unknown execution root {}", root_id.as_str()))?;
        (root.id.clone(), root.path.clone())
    } else if let Some(execution_dir) = mapped_execution_dir {
        let primary = roots
            .iter()
            .find(|root| {
                root.id == *source.primary_root_id()
                    && mapped_roots.contains(&root.id)
                    && execution_dir.starts_with(&root.path)
            })
            .or_else(|| {
                roots
                    .iter()
                    .filter(|root| {
                        mapped_roots.contains(&root.id) && execution_dir.starts_with(&root.path)
                    })
                    .max_by_key(|root| root.path.components().count())
            })
            .ok_or("mapped execution directory is outside every mapped workspace root")?;
        (primary.id.clone(), execution_dir)
    } else if mappings.is_empty() {
        return Ok(source.clone());
    } else {
        return Err(
            "execution directory is outside the selected checkouts; select an execution root"
                .into(),
        );
    };
    WorkspaceSpec::new(roots, primary_root, execution_dir).map_err(|error| error.to_string())
}

fn map_checkout_path(path: &Path, mapping: &WorkspaceWorktreeMapping) -> Result<PathBuf, String> {
    let offset = path.strip_prefix(&mapping.source_checkout).map_err(|_| {
        format!(
            "{} is outside source checkout {}",
            path.display(),
            mapping.source_checkout.display()
        )
    })?;
    if offset
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "invalid checkout-relative path {}",
            offset.display()
        ));
    }
    Ok(mapping.worktree_path.join(offset))
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
            execution_dir: None,
        }
    }

    pub fn from_workspace(
        id: impl Into<String>,
        name: impl Into<String>,
        workspace: &WorkspaceSpec,
    ) -> Result<Self, String> {
        let project = Self {
            id: id.into(),
            name: name.into(),
            roots: workspace.roots().to_vec(),
            primary_root: workspace.primary_root_id().clone(),
            execution_dir: (workspace.cwd() != workspace.primary_root().path)
                .then(|| workspace.cwd().to_path_buf()),
        };
        project.validate()?;
        Ok(project)
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
        project.execution_dir = None;
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
        WorkspaceSpec::new(
            self.roots.clone(),
            self.primary_root.clone(),
            self.execution_dir.as_ref().unwrap_or(&primary.path),
        )
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
        let execution_dir = if let Some(path) = &self.execution_dir {
            let resolved = fs::canonicalize(path).map_err(|error| {
                format!(
                    "cannot access execution directory {}: {error}",
                    path.display()
                )
            })?;
            if !resolved.is_dir() {
                return Err(format!(
                    "execution path is not a directory: {}",
                    path.display()
                ));
            }
            resolved
        } else {
            roots
                .iter()
                .find(|root| root.id == self.primary_root)
                .expect("validated project")
                .path
                .clone()
        };
        WorkspaceSpec::new(roots, self.primary_root.clone(), execution_dir)
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

    fn worktree_mapping(
        source: &str,
        destination: &str,
        roots: &[&str],
        contains_execution_dir: bool,
    ) -> WorkspaceWorktreeMapping {
        WorkspaceWorktreeMapping {
            source_checkout: source.into(),
            worktree_path: destination.into(),
            roots: roots.iter().map(|id| WorkspaceRootId::new(*id)).collect(),
            contains_execution_dir,
        }
    }

    #[test]
    fn multiple_worktrees_map_all_member_roots_and_preserve_execution_offset() {
        let roots = vec![
            WorkspaceRoot::external("web", "Web", "/repos/web"),
            WorkspaceRoot::external("web-src", "Web source", "/repos/web/src"),
            WorkspaceRoot::external("api", "API", "/repos/api/service"),
            WorkspaceRoot::external("nested", "Nested repo", "/repos/web/vendor/nested"),
            WorkspaceRoot::external("assets", "Assets", "/shared/assets"),
        ];
        let source = WorkspaceSpec::new(
            roots.clone(),
            WorkspaceRootId::new("web-src"),
            "/repos/web/src/components",
        )
        .unwrap();
        let mapped = realize_worktrees(
            &source,
            &[
                worktree_mapping("/repos/web", "/worktrees/web", &["web", "web-src"], true),
                worktree_mapping("/repos/api", "/worktrees/api", &["api"], false),
            ],
            None,
        )
        .unwrap();

        assert_eq!(mapped.primary_root_id(), source.primary_root_id());
        assert_eq!(mapped.cwd(), Path::new("/worktrees/web/src/components"));
        assert_eq!(mapped.roots().len(), roots.len());
        for (original, mapped) in roots.iter().zip(mapped.roots()) {
            assert_eq!(mapped.id, original.id);
            assert_eq!(mapped.name, original.name);
        }
        for (index, path, checkout) in [
            (0, "/worktrees/web", "/repos/web"),
            (1, "/worktrees/web/src", "/repos/web"),
            (2, "/worktrees/api/service", "/repos/api"),
        ] {
            assert_eq!(mapped.roots()[index].path, Path::new(path));
            assert_eq!(
                mapped.roots()[index].ownership,
                pi_core::WorkspaceRootOwnership::ManagedWorktree {
                    source_root: checkout.into()
                }
            );
        }
        // Explicit ownership protects both a nested repository and an unrelated directory.
        assert_eq!(&mapped.roots()[3..], &roots[3..]);
        assert_eq!(source.roots(), roots);
        assert_eq!(source.cwd(), Path::new("/repos/web/src/components"));
    }

    #[test]
    fn nested_execution_checkout_requires_explicit_membership_or_root_selection() {
        let roots = vec![
            WorkspaceRoot::external("outer", "Outer", "/repos/outer"),
            WorkspaceRoot::external("nested", "Nested", "/repos/outer/nested"),
        ];
        let source = WorkspaceSpec::new(
            roots.clone(),
            WorkspaceRootId::new("outer"),
            "/repos/outer/nested/src",
        )
        .unwrap();
        let mappings = [worktree_mapping(
            "/repos/outer",
            "/worktrees/outer",
            &["outer"],
            false,
        )];
        assert!(
            realize_worktrees(&source, &mappings, None)
                .unwrap_err()
                .contains("select an execution root")
        );

        let mapped =
            realize_worktrees(&source, &mappings, Some(&WorkspaceRootId::new("outer"))).unwrap();
        assert_eq!(mapped.cwd(), Path::new("/worktrees/outer"));
        assert_eq!(mapped.roots()[1], roots[1]);

        let keep_nested =
            realize_worktrees(&source, &mappings, Some(&WorkspaceRootId::new("nested"))).unwrap();
        assert_eq!(keep_nested.cwd(), Path::new("/repos/outer/nested"));
        assert_eq!(keep_nested.primary_root_id().as_str(), "nested");
    }

    #[test]
    fn mapped_execution_selects_its_own_root_when_original_primary_belongs_elsewhere() {
        let source = WorkspaceSpec::new(
            vec![
                WorkspaceRoot::external("outer", "Outer", "/repos/outer"),
                WorkspaceRoot::external("nested", "Nested", "/repos/outer/nested"),
                WorkspaceRoot::external("src", "Source", "/repos/outer/nested/src"),
            ],
            WorkspaceRootId::new("outer"),
            "/repos/outer/nested/src/module",
        )
        .unwrap();
        let mappings = [worktree_mapping(
            "/repos/outer/nested",
            "/worktrees/nested",
            &["nested", "src"],
            true,
        )];
        let mapped = realize_worktrees(&source, &mappings, None).unwrap();
        assert_eq!(mapped.primary_root_id().as_str(), "src");
        assert_eq!(mapped.cwd(), Path::new("/worktrees/nested/src/module"));
        assert_eq!(mapped.roots()[0], source.roots()[0]);

        let explicit =
            realize_worktrees(&source, &mappings, Some(&WorkspaceRootId::new("nested"))).unwrap();
        assert_eq!(explicit.primary_root_id().as_str(), "nested");
        assert_eq!(explicit.cwd(), Path::new("/worktrees/nested"));
    }

    #[test]
    fn worktree_mapping_rejects_duplicate_and_invalid_membership() {
        let source = WorkspaceSpec::new(
            vec![
                WorkspaceRoot::external("first", "First", "/repos/first"),
                WorkspaceRoot::external("second", "Second", "/repos/second"),
            ],
            WorkspaceRootId::new("first"),
            "/repos/first/src",
        )
        .unwrap();
        let first = worktree_mapping("/repos/first", "/worktrees/first", &["first"], true);
        for (mappings, expected) in [
            (vec![first.clone(), first.clone()], "duplicate source"),
            (
                vec![
                    first.clone(),
                    worktree_mapping("/repos/second", "/worktrees/first", &["second"], false),
                ],
                "duplicate worktree destination",
            ),
            (
                vec![worktree_mapping(
                    "/repos/first",
                    "/worktrees/first",
                    &["first", "first"],
                    true,
                )],
                "mapped more than once",
            ),
            (
                vec![worktree_mapping(
                    "/repos/first",
                    "/worktrees/first",
                    &["missing"],
                    true,
                )],
                "unknown worktree root",
            ),
            (
                vec![worktree_mapping(
                    "/repos/first",
                    "/worktrees/first",
                    &["second"],
                    true,
                )],
                "outside source checkout",
            ),
            (
                vec![
                    first.clone(),
                    worktree_mapping("/repos/second", "/worktrees/second", &["second"], true),
                ],
                "execution directory is assigned to multiple checkouts",
            ),
            (
                vec![worktree_mapping(
                    "repos/first",
                    "/worktrees/first",
                    &["first"],
                    true,
                )],
                "canonical absolute",
            ),
            (
                vec![worktree_mapping(
                    "/repos/first",
                    "worktrees/first",
                    &["first"],
                    true,
                )],
                "canonical absolute",
            ),
            (
                vec![worktree_mapping(
                    "/repos/first",
                    "/repos/first",
                    &["first"],
                    true,
                )],
                "must differ",
            ),
        ] {
            assert!(
                realize_worktrees(&source, &mappings, None)
                    .unwrap_err()
                    .contains(expected),
                "{expected}"
            );
        }
        assert!(
            realize_worktrees(&source, &[first], Some(&WorkspaceRootId::new("missing")))
                .unwrap_err()
                .contains("unknown execution root")
        );
    }

    #[test]
    fn worktree_mapping_rejects_parent_relative_offsets_and_unrepresented_execution() {
        let source = WorkspaceSpec::new(
            vec![WorkspaceRoot::external(
                "root",
                "Root",
                "/repos/first/../second",
            )],
            WorkspaceRootId::new("root"),
            "/repos/first/../second",
        )
        .unwrap();
        let mappings = [worktree_mapping(
            "/repos/first",
            "/worktrees/first",
            &["root"],
            true,
        )];
        assert!(
            realize_worktrees(&source, &mappings, None)
                .unwrap_err()
                .contains("invalid checkout-relative")
        );

        let source = WorkspaceSpec::new(
            vec![WorkspaceRoot::external("root", "Root", "/repos")],
            WorkspaceRootId::new("root"),
            "/repos/first/src",
        )
        .unwrap();
        let mappings = [worktree_mapping(
            "/repos/first",
            "/worktrees/first",
            &[],
            true,
        )];
        assert!(
            realize_worktrees(&source, &mappings, None)
                .unwrap_err()
                .contains("outside every mapped workspace root")
        );
        // An unmapped ancestor containing the destination does not represent the new checkout.
        let mappings = [worktree_mapping(
            "/repos/first",
            "/repos/worktree",
            &[],
            true,
        )];
        assert!(
            realize_worktrees(&source, &mappings, None)
                .unwrap_err()
                .contains("outside every mapped workspace root")
        );
    }

    #[test]
    fn empty_worktree_mapping_preserves_the_source_value() {
        let source = WorkspaceSpec::from_cwd("/offline/project/src");
        assert_eq!(realize_worktrees(&source, &[], None).unwrap(), source);
    }

    #[test]
    fn mapped_project_persistence_and_resolution_retain_execution_subdirectory() {
        let directory = tempfile::tempdir().unwrap();
        let directory = fs::canonicalize(directory.path()).unwrap();
        let source_path = directory.join("source");
        let worktree_path = directory.join("worktree");
        fs::create_dir_all(source_path.join("src/module")).unwrap();
        fs::create_dir_all(worktree_path.join("src/module")).unwrap();
        let source = WorkspaceSpec::new(
            vec![WorkspaceRoot::external("repo", "Repository", &source_path)],
            WorkspaceRootId::new("repo"),
            source_path.join("src/module"),
        )
        .unwrap();
        let mapped = realize_worktrees(
            &source,
            &[WorkspaceWorktreeMapping {
                source_checkout: source_path,
                worktree_path: worktree_path.clone(),
                roots: vec![WorkspaceRootId::new("repo")],
                contains_execution_dir: true,
            }],
            None,
        )
        .unwrap();
        let project = Project::from_workspace("child", "Child", &mapped).unwrap();
        assert_eq!(
            project.execution_dir,
            Some(worktree_path.join("src/module"))
        );
        assert_eq!(project.spec().unwrap(), mapped);

        let store = ProjectStore::new(&directory);
        store.upsert(project.clone()).unwrap();
        let saved = store.get("child").unwrap();
        assert_eq!(saved, project);
        assert_eq!(saved.spec().unwrap(), mapped);
        assert_eq!(saved.resolve().unwrap(), mapped);
        assert_eq!(
            serde_json::to_value(saved).unwrap()["executionDir"],
            serde_json::to_value(worktree_path.join("src/module")).unwrap()
        );
    }

    #[test]
    fn legacy_projects_default_to_primary_root_without_execution_field() {
        let project = Project::single_root("legacy", "Legacy", "/offline/project");
        let wire = serde_json::to_value(&project).unwrap();
        assert!(wire.get("executionDir").is_none());
        let decoded: Project = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.execution_dir, None);
        assert_eq!(decoded.spec().unwrap().cwd(), Path::new("/offline/project"));
        assert_eq!(
            Project::from_workspace("legacy", "Legacy", &decoded.spec().unwrap())
                .unwrap()
                .execution_dir,
            None
        );

        let mut invalid = decoded;
        invalid.execution_dir = Some("/another/project".into());
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn project_resolution_checks_explicit_execution_directory() {
        let directory = tempfile::tempdir().unwrap();
        let mut project = Project::single_root("project", "Project", directory.path());
        let execution = directory.path().join("src");
        project.execution_dir = Some(execution.clone());
        assert!(project.spec().is_ok());
        assert!(
            project
                .resolve()
                .unwrap_err()
                .contains("cannot access execution")
        );
        fs::write(&execution, "a file").unwrap();
        assert!(project.resolve().unwrap_err().contains("not a directory"));
    }

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
        project.execution_dir = Some(shared.join("nested"));
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
