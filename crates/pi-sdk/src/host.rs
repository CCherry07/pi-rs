use std::path::Path;
use std::sync::Arc;

use pi_js_plugin::JsPluginHost;
use pi_plugin::PresentationMode;
use pi_session::{MultiSessionManager, PluginContextBinding, PluginUiBridge};
use pi_settings::{SettingsContext, SettingsManager};
use tokio::sync::mpsc;

use crate::{Config, ProductSessionFactory, ProjectTrustPromptRequest, ProjectTrustService};

/// Headless Pi application host shared by presentation adapters.
pub struct Pi {
    sessions: MultiSessionManager,
    agent_dir: std::path::PathBuf,
    project_trust: ProjectTrustService,
    trust_requests: mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
}

pub struct PiBuilder {
    config: Config,
    presentation_mode: PresentationMode,
    interactive_project_trust: bool,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
    plugin_ui_bridge: Option<Arc<dyn PluginUiBridge>>,
}

impl Pi {
    pub fn builder(config: Config) -> PiBuilder {
        PiBuilder {
            config,
            presentation_mode: PresentationMode::Print,
            interactive_project_trust: false,
            js_plugin_host: None,
            plugin_ui_bridge: None,
        }
    }

    pub fn sessions(&self) -> &MultiSessionManager {
        &self.sessions
    }

    pub fn session_manager(&self) -> MultiSessionManager {
        self.sessions.clone()
    }

    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    pub fn projects(&self) -> crate::projects::ProjectStore {
        crate::projects::ProjectStore::new(&self.agent_dir)
    }

    pub async fn create_project_session(
        &self,
        project_id: &str,
    ) -> Result<pi_session::PiSession, String> {
        let project = self.projects().get(project_id)?;
        let workspace = project.resolve()?;
        let id = uuid::Uuid::now_v7().to_string();
        let path = match pi_session::JsonlSessionRepo::new(self.agent_dir.join("sessions"))
            .resolve_exact_id(workspace.cwd(), &id)
            .map_err(|error| error.to_string())?
        {
            pi_session::ExactSessionIdResolution::New { path, .. } => path,
            _ => return Err("new session identity already exists".into()),
        };
        self.sessions
            .create_session_with_workspace(
                workspace,
                path,
                Some(id),
                Some(project.session_metadata()),
            )
            .await
            .map_err(|error| error.to_string())
    }

    pub fn project_trust(&self) -> &ProjectTrustService {
        &self.project_trust
    }

    pub async fn next_project_trust_prompt(&mut self) -> Option<ProjectTrustPromptRequest> {
        self.trust_requests.recv().await
    }
}

impl PiBuilder {
    pub fn presentation_mode(mut self, mode: PresentationMode) -> Self {
        self.presentation_mode = mode;
        self
    }

    pub fn interactive_project_trust(mut self, interactive: bool) -> Self {
        self.interactive_project_trust = interactive;
        self
    }

    pub fn js_plugin_host(mut self, host: Arc<dyn JsPluginHost>) -> Self {
        self.js_plugin_host = Some(host);
        self
    }

    pub fn plugin_ui_bridge(mut self, bridge: Arc<dyn PluginUiBridge>) -> Self {
        self.plugin_ui_bridge = Some(bridge);
        self
    }

    pub fn build(self) -> Result<Pi, String> {
        let settings = SettingsManager::new(&self.config.agent_dir);
        let default_trust = settings
            .load(&SettingsContext::new(&self.config.cwd, false))
            .default_project_trust();
        let (project_trust, trust_requests) = ProjectTrustService::new(
            &self.config.agent_dir,
            self.config.trust_override,
            self.interactive_project_trust,
            default_trust,
        )
        .map_err(|error| error.to_string())?;
        let agent_dir = self.config.agent_dir.clone();
        let mut factory = ProductSessionFactory::new(self.config, project_trust.clone(), settings)
            .with_plugin_context(self.presentation_mode, PluginContextBinding::new());
        if let Some(host) = self.js_plugin_host {
            factory = factory.with_js_plugin_host(host);
        }
        if let Some(bridge) = self.plugin_ui_bridge {
            factory = factory.with_plugin_ui_bridge(bridge);
        }
        Ok(Pi {
            sessions: MultiSessionManager::new(factory),
            agent_dir,
            project_trust,
            trust_requests,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(cwd: &Path, agent_dir: &Path) -> Config {
        std::fs::create_dir_all(agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("memory.json"),
            r#"{"version": 1, "enabled": false}"#,
        )
        .unwrap();
        let mut config = Config::new(cwd.to_path_buf(), agent_dir.to_path_buf());
        config.discover_extensions = false;
        config
    }

    #[tokio::test]
    async fn host_creates_independent_unsaved_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let host = Pi::builder(config(directory.path(), &agent_dir))
            .presentation_mode(PresentationMode::Rpc)
            .build()
            .unwrap();
        let first_path = directory.path().join("first.jsonl");
        let first = host
            .sessions()
            .create_session(directory.path(), &first_path)
            .await
            .unwrap();
        let second_path = directory.path().join("second.jsonl");
        let second = host
            .sessions()
            .create_session(directory.path(), &second_path)
            .await
            .unwrap();

        assert_ne!(first.id(), second.id());
        assert!(!first_path.exists());
        assert!(!second_path.exists());
        first.reload().await.unwrap();
        assert!(!first_path.exists());
        host.sessions().shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn host_exposes_project_trust_before_loading_project_skills() {
        let directory = tempfile::tempdir().unwrap();
        let skill = directory.path().join(".pi/skills/desktop-probe");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: desktop-probe\ndescription: Test resource\n---\nProbe",
        )
        .unwrap();
        let agent_dir = directory.path().join("agent");
        let mut host = Pi::builder(config(directory.path(), &agent_dir))
            .presentation_mode(PresentationMode::Rpc)
            .interactive_project_trust(true)
            .build()
            .unwrap();
        let sessions = host.session_manager();
        let cwd = directory.path().to_path_buf();
        let session =
            tokio::spawn(
                async move { sessions.create_session(&cwd, cwd.join("task.jsonl")).await },
            );
        let prompt = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            host.next_project_trust_prompt(),
        )
        .await
        .unwrap()
        .unwrap();
        let deny = prompt
            .options
            .iter()
            .position(|option| option.label == "Do not trust")
            .unwrap();
        prompt.response.send(Some(deny)).unwrap();
        let handle = session.await.unwrap().unwrap();

        assert!(
            !handle
                .current()
                .runtime()
                .command_specs()
                .iter()
                .any(|command| command.name.contains("desktop-probe"))
        );
        assert!(!handle.path().exists());
        host.sessions().shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn project_edits_apply_only_to_new_sessions_and_resume_needs_no_project_record() {
        let root = tempfile::tempdir().unwrap();
        let primary = root.path().join("primary");
        let shared = root.path().join("shared");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(shared.join(".pi/skills/shared-only")).unwrap();
        std::fs::write(
            shared.join(".pi/skills/shared-only/SKILL.md"),
            "---\nname: shared-only\ndescription: supplemental root resource\n---\nDo not merge",
        )
        .unwrap();
        let host = Pi::builder(config(&primary, &root.path().join("agent")))
            .build()
            .unwrap();
        let mut project = crate::projects::Project::single_root("project-id", "project", &primary);
        project.roots.push(pi_core::WorkspaceRoot::external(
            "shared", "shared", &shared,
        ));
        host.projects().upsert(project.clone()).unwrap();
        let first = host.create_project_session(&project.id).await.unwrap();
        let first_spec = first.current().runtime().workspace().spec().clone();
        assert_eq!(first_spec.roots().len(), 2);
        assert!(!first.path().exists());
        assert!(
            !first
                .current()
                .runtime()
                .command_specs()
                .iter()
                .any(|command| command.name.contains("shared-only"))
        );
        project.roots.pop();
        host.projects().upsert(project.clone()).unwrap();
        first.reload().await.unwrap();
        assert_eq!(first.current().runtime().workspace().spec(), &first_spec);
        let second = host.create_project_session(&project.id).await.unwrap();
        assert_eq!(second.current().runtime().workspace().roots().len(), 1);
        first.current().log().materialize().unwrap();
        let path = first.path();
        host.sessions().close_session(&first).await.unwrap();
        host.projects().remove(&project.id).unwrap();
        let resumed = host.sessions().open_session(path).await.unwrap();
        assert_eq!(resumed.current().runtime().workspace().spec(), &first_spec);
        host.sessions().shutdown().await.unwrap();
    }
}
