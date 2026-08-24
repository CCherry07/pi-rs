use std::sync::Arc;

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId};
use pi_js_plugin::{JsGenerationRequest, JsHostMode, JsPluginGeneration, JsPluginHost};
use pi_plugin_anthropic::AnthropicPlugin;
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
use pi_plugin_openai::{OpenAiCodexPlugin, OpenAiCompatibleConfig, OpenAiCompatiblePlugin};
use pi_plugin_read::ReadPlugin;
use pi_plugin_skills::{SkillLoaderOptions, SkillsPlugin};
use pi_plugin_write::WritePlugin;
use pi_plugin_xai::XAiPlugin;
use pi_resources::ResourceLoaderOptions;
use pi_runtime::{PiRuntime, RuntimeError, SystemPrompt};
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
    AgentSessionRuntimeTarget, InitialModelRequest, ModelRuntimeServices, PreparedAgentSession,
    SessionError, SessionPlugins,
};

use crate::auth::{StoredCredential, read_stored_credential};
use crate::config::AppConfig;
use crate::project_trust::ProjectTrustService;

#[derive(Clone)]
pub(crate) struct ProductSessionFactory {
    config: AppConfig,
    project_trust: ProjectTrustService,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
    js_host_mode: JsHostMode,
}

impl ProductSessionFactory {
    pub(crate) fn new(config: AppConfig, project_trust: ProjectTrustService) -> Self {
        Self {
            config,
            project_trust,
            js_plugin_host: None,
            js_host_mode: JsHostMode::Print,
        }
    }

    pub(crate) fn with_js_plugin_host(
        mut self,
        host: Arc<dyn JsPluginHost>,
        mode: JsHostMode,
    ) -> Self {
        self.js_plugin_host = Some(host);
        self.js_host_mode = mode;
        self
    }
}

#[async_trait]
impl AgentSessionRuntimeFactory for ProductSessionFactory {
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
        let project_trusted = self
            .project_trust
            .resolve(&config.cwd)
            .await
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let package_reconciliations = prepare_native_packages(&config, project_trusted).await?;
        let mut native_options = NativePluginLoaderOptions::new(&config.cwd, &config.agent_dir);
        native_options.project_trusted = project_trusted;
        native_options.explicit_paths = config.native_plugins.clone();
        let native_plugins = NativePluginLoader::new(native_options)
            .discover()
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let js_generation = if let Some(host) = &self.js_plugin_host {
            let manifest = host
                .prepare_generation(JsGenerationRequest {
                    cwd: config.cwd.clone(),
                    agent_dir: config.agent_dir.clone(),
                    project_trusted,
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
        let runtime = build_runtime(
            &config,
            project_trusted,
            &native_plugins,
            js_generation.as_ref(),
        )?;
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
    project_trusted: bool,
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
    if project_trusted {
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
    project_trusted: bool,
    native_plugins: &NativePlugins,
    js_generation: Option<&JsPluginGeneration>,
) -> Result<PiRuntime, RuntimeError> {
    build_runtime_with_codex_credentials(
        config,
        project_trusted,
        native_plugins,
        js_generation,
        None,
    )
}

fn build_runtime_with_codex_credentials(
    config: &AppConfig,
    project_trusted: bool,
    native_plugins: &NativePlugins,
    js_generation: Option<&JsPluginGeneration>,
    codex_credentials: Option<pi_plugin_openai::CodexCredentials>,
) -> Result<PiRuntime, RuntimeError> {
    let stored_anthropic =
        read_stored_credential(&config.agent_dir, "anthropic").map_err(RuntimeError::Build)?;
    let stored_xai =
        read_stored_credential(&config.agent_dir, "xai").map_err(RuntimeError::Build)?;
    let stored_codex =
        read_stored_credential(&config.agent_dir, "openai-codex").map_err(RuntimeError::Build)?;
    let stored_compatible =
        read_stored_credential(&config.agent_dir, &config.provider).map_err(RuntimeError::Build)?;
    let effective_api_key = config.api_key.clone().or_else(|| {
        stored_compatible
            .as_ref()
            .and_then(StoredCredential::secret)
            .map(str::to_string)
    });
    let provider_config = effective_api_key
        .as_ref()
        .map_or_else(
            || OpenAiCompatibleConfig::without_api_key(&config.base_url),
            |api_key| OpenAiCompatibleConfig::new(&config.base_url, api_key),
        )
        .provider_id(config.provider.clone());
    let mut skill_options = SkillLoaderOptions::new(&config.cwd, &config.agent_dir);
    skill_options.project_trusted = project_trusted;
    if let Some(home) = std::env::var_os("HOME") {
        skill_options
            .additional_paths
            .push(std::path::PathBuf::from(home).join(".agents/skills"));
    }
    let mut model_options = ModelsPluginOptions::for_agent_dir(&config.agent_dir);
    if let Some(api_key) = &effective_api_key {
        model_options = model_options.runtime_api_key(config.provider.clone(), api_key.clone());
    }

    let builder = PiRuntime::builder();
    let codex_credentials = codex_credentials.unwrap_or_else(|| {
        stored_codex
            .as_ref()
            .and_then(StoredCredential::secret)
            .map(pi_plugin_openai::CodexCredentials::from_access_token)
            .unwrap_or_else(pi_plugin_openai::CodexCredentials::discover)
    });
    let builder = if config.provider == "openai-codex" {
        let credentials = codex_credentials.clone();
        builder.provider_plugin_factory(move || OpenAiCodexPlugin::new(credentials.clone()))
    } else if config.provider == "xai" {
        let api_key = config.api_key.clone();
        let selected_xai = stored_xai.clone();
        builder.provider_plugin_factory(move || match &api_key {
            Some(api_key) => XAiPlugin::new(Some(api_key.clone())),
            None => XAiPlugin::from_stored(
                selected_xai
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_string),
            ),
        })
    } else if config.provider == "anthropic" {
        builder
    } else {
        builder.try_provider_plugin_factory({
            let provider_config = provider_config.clone();
            move || OpenAiCompatiblePlugin::new(provider_config.clone())
        })
    };
    let builder = if config.provider == "anthropic" {
        let api_key = config.api_key.clone();
        builder.provider_plugin_factory(move || match &api_key {
            Some(api_key) => AnthropicPlugin::with_api_key(api_key.clone()),
            None => {
                AnthropicPlugin::from_stored(stored_anthropic.as_ref().and_then(|credential| {
                    credential
                        .secret()
                        .map(|secret| (secret, credential.is_oauth()))
                }))
            }
        })
    } else {
        let stored_anthropic = stored_anthropic.clone();
        builder.provider_plugin_factory(move || {
            AnthropicPlugin::from_stored(stored_anthropic.as_ref().and_then(|credential| {
                credential
                    .secret()
                    .map(|secret| (secret, credential.is_oauth()))
            }))
        })
    };
    let builder = if config.provider == "openai-codex" {
        builder
    } else {
        builder.provider_plugin_factory(move || OpenAiCodexPlugin::new(codex_credentials.clone()))
    };
    let builder = if config.provider == "xai" {
        builder
    } else {
        let stored_xai = stored_xai.clone();
        builder.provider_plugin_factory(move || {
            XAiPlugin::from_stored(
                stored_xai
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_string),
            )
        })
    };
    let builder = builder
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
    resources.project_trusted = project_trusted;

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
            max_tool_iterations: 100,
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

    fn jwt(account_id: &str) -> String {
        use base64::Engine;

        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
            })
            .to_string(),
        );
        format!("header.{payload}.signature")
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
            trust_override: None,
            native_plugins: Vec::new(),
            extensions: Vec::new(),
            discover_extensions: true,
        }
    }

    #[test]
    fn codex_catalog_is_registered_even_when_another_provider_is_selected() {
        let directory = tempfile::tempdir().unwrap();
        let config = app_config(directory.path(), None);

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        assert!(
            runtime
                .model(&ProviderId::new("openai-codex"), &ModelId::new("gpt-5.5"))
                .is_some()
        );
        assert!(runtime.available_models().is_empty());
    }

    #[test]
    fn xai_catalog_is_registered_but_unavailable_without_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let config = app_config(directory.path(), None);

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        let model = runtime
            .model(&ProviderId::new("xai"), &ModelId::new("grok-4.6"))
            .unwrap();
        assert_eq!(model.context_window, 500_000);
        assert!(
            runtime
                .available_models()
                .iter()
                .all(|model| model.provider != ProviderId::new("xai"))
        );
    }

    #[test]
    fn explicit_xai_selection_uses_cli_credentials_without_duplicate_registration() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = app_config(directory.path(), Some("grok-4.6"));
        config.provider = "xai".to_string();
        config.requested_provider = Some("xai".to_string());
        config.api_key = Some("xai-test-token".to_string());

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        assert_eq!(runtime.agent().state().provider_id.as_str(), "xai");
        assert_eq!(runtime.agent().state().model_id.as_str(), "grok-4.6");
        assert!(runtime.available_models().iter().any(|model| {
            model.provider == ProviderId::new("xai") && model.id == ModelId::new("grok-4.6")
        }));
    }

    #[test]
    fn anthropic_catalog_is_registered_and_cli_credentials_select_claude() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = app_config(directory.path(), Some("claude-sonnet-4-6"));
        config.provider = "anthropic".to_string();
        config.requested_provider = Some("anthropic".to_string());
        config.api_key = Some("anthropic-test-token".to_string());

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        assert_eq!(runtime.agent().state().provider_id.as_str(), "anthropic");
        assert_eq!(
            runtime.agent().state().model_id.as_str(),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn stored_codex_oauth_credential_makes_the_catalog_available() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("auth.json"),
            serde_json::json!({
                "openai-codex": {
                    "type": "oauth",
                    "access": jwt("acct-stored"),
                    "refresh": "refresh-token",
                    "expires": 4_102_444_800_000_f64,
                    "accountId": "acct-stored"
                }
            })
            .to_string(),
        )
        .unwrap();
        let mut config = app_config(directory.path(), Some("gpt-5.5"));
        config.provider = "openai-codex".to_string();
        config.requested_provider = Some("openai-codex".to_string());

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(runtime.agent().state().provider_id.as_str(), "openai-codex");
        assert!(runtime.available_models().iter().any(|model| {
            model.provider == ProviderId::new("openai-codex") && model.id == ModelId::new("gpt-5.5")
        }));
    }

    #[test]
    fn openai_codex_provider_loads_its_builtin_model_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = app_config(directory.path(), Some("gpt-5.5"));
        config.provider = "openai-codex".to_string();
        config.requested_provider = Some("openai-codex".to_string());
        config.base_url = "https://chatgpt.com/backend-api".to_string();

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        let model = runtime
            .model(&ProviderId::new("openai-codex"), &ModelId::new("gpt-5.5"))
            .unwrap();
        assert_eq!(model.context_window, 272_000);
        assert_eq!(runtime.agent().state().model_id.as_str(), "gpt-5.5");
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
                  "apiKey": "test-key",
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
            true,
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
                  "apiKey": "test-key",
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
            true,
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

        let runtime = build_runtime_with_codex_credentials(
            &app_config(directory.path(), None),
            true,
            &NativePlugins::default(),
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
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
        let activation = project.join(".pi/plugins/installed/0000-local-plugin");
        let lock_path = project.join(".pi/plugins.lock");

        {
            let prepared = prepare_native_packages(&config, true).await.unwrap();
            assert!(activation.join("plugin.dylib").is_file());
            assert!(lock_path.is_file());
            let manifest = std::fs::read_to_string(activation.join("pi-plugin.toml")).unwrap();
            assert!(manifest.contains("command = \"project-command\""));
            drop(prepared);
        }
        assert!(!activation.exists());
        assert!(!lock_path.exists());

        for reconciliation in prepare_native_packages(&config, true).await.unwrap() {
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

        for reconciliation in prepare_native_packages(&config, false).await.unwrap() {
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
        let factory = ProductSessionFactory::new(config.clone(), trust)
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
                  "apiKey": "test-key",
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
                true,
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
            build_runtime(&config, true, &NativePlugins::default(), None).unwrap(),
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
