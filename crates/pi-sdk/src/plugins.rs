//! Read-only Native package inspection and explicit management operations.
//! Deliberate Rust/Desktop extension, not Pi's npm/git extension manager.
use std::path::PathBuf;

use pi_plugin_manager::{InstallScope, PluginManager, PluginManagerOptions, PluginPackageRow};
use serde::{Deserialize, Serialize};

use crate::{ProjectTrustEvaluation, ProjectTrustService};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLibrarySnapshot {
    pub scope: &'static str,
    pub path: PathBuf,
    pub lock_path: PathBuf,
    pub target: String,
    pub writable: bool,
    pub project_trusted: bool,
    pub plugins: Vec<PluginPackageRow>,
    pub intent_current: Option<bool>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PluginOperation {
    Install {
        source: String,
        version: Option<String>,
        registry: Option<String>,
    },
    Sync {
        registry: Option<String>,
    },
    Remove {
        id: String,
    },
}

/// Owns trust checks; no cached authorization, runtime, session or native loader.
#[derive(Clone)]
pub struct PluginLibrary {
    agent_dir: PathBuf,
    cwd: Option<PathBuf>,
    trust: ProjectTrustService,
}

impl PluginLibrary {
    pub fn new(agent_dir: PathBuf, cwd: Option<PathBuf>, trust: ProjectTrustService) -> Self {
        Self {
            agent_dir,
            cwd,
            trust,
        }
    }

    fn trusted(&self) -> Result<bool, String> {
        match &self.cwd {
            Some(cwd) => self
                .trust
                .evaluate_resource_access(cwd)
                .map(|decision| matches!(decision, ProjectTrustEvaluation::Known(true)))
                .map_err(|error| error.to_string()),
            None => Ok(false),
        }
    }

    fn scope(&self) -> InstallScope {
        if self.cwd.is_some() {
            InstallScope::Project
        } else {
            InstallScope::Global
        }
    }

    fn manager(&self, registry: Option<String>) -> Result<PluginManager, String> {
        let mut options = PluginManagerOptions::new(
            self.cwd.as_ref().unwrap_or(&self.agent_dir),
            &self.agent_dir,
        );
        options.registry = registry
            .filter(|value| !value.trim().is_empty())
            .or_else(|| std::env::var("PI_PLUGIN_REGISTRY").ok());
        PluginManager::new(options).map_err(|error| error.to_string())
    }

    fn read_blocking(&self) -> Result<PluginLibrarySnapshot, String> {
        let trusted = self.trusted()?;
        let project = self.cwd.is_some();
        let base = self
            .cwd
            .as_ref()
            .map(|cwd| cwd.join(".pi"))
            .unwrap_or_else(|| self.agent_dir.clone());
        let mut result = PluginLibrarySnapshot {
            scope: if project { "project" } else { "global" },
            path: base.join("plugins.json"),
            lock_path: base.join("plugins.lock"),
            target: pi_plugin_manager::HOST_TARGET.into(),
            writable: !project || trusted,
            project_trusted: trusted,
            plugins: Vec::new(),
            intent_current: None,
            diagnostics: Vec::new(),
        };
        if project && !trusted {
            result
                .diagnostics
                .push("Project plugin configuration requires project trust".into());
            return Ok(result);
        }
        let snapshot = self.manager(None)?.snapshot(self.scope());
        result.plugins = snapshot.plugins;
        result.intent_current = snapshot.intent_current;
        result.diagnostics = snapshot.diagnostics;
        Ok(result)
    }

    pub async fn read(&self) -> Result<PluginLibrarySnapshot, String> {
        let library = self.clone();
        tokio::task::spawn_blocking(move || library.read_blocking())
            .await
            .map_err(|error| error.to_string())?
    }

    pub async fn operate(
        &self,
        operation: PluginOperation,
    ) -> Result<PluginLibrarySnapshot, String> {
        let library = self.clone();
        let runtime = tokio::runtime::Handle::current();
        // The manager's synchronous filesystem work and cross-process locks never run
        // on a Tokio worker. Keep the operation alive to completion if the IPC caller leaves.
        tokio::task::spawn_blocking(move || {
            if library.cwd.is_some() && !library.trusted()? {
                return Err("Project plugin configuration requires project trust".into());
            }
            let scope = library.scope();
            match operation {
                PluginOperation::Install {
                    source,
                    version,
                    registry,
                } => {
                    let source = source.trim();
                    if source.is_empty() {
                        return Err("Plugin source is required".into());
                    }
                    let manager = library.manager(registry)?;
                    runtime
                        .block_on(manager.install(source, version.as_deref(), scope))
                        .map_err(|error| error.to_string())?;
                }
                PluginOperation::Sync { registry } => {
                    runtime
                        .block_on(library.manager(registry)?.sync(scope))
                        .map_err(|error| error.to_string())?;
                }
                PluginOperation::Remove { id } => {
                    library
                        .manager(None)?
                        .remove(&id, scope)
                        .map_err(|error| error.to_string())?;
                }
            }
            library.read_blocking()
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_settings::DefaultProjectTrust;

    fn library(agent: &std::path::Path, project: Option<PathBuf>) -> PluginLibrary {
        let (trust, _) =
            ProjectTrustService::new(agent, None, false, DefaultProjectTrust::Ask).unwrap();
        PluginLibrary::new(agent.into(), project, trust)
    }

    #[tokio::test]
    async fn inspection_does_not_create_state_or_grant_empty_project_trust() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("absent-agent");
        let global = library(&agent, None).read().await.unwrap();
        assert!(global.plugins.is_empty());
        assert!(global.writable);
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let library = library(&agent, Some(project.clone()));
        let view = library.read().await.unwrap();
        assert!(!view.writable);
        assert!(!view.project_trusted);
        assert!(
            library
                .operate(PluginOperation::Install {
                    source: "https://invalid.example/release.json".into(),
                    version: None,
                    registry: None,
                })
                .await
                .unwrap_err()
                .contains("requires project trust")
        );
        assert!(!agent.exists());
        assert!(!project.join(".pi").exists());
    }

    #[tokio::test]
    async fn preparing_empty_project_does_not_authorize_plugin_installation() {
        for default in [DefaultProjectTrust::Ask, DefaultProjectTrust::Never] {
            let dir = tempfile::tempdir().unwrap();
            let agent = dir.path().join("agent");
            let project = dir.path().join("project");
            std::fs::create_dir(&project).unwrap();
            let (trust, _) = ProjectTrustService::new(&agent, None, false, default).unwrap();
            assert!(trust.resolve(&project).await.unwrap());
            let library = PluginLibrary::new(agent.clone(), Some(project.clone()), trust.clone());
            assert!(!library.read().await.unwrap().writable);
            assert!(
                library
                    .operate(PluginOperation::Sync { registry: None })
                    .await
                    .unwrap_err()
                    .contains("requires project trust")
            );
            assert!(!agent.exists());
            assert!(!project.join(".pi").exists());
            // An explicit session-only approval remains authoritative.
            trust.remember(&project, true).unwrap();
            assert!(library.read().await.unwrap().writable);
            assert!(!agent.exists());
        }
    }

    #[tokio::test]
    async fn empty_project_preparation_does_not_override_saved_ancestor_denial() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let parent = dir.path().join("parent");
        let project = parent.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir(&agent).unwrap();
        let parent = std::fs::canonicalize(parent).unwrap();
        std::fs::write(
            agent.join("trust.json"),
            serde_json::json!({ parent.to_str().unwrap(): false }).to_string(),
        )
        .unwrap();
        let (trust, _) =
            ProjectTrustService::new(&agent, None, false, DefaultProjectTrust::Always).unwrap();
        assert!(trust.resolve(&project).await.unwrap());
        let library = PluginLibrary::new(agent.clone(), Some(project.clone()), trust);
        assert!(!library.read().await.unwrap().project_trusted);
        assert!(
            library
                .operate(PluginOperation::Sync { registry: None })
                .await
                .is_err()
        );
        assert!(!project.join(".pi").exists());
        assert!(!agent.join("trust.json.lock").exists());
    }

    #[tokio::test]
    async fn project_access_rechecks_nearest_ancestor_trust_and_rejects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("parent/project");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(project.join(".pi")).unwrap();
        // Untrusted project bytes must never be parsed.
        std::fs::write(project.join(".pi/plugins.json"), "not json").unwrap();
        let library = library(&agent, Some(project.clone()));
        let parent = std::fs::canonicalize(project.parent().unwrap()).unwrap();
        let project = std::fs::canonicalize(project).unwrap();
        let trust_path = agent.join("trust.json");
        std::fs::write(
            &trust_path,
            serde_json::json!({ parent.to_str().unwrap(): true }).to_string(),
        )
        .unwrap();
        let view = library.read().await.unwrap();
        assert!(view.writable);
        assert!(view.diagnostics[0].contains("invalid plugin"));
        std::fs::write(
            &trust_path,
            serde_json::json!({ parent.to_str().unwrap(): true, project.to_str().unwrap(): false })
                .to_string(),
        )
        .unwrap();
        let view = library.read().await.unwrap();
        assert!(!view.writable);
        assert!(view.diagnostics[0].contains("requires project trust"));
        std::fs::write(&trust_path, "invalid").unwrap();
        assert!(
            library
                .read()
                .await
                .unwrap_err()
                .contains("invalid trust store")
        );
        assert!(!agent.join("trust.json.lock").exists());
    }

    #[tokio::test]
    async fn operations_install_sync_remove_without_loading_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("package");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("libfixture.so"), b"not executable native code").unwrap();
        std::fs::write(package.join("pi-plugin.toml"), "schema = 1\n[plugin]\nid = \"fixture\"\nversion = \"1.0.0\"\nkind = \"agent\"\nartifact = \"libfixture.so\"\n").unwrap();
        let library = library(&dir.path().join("agent"), None);
        let installed = library
            .operate(PluginOperation::Install {
                source: package.to_string_lossy().into_owned(),
                version: None,
                registry: None,
            })
            .await
            .unwrap();
        assert_eq!(installed.plugins.len(), 1);
        assert_eq!(
            installed.plugins[0].installed.as_ref().unwrap().version,
            "1.0.0"
        );
        assert_eq!(installed.intent_current, Some(true));
        let synced = library
            .operate(PluginOperation::Sync { registry: None })
            .await
            .unwrap();
        assert_eq!(synced.plugins[0].installed, installed.plugins[0].installed);
        let removed = library
            .operate(PluginOperation::Remove {
                id: "fixture".into(),
            })
            .await
            .unwrap();
        assert!(removed.plugins.is_empty());
    }
}
