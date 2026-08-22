use std::sync::Arc;

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId};
use pi_js_plugin::{JsGenerationRequest, JsHostMode, JsPluginGeneration, JsPluginHost};
use pi_plugin_bash::BashPlugin;
use pi_plugin_edit::EditPlugin;
use pi_plugin_find::FindPlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_hashline_edit::HashlineEditPlugin;
use pi_plugin_loader::{NativePluginLoader, NativePluginLoaderOptions, NativePlugins};
use pi_plugin_ls::LsPlugin;
use pi_plugin_manager::{
    InstallScope, PluginManager, PluginManagerOptions, PreparedPluginReconcile,
};
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
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
    js_host_mode: JsHostMode,
}

impl AppSessionFactory {
    pub fn new(config: AppConfig, project_trust: ProjectTrustService) -> Self {
        Self {
            config,
            project_trust,
            js_plugin_host: None,
            js_host_mode: JsHostMode::Print,
        }
    }

    pub fn with_js_plugin_host(mut self, host: Arc<dyn JsPluginHost>, mode: JsHostMode) -> Self {
        self.js_plugin_host = Some(host);
        self.js_host_mode = mode;
        self
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
        let package_reconciliations = prepare_native_packages(&config).await?;
        let mut native_options = NativePluginLoaderOptions::new(&config.cwd, &config.agent_dir);
        native_options.project_trusted = config.trust_project;
        native_options.explicit_paths = config.native_plugins.clone();
        let native_plugins = NativePluginLoader::new(native_options)
            .discover()
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let js_generation = if let Some(host) = &self.js_plugin_host {
            let manifest = host
                .prepare_generation(JsGenerationRequest {
                    cwd: config.cwd.clone(),
                    agent_dir: config.agent_dir.clone(),
                    project_trusted: config.trust_project,
                    explicit_paths: config.extensions.clone(),
                    discover_extensions: config.discover_extensions,
                    mode: self.js_host_mode,
                })
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            Some(
                JsPluginGeneration::prepare_with_host(manifest, Arc::clone(host))
                    .map_err(|error| SessionError::Runtime(error.to_string()))?,
            )
        } else {
            None
        };
        let runtime = build_runtime(&config, &native_plugins, js_generation.as_ref())?;
        let mut session_plugins = native_plugins.apply_session(SessionPlugins::new());
        if let Some(js_generation) = &js_generation {
            for plugin in js_generation.session_plugins() {
                session_plugins = session_plugins.try_plugin_arc_factory({
                    let plugin = Arc::clone(&plugin);
                    move || Ok::<_, String>(Arc::clone(&plugin))
                });
            }
        }
        let session_options = AgentSessionOptions::default()
            .plugins(session_plugins)
            .initial_model(initial_model_request(&config));
        let prepared = if create {
            AgentSession::prepare_create_with_options(runtime, path, session_options).await
        } else if let Some(log) = reused_log {
            AgentSession::prepare_reuse_with_options(runtime, log, session_options).await
        } else {
            AgentSession::prepare_open_with_options(runtime, path, session_options).await
        };
        match prepared {
            Ok(prepared) => {
                for reconciliation in package_reconciliations {
                    reconciliation.commit();
                }
                Ok(prepared)
            }
            Err(error) => {
                let mut rollback_errors = Vec::new();
                for reconciliation in package_reconciliations.into_iter().rev() {
                    if let Err(rollback_error) = reconciliation.rollback() {
                        rollback_errors.push(rollback_error.to_string());
                    }
                }
                if rollback_errors.is_empty() {
                    Err(error)
                } else {
                    Err(SessionError::Runtime(format!(
                        "{error}; native package rollback failed: {}",
                        rollback_errors.join("; ")
                    )))
                }
            }
        }
    }
}

async fn prepare_native_packages(
    config: &AppConfig,
) -> Result<Vec<PreparedPluginReconcile>, SessionError> {
    let mut options = PluginManagerOptions::new(&config.cwd, &config.agent_dir);
    options.registry = std::env::var("PI_PLUGIN_REGISTRY")
        .ok()
        .filter(|registry| !registry.trim().is_empty());
    let manager =
        PluginManager::new(options).map_err(|error| SessionError::Runtime(error.to_string()))?;
    let mut prepared = vec![
        manager
            .prepare_reconcile(InstallScope::Global)
            .await
            .map_err(|error| SessionError::Runtime(error.to_string()))?,
    ];
    if config.trust_project {
        prepared.push(
            manager
                .prepare_reconcile(InstallScope::Project)
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))?,
        );
    }
    Ok(prepared)
}

fn build_runtime(
    config: &AppConfig,
    native_plugins: &NativePlugins,
    js_generation: Option<&JsPluginGeneration>,
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
    let mut builder = native_plugins.apply_runtime(builder);
    if let Some(js_generation) = js_generation {
        for plugin in js_generation.agent_plugins() {
            builder = builder.try_agent_plugin_arc_factory({
                let plugin = Arc::clone(&plugin);
                move || Ok::<_, String>(Arc::clone(&plugin))
            });
        }
        for plugin in js_generation.provider_plugins() {
            builder = builder.try_provider_plugin_arc_factory({
                let plugin = Arc::clone(&plugin);
                move || Ok::<_, String>(Arc::clone(&plugin))
            });
        }
    }

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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct RecordingJsHost {
        generation: AtomicUsize,
        requests: Mutex<Vec<JsGenerationRequest>>,
        retired: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl pi_js_plugin::JsCallbackDispatcher for RecordingJsHost {
        async fn invoke(
            &self,
            _invocation: pi_js_plugin::JsInvocation,
        ) -> Result<serde_json::Value, pi_js_plugin::JsCallbackError> {
            Ok(serde_json::json!({ "action": "continue" }))
        }

        fn retire_generation(&self, generation_id: &str) {
            self.retired.lock().unwrap().push(generation_id.to_string());
        }
    }

    #[async_trait]
    impl JsPluginHost for RecordingJsHost {
        async fn prepare_generation(
            &self,
            request: JsGenerationRequest,
        ) -> Result<pi_js_plugin::JsGenerationManifest, pi_js_plugin::JsCallbackError> {
            self.requests.lock().unwrap().push(request);
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(pi_js_plugin::JsGenerationManifest {
                generation_id: format!("js-{generation}"),
                agent_plugins: vec![pi_js_plugin::JsAgentPluginManifest {
                    id: "reload-fixture".to_string(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    hooks: vec![pi_js_plugin::JsHookManifest {
                        name: "input".to_string(),
                        callback_id: format!("input-{generation}"),
                    }],
                }],
                provider_plugins: Vec::new(),
                session_plugins: Vec::new(),
            })
        }
    }

    fn write_local_plugin_package(root: &std::path::Path) -> std::path::PathBuf {
        let package = root.join("local-plugin");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("plugin.dylib"), b"native plugin fixture").unwrap();
        std::fs::write(
            package.join("pi-plugin.toml"),
            r#"schema = 1

[plugin]
id = "local-plugin"
version = "1.0.0"
kind = "agent"
artifact = "plugin.dylib"

[options]
command = "fixture-command"
"#,
        )
        .unwrap();
        package
    }

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
            extensions: Vec::new(),
            discover_extensions: true,
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
            None,
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
            None,
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
            None,
        )
        .unwrap();
        let state = runtime.agent().state();

        assert_eq!(state.provider_id.as_str(), "openai-compatible");
        assert_eq!(state.model_id.as_str(), "gpt-4o-mini");
    }

    #[tokio::test]
    async fn trusted_project_packages_prepare_transactionally_before_runtime_loading() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let agent_dir = root.path().join("agent");
        let package = write_local_plugin_package(root.path());
        std::fs::create_dir_all(project.join(".pi")).unwrap();
        std::fs::write(
            project.join(".pi/plugins.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1,
                "plugins": [{
                    "id": "local-plugin",
                    "source": package.display().to_string(),
                    "version": "*",
                    "options": {"command": "project-command"}
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut config = app_config(&agent_dir, None);
        config.cwd = project.clone();
        config.trust_project = true;
        let activation = project.join(".pi/plugins/installed/0000-local-plugin");
        let lock_path = project.join(".pi/plugins.lock");

        {
            let prepared = prepare_native_packages(&config).await.unwrap();
            assert!(activation.join("plugin.dylib").is_file());
            assert!(lock_path.is_file());
            let manifest = std::fs::read_to_string(activation.join("pi-plugin.toml")).unwrap();
            assert!(manifest.contains("command = \"project-command\""));
            drop(prepared);
        }
        assert!(!activation.exists());
        assert!(!lock_path.exists());

        for reconciliation in prepare_native_packages(&config).await.unwrap() {
            reconciliation.commit();
        }
        assert!(activation.join("plugin.dylib").is_file());
        let lock: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lock_path).unwrap()).unwrap();
        assert!(lock["intent_sha256"].as_str().is_some());
    }

    #[tokio::test]
    async fn untrusted_project_package_intent_is_not_reconciled() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let agent_dir = root.path().join("agent");
        let package = write_local_plugin_package(root.path());
        std::fs::create_dir_all(project.join(".pi")).unwrap();
        std::fs::write(
            project.join(".pi/plugins.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1,
                "plugins": [{
                    "id": "local-plugin",
                    "source": package.display().to_string(),
                    "version": "*",
                    "options": {}
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut config = app_config(&agent_dir, None);
        config.cwd = project.clone();
        config.trust_project = false;

        for reconciliation in prepare_native_packages(&config).await.unwrap() {
            reconciliation.commit();
        }

        assert!(!project.join(".pi/plugins.lock").exists());
        assert!(!project.join(".pi/plugins/installed").exists());
    }

    #[tokio::test]
    async fn whole_session_reload_prepares_a_fresh_javascript_generation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let mut config = app_config(&agent_dir, None);
        config.cwd = project.clone();
        config.session_path = agent_dir.join("reload.jsonl");
        config.trust_override = Some(true);
        let (trust, _) = ProjectTrustService::new(&agent_dir, Some(true), false).unwrap();
        let host = Arc::new(RecordingJsHost::default());
        let factory = AppSessionFactory::new(config.clone(), trust)
            .with_js_plugin_host(host.clone(), JsHostMode::Tui);
        let runtime = pi_session::AgentSessionRuntime::create(
            factory,
            AgentSessionRuntimeTarget::create(&project, &config.session_path),
        )
        .await
        .unwrap();

        runtime.reload().await.unwrap();
        assert_eq!(*host.retired.lock().unwrap(), ["js-1"]);
        runtime.shutdown().await.unwrap();

        assert_eq!(host.generation.load(Ordering::SeqCst), 2);
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.mode == JsHostMode::Tui)
        );
        assert!(requests.iter().all(|request| request.cwd == project));
        drop(requests);
        drop(runtime);
        assert_eq!(*host.retired.lock().unwrap(), ["js-1", "js-2"]);
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
                None,
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
            build_runtime(&config, &NativePlugins::default(), None).unwrap(),
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
