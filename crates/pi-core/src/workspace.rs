//! Execution directories, independent of project storage and filesystem policy.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceRootId(String);

impl WorkspaceRootId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provenance is descriptive; it never authorizes deleting a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkspaceRootOwnership {
    #[default]
    External,
    ManagedWorktree {
        source_root: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub ownership: WorkspaceRootOwnership,
}

impl WorkspaceRoot {
    pub fn external(
        id: impl Into<String>,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            id: WorkspaceRootId::new(id),
            name: name.into(),
            path: path.into(),
            ownership: WorkspaceRootOwnership::External,
        }
    }
}

/// A validated value owned by a session. Changing a project cannot change it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "WorkspaceSpecWire")]
pub struct WorkspaceSpec {
    roots: Vec<WorkspaceRoot>,
    primary_root: WorkspaceRootId,
    execution_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSpecWire {
    roots: Vec<WorkspaceRoot>,
    primary_root: WorkspaceRootId,
    execution_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid workspace: {0}")]
pub struct WorkspaceError(pub String);

impl TryFrom<WorkspaceSpecWire> for WorkspaceSpec {
    type Error = WorkspaceError;

    fn try_from(value: WorkspaceSpecWire) -> Result<Self, Self::Error> {
        Self::new(value.roots, value.primary_root, value.execution_dir)
    }
}

impl WorkspaceSpec {
    pub fn new(
        roots: Vec<WorkspaceRoot>,
        primary_root: WorkspaceRootId,
        execution_dir: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceError> {
        let execution_dir = execution_dir.into();
        let mut ids = HashSet::new();
        for root in &roots {
            if root.id.as_str().trim().is_empty()
                || root.name.trim().is_empty()
                || root.path.as_os_str().is_empty()
            {
                return Err(WorkspaceError(
                    "root identity, name and path must be nonempty".into(),
                ));
            }
            if !ids.insert(&root.id) {
                return Err(WorkspaceError(format!(
                    "duplicate root identity {}",
                    root.id.as_str()
                )));
            }
        }
        let primary = roots
            .iter()
            .find(|root| root.id == primary_root)
            .ok_or_else(|| WorkspaceError("primary root must reference an existing root".into()))?;
        let normalized_execution = normalize(&execution_dir);
        let normalized_primary = normalize(&primary.path);
        if (normalized_primary.as_os_str().is_empty()
            && normalized_execution.components().next() == Some(Component::ParentDir))
            || execution_dir.is_absolute() != primary.path.is_absolute()
            || execution_dir.as_os_str().is_empty()
            || !normalized_execution.starts_with(&normalized_primary)
        {
            return Err(WorkspaceError(
                "execution directory must be inside the primary root".into(),
            ));
        }
        Ok(Self {
            roots,
            primary_root,
            execution_dir,
        })
    }

    /// Compatibility adapter. Relative cwd values retain their existing meaning.
    pub fn from_cwd(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let cwd = if cwd.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            cwd
        };
        Self {
            roots: vec![WorkspaceRoot::external("primary", "primary", cwd.clone())],
            primary_root: WorkspaceRootId::new("primary"),
            execution_dir: cwd,
        }
    }

    pub fn roots(&self) -> &[WorkspaceRoot] {
        &self.roots
    }
    pub fn primary_root_id(&self) -> &WorkspaceRootId {
        &self.primary_root
    }
    pub fn primary_root(&self) -> &WorkspaceRoot {
        self.roots
            .iter()
            .find(|root| root.id == self.primary_root)
            .expect("validated primary root")
    }
    pub fn cwd(&self) -> &Path {
        &self.execution_dir
    }
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot(Arc::new(self.clone()))
    }
}

/// Shared immutable environment description; file contents are not frozen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot(Arc<WorkspaceSpec>);

impl WorkspaceSnapshot {
    pub fn spec(&self) -> &WorkspaceSpec {
        &self.0
    }
    pub fn roots(&self) -> &[WorkspaceRoot] {
        self.0.roots()
    }
    pub fn primary_root(&self) -> &WorkspaceRoot {
        self.0.primary_root()
    }
    pub fn cwd(&self) -> &Path {
        self.0.cwd()
    }
}

impl From<WorkspaceSpec> for WorkspaceSnapshot {
    fn from(value: WorkspaceSpec) -> Self {
        Self(Arc::new(value))
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if result.file_name().is_some_and(|name| name != "..") => {
                result.pop();
            }
            Component::ParentDir if result.has_root() => {}
            component => result.push(component.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_roots_and_execution_relationship_on_deserialization() {
        let root = WorkspaceRoot::external("app", "app", "/work/app");
        let shared = WorkspaceRoot::external("shared", "shared", "/work/shared");
        let spec = WorkspaceSpec::new(vec![root.clone(), shared], root.id.clone(), "/work/app/src")
            .unwrap();
        assert_eq!(
            serde_json::from_value::<WorkspaceSpec>(serde_json::to_value(&spec).unwrap()).unwrap(),
            spec
        );
        assert!(WorkspaceSpec::new(vec![], root.id.clone(), "/work/app").is_err());
        assert!(
            WorkspaceSpec::new(
                vec![root.clone(), root.clone()],
                root.id.clone(),
                "/work/app"
            )
            .is_err()
        );
        assert!(
            WorkspaceSpec::new(
                vec![root.clone()],
                WorkspaceRootId::new("missing"),
                "/work/app"
            )
            .is_err()
        );
        for path in ["/work/app/../shared", "/work/application", "/elsewhere"] {
            assert!(WorkspaceSpec::new(vec![root.clone()], root.id.clone(), path).is_err());
        }
        let mut wire = serde_json::to_value(spec).unwrap();
        wire["roots"] = serde_json::json!([]);
        assert!(serde_json::from_value::<WorkspaceSpec>(wire).is_err());
    }

    #[test]
    fn single_root_adapter_preserves_cwd_and_snapshot() {
        for cwd in [".", "../repo", "/work/app"] {
            let spec = WorkspaceSpec::from_cwd(cwd);
            assert_eq!(spec.cwd(), Path::new(cwd));
            assert_eq!(spec.snapshot().spec(), &spec);
            assert_eq!(
                serde_json::from_value::<WorkspaceSpec>(serde_json::to_value(&spec).unwrap())
                    .unwrap(),
                spec
            );
        }
    }
}
