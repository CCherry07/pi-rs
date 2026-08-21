use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId};
use pi_plugin_bash::BashPlugin;
use pi_plugin_edit::EditPlugin;
use pi_plugin_find::FindPlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_hashline_edit::HashlineEditPlugin;
use pi_plugin_loader::{NativePluginLoader, NativePluginLoaderOptions, NativePlugins};
use pi_plugin_ls::LsPlugin;
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiCompatiblePlugin};
use pi_plugin_read::ReadPlugin;
use pi_plugin_skills::{SkillLoaderOptions, SkillsPlugin};
use pi_plugin_write::WritePlugin;
use pi_resources::ResourceLoaderOptions;
use pi_runtime::{PiRuntime, RuntimeError, SystemPrompt};
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
    AgentSessionRuntimeTarget, InitialModelRequest, ModelRuntimeServices, PreparedAgentSession,
    SessionError, SessionPlugins,
};

use crate::config::AppConfig;
use crate::project_trust::ProjectTrustService;

#[derive(Clone)]
pub struct AppSessionFactory {
    config: AppConfig,
    project_trust: ProjectTrustService,
}

impl AppSessionFactory {
    pub fn new(config: AppConfig, project_trust: ProjectTrustService) -> Self {
        Self {
            config,
            project_trust,
        }
    }
}

#[async_trait]
impl AgentSessionRuntimeFactory for AppSessionFactory {
    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError> {
        let (path, create, cwd, reused_log) = match request.target {
            AgentSessionRuntimeTarget::Create { cwd, path } => (path, true, cwd, None),
            AgentSessionRuntimeTarget::Open { path } => {
                let (_, document) = pi_session::SessionLog::open(&path)?;
                (path, false, document.header.cwd, None)
            }
            AgentSessionRuntimeTarget::Reuse { log } => {
                let document = log.load()?;
                (
                    log.path().to_path_buf(),
                    false,
                    document.header.cwd,
                    Some(log),
                )
            }
        };
        let mut config = self.config.clone();
        config.cwd = cwd;
        config.trust_project = self
            .project_trust
            .resolve(&config.cwd)
            .await
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let mut native_options = NativePluginLoaderOptions::new(&config.cwd, &config.agent_dir);
        native_options.project_trusted = config.trust_project;
        native_options.explicit_paths = config.native_plugins.clone();
        let native_plugins = NativePluginLoader::new(native_options)
            .discover()
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let runtime = build_runtime(&config, &native_plugins)?;
        let session_options = AgentSessionOptions::default()
            .plugins(native_plugins.apply_session(SessionPlugins::new()))
            .initial_model(initial_model_request(&config));
        if create {
            AgentSession::prepare_create_with_options(runtime, path, session_options).await
        } else if let Some(log) = reused_log {
            AgentSession::prepare_reuse_with_options(runtime, log, session_options).await
        } else {
            AgentSession::prepare_open_with_options(runtime, path, session_options).await
        }
    }
}

fn build_runtime(
    config: &AppConfig,
    native_plugins: &NativePlugins,
) -> Result<PiRuntime, RuntimeError> {
    let provider_config = config
        .api_key
        .as_ref()
        .map_or_else(
            || OpenAiCompatibleConfig::without_api_key(&config.base_url),
            |api_key| OpenAiCompatibleConfig::new(&config.base_url, api_key),
        )
        .provider_id(config.provider.clone());
    let mut skill_options = SkillLoaderOptions::new(&config.cwd, &config.agent_dir);
    skill_options.project_trusted = config.trust_project;
    if let Some(home) = std::env::var_os("HOME") {
        skill_options
            .additional_paths
            .push(std::path::PathBuf::from(home).join(".agents/skills"));
    }
    let mut model_options = ModelsPluginOptions::for_agent_dir(&config.agent_dir);
    if let Some(api_key) = &config.api_key {
        model_options = model_options.runtime_api_key(config.provider.clone(), api_key.clone());
    }

    let builder = PiRuntime::builder()
        .try_provider_plugin_factory({
            let provider_config = provider_config.clone();
            move || OpenAiCompatiblePlugin::new(provider_config.clone())
        })
        .try_provider_plugin_factory({
            let model_options = model_options.clone();
            move || ModelsPlugin::load(model_options.clone())
        })
        .agent_plugin_factory({
            let skill_options = skill_options.clone();
            move || SkillsPlugin::load(skill_options.clone())
        })
        .agent_plugin_factory(|| ReadPlugin)
        .agent_plugin_factory(|| GrepPlugin)
        .agent_plugin_factory(|| FindPlugin)
        .agent_plugin_factory(|| LsPlugin)
        .agent_plugin_factory(|| WritePlugin)
        .agent_plugin_factory(|| EditPlugin)
        .agent_plugin_factory(|| HashlineEditPlugin)
        .agent_plugin_factory(|| BashPlugin);
    let builder = native_plugins.apply_runtime(builder);

    let mut resources = ResourceLoaderOptions::new(&config.cwd, &config.agent_dir);
    resources.project_trusted = config.trust_project;

    let runtime = builder
        .agent_options(AgentOptions {
            provider_id: ProviderId::new(config.provider.clone()),
            model_id: ModelId::new(config.model.as_deref().unwrap_or(&config.fallback_model)),
            active_tools: vec![
                "read".into(),
                "grep".into(),
                "find".into(),
                "ls".into(),
                "write".into(),
                "edit".into(),
                "hashline_edit".into(),
                "bash".into(),
            ],
            cwd: config.cwd.clone(),
            max_tool_iterations: 50,
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Pi(Box::default()))
        .resources(resources)
        .build()?;

    let mut active_tools = runtime.active_tools();
    let mut seen = active_tools
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for spec in runtime.tool_specs() {
        if seen.insert(spec.name.clone()) {
            active_tools.push(spec.name);
        }
    }
    runtime.set_active_tools(active_tools)?;

    ModelRuntimeServices::new(&runtime)
        .select_initial_model(initial_model_request(config))
        .map_err(|error| RuntimeError::Build(error.to_string()))?;

    Ok(runtime)
}

fn initial_model_request(config: &AppConfig) -> InitialModelRequest {
    config
        .model
        .as_ref()
        .map_or_else(InitialModelRequest::default, |model| InitialModelRequest {
            requested_provider: config.requested_provider.clone().map(ProviderId::new),
            requested_model: Some(model.clone()),
            session_model: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_config(agent_dir: &std::path::Path, model: Option<&str>) -> AppConfig {
        AppConfig {
            cwd: agent_dir.to_path_buf(),
            agent_dir: agent_dir.to_path_buf(),
            session_path: agent_dir.join("session.jsonl"),
            model: model.map(str::to_string),
            fallback_model: "gpt-4o-mini".to_string(),
            base_url: "https://fallback.example/v1".to_string(),
            api_key: None,
            provider: "openai-compatible".to_string(),
            requested_provider: None,
            trust_project: true,
            trust_override: None,
            native_plugins: Vec::new(),
        }
    }

    #[test]
    fn models_json_catalog_owns_the_initial_provider_and_model() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("models.json"),
            r#"{
              "providers": {
                "catalog-provider": {
                  "baseUrl": "https://catalog.example/v1",
                  "api": "openai-completions",
                  "models": [
                    { "id": "catalog-first", "name": "Catalog First" },
                    { "id": "catalog-requested", "name": "Catalog Requested" }
                  ]
                }
              }
            }"#,
        )
        .unwrap();

        let runtime = build_runtime(
            &app_config(directory.path(), None),
            &NativePlugins::default(),
        )
        .unwrap();
        let state = runtime.agent().state();

        assert_eq!(state.provider_id.as_str(), "catalog-provider");
        assert_eq!(state.model_id.as_str(), "catalog-first");
    }

    #[test]
    fn requested_model_id_resolves_to_its_models_json_provider() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("models.json"),
            r#"{
              "providers": {
                "catalog-provider": {
                  "baseUrl": "https://catalog.example/v1",
                  "api": "openai-completions",
                  "models": [
                    { "id": "catalog-first" },
                    { "id": "catalog-requested" }
                  ]
                }
              }
            }"#,
        )
        .unwrap();

        let runtime = build_runtime(
            &app_config(directory.path(), Some("catalog-requested")),
            &NativePlugins::default(),
        )
        .unwrap();
        let state = runtime.agent().state();

        assert_eq!(state.provider_id.as_str(), "catalog-provider");
        assert_eq!(state.model_id.as_str(), "catalog-requested");
    }

    #[test]
    fn missing_models_json_keeps_the_cli_fallback() {
        let directory = tempfile::tempdir().unwrap();

        let runtime = build_runtime(
            &app_config(directory.path(), None),
            &NativePlugins::default(),
        )
        .unwrap();
        let state = runtime.agent().state();

        assert_eq!(state.provider_id.as_str(), "openai-compatible");
        assert_eq!(state.model_id.as_str(), "gpt-4o-mini");
    }

    #[tokio::test]
    async fn explicit_model_wins_over_the_model_saved_in_a_resumed_session() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("models.json"),
            r#"{
              "providers": {
                "catalog-provider": {
                  "baseUrl": "https://catalog.example/v1",
                  "api": "openai-completions",
                  "models": [
                    { "id": "alpha" },
                    { "id": "beta" }
                  ]
                }
              }
            }"#,
        )
        .unwrap();
        let path = directory.path().join("resume.jsonl");

        let original = AgentSession::create(
            build_runtime(
                &app_config(directory.path(), None),
                &NativePlugins::default(),
            )
            .unwrap(),
            &path,
        )
        .await
        .unwrap();
        original
            .set_model(ProviderId::new("catalog-provider"), ModelId::new("beta"))
            .unwrap();
        original.log().materialize().unwrap();
        original.shutdown().await;

        let config = app_config(directory.path(), Some("alpha"));
        let resumed = AgentSession::open_with_options(
            build_runtime(&config, &NativePlugins::default()).unwrap(),
            &path,
            AgentSessionOptions::default().initial_model(initial_model_request(&config)),
        )
        .await
        .unwrap();
        let state = resumed.runtime().agent().state();

        assert_eq!(state.provider_id.as_str(), "catalog-provider");
        assert_eq!(state.model_id.as_str(), "alpha");
        resumed.shutdown().await;
    }
}
