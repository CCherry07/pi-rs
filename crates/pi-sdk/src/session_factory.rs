//! Complete product-generation preparation and its staged activation transaction.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use pi_js_package_manager::PackageManager as JsPackageManager;
use pi_js_plugin::{JsGenerationRequest, JsPluginGeneration, JsPluginHost};
use pi_plugin::{PluginContext, PresentationMode};
use pi_plugin_manager::install::{
    InstallScope, PluginManager, PluginManagerOptions, PreparedPluginReconcile,
};
use pi_plugin_manager::loader::{NativePluginLoader, NativePluginLoaderOptions};
use pi_plugin_subagents::SubagentRuntime;
use pi_session::{
    PiPluginContext, PluginContextBinding, PluginProviderMutationAccess, PluginUiBridge,
    PreparedSessionGeneration, SessionError, SessionGenerationActivation, SessionGenerationFactory,
    SessionGenerationRequest, SessionRuntimeInventory, validate_initial_model_scope,
};
use pi_settings::{SettingsContext, SettingsManager};

use crate::Config;
use crate::builtin_providers::BuiltinProviderSet;
use crate::configuration::{
    apply_settings, initial_model_request, memory_options, session_options,
};
use crate::dynamic_providers::{
    DynamicProviderCandidate, DynamicProviderOverlay, DynamicProviderPreparation,
};
use crate::project_trust::ProjectTrustService;
use crate::runtime_composition::{
    GenerationComponents, RuntimeBuildOutcome, additional_active_tools, prepare_memory_provider,
};
use crate::runtime_inventory::{configured_native_plugin_ids, javascript_inventory_labels};

#[derive(Clone)]
pub struct ProductSessionFactory {
    config: Config,
    project_trust: ProjectTrustService,
    settings: SettingsManager,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
    plugin_context_binding: PluginContextBinding,
    plugin_ui_bridge: Option<Arc<dyn PluginUiBridge>>,
    presentation_mode: PresentationMode,
    dynamic_providers: DynamicProviderOverlay,
    subagents: SubagentRuntime,
}

/// Generation-external product state staged while a complete session
/// generation is prepared. The contained package reconciliations roll back on
/// drop; provider mutations remain pending only when this candidate commits.
struct PreparedProductActivation {
    dynamic_providers: DynamicProviderOverlay,
    dynamic_provider_candidate: Option<DynamicProviderCandidate>,
    dynamic_provider_preparation: DynamicProviderPreparation,
    package_reconciliations: Vec<PreparedPluginReconcile>,
}

impl SessionGenerationActivation for PreparedProductActivation {
    fn commit(self: Box<Self>) {
        let Self {
            dynamic_providers,
            dynamic_provider_candidate,
            mut dynamic_provider_preparation,
            package_reconciliations,
        } = *self;
        for reconciliation in package_reconciliations {
            reconciliation.commit();
        }
        if let Some(candidate) = dynamic_provider_candidate {
            dynamic_providers.commit(candidate);
        }
        dynamic_provider_preparation.finish();
    }

    fn rollback(self: Box<Self>, error: SessionError) -> SessionError {
        let Self {
            dynamic_providers,
            dynamic_provider_candidate,
            dynamic_provider_preparation: _dynamic_provider_preparation,
            package_reconciliations,
        } = *self;
        if let Some(candidate) = &dynamic_provider_candidate {
            dynamic_providers.reject(candidate);
        }
        let mut rollback_errors = Vec::new();
        for reconciliation in package_reconciliations.into_iter().rev() {
            if let Err(rollback_error) = reconciliation.rollback() {
                rollback_errors.push(rollback_error.to_string());
            }
        }
        if rollback_errors.is_empty() {
            error
        } else {
            SessionError::Runtime(format!(
                "{error}; native package rollback failed: {}",
                rollback_errors.join("; ")
            ))
        }
    }
}

impl ProductSessionFactory {
    pub fn new(
        config: Config,
        project_trust: ProjectTrustService,
        settings: SettingsManager,
    ) -> Self {
        Self {
            config,
            project_trust,
            settings,
            js_plugin_host: None,
            plugin_context_binding: PluginContextBinding::new(),
            plugin_ui_bridge: None,
            presentation_mode: PresentationMode::Print,
            dynamic_providers: DynamicProviderOverlay::default(),
            subagents: SubagentRuntime::default(),
        }
    }

    pub fn with_js_plugin_host(mut self, host: Arc<dyn JsPluginHost>) -> Self {
        self.js_plugin_host = Some(host);
        self
    }

    pub fn with_plugin_context(
        mut self,
        mode: PresentationMode,
        session_binding: PluginContextBinding,
    ) -> Self {
        self.presentation_mode = mode;
        self.plugin_context_binding = session_binding;
        self
    }

    pub fn with_plugin_ui_bridge(mut self, bridge: Arc<dyn PluginUiBridge>) -> Self {
        self.plugin_ui_bridge = Some(bridge);
        self
    }
}

#[async_trait]
impl SessionGenerationFactory for ProductSessionFactory {
    fn session_registered(&self, session: &pi_session::PiSession) {
        self.plugin_context_binding.bind(session.clone());
        if self.config.features.subagents {
            self.subagents.session_registered(session.clone());
        }
    }

    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        let SessionGenerationRequest {
            cwd,
            workspace,
            session_path: path,
            reason,
            generation_overlay,
            initial_state,
            reload_model,
        } = request;
        if workspace.cwd() != cwd {
            return Err(SessionError::Runtime(
                "generation cwd disagrees with workspace".into(),
            ));
        }
        let reloading = reason == pi_session::SessionStartReason::Reload;
        let dynamic_provider_preparation = self.dynamic_providers.begin_preparation();
        let mut config = self.config.clone();
        config.cwd = cwd;
        config.workspace = Some(workspace);
        if reloading {
            // Startup CLI selections must not overwrite later model changes.
            // Treat the settled live selection as explicit during reload so
            // intentionally unlisted provider models remain selectable too.
            config.requested_provider = reload_model
                .as_ref()
                .map(|model| model.provider.to_string());
            config.model = reload_model.map(|model| model.model_id.to_string());
        }
        let project_trusted = self
            .project_trust
            .resolve(&config.cwd)
            .await
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let settings = self
            .settings
            .load(&SettingsContext::new(&config.cwd, project_trusted));
        apply_settings(&mut config, &settings);
        let local_mcp = if config.load_mcp_config {
            Some(
                pi_plugin_mcp::McpLibrary::new(
                    &config.agent_dir,
                    Some(&config.cwd),
                    project_trusted,
                )
                .prepare()
                .await
                .map_err(SessionError::Runtime)?,
            )
        } else {
            None
        };
        let memory = prepare_memory_provider(
            config.features.memory,
            memory_options(
                &config.cwd,
                &config.agent_dir,
                &config.session_path,
                Some(&path),
                project_trusted,
            ),
        )
        .await
        .map_err(SessionError::Runtime)?;
        let package_reconciliations =
            prepare_native_packages(&config.cwd, &config.agent_dir, project_trusted).await?;
        let mut native_options = NativePluginLoaderOptions::new(&config.cwd, &config.agent_dir);
        native_options.project_trusted = project_trusted;
        native_options.explicit_paths = config.native_plugins.clone();
        let native_plugins = NativePluginLoader::new(native_options)
            .discover()
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let configured_native_plugins =
            configured_native_plugin_ids(&package_reconciliations, &native_plugins);
        let js_resolution = JsPackageManager::with_settings(
            config.javascript_resolve_request(project_trusted),
            self.settings.clone(),
        )
        .resolve()
        .await
        .map_err(|error| SessionError::Runtime(error.to_string()))?;
        config
            .settings_skill_paths
            .extend(js_resolution.skill_paths.iter().cloned());
        config
            .settings_prompt_paths
            .extend(js_resolution.prompt_paths.iter().cloned());
        let mutation_access: Arc<dyn PluginProviderMutationAccess> =
            Arc::new(self.dynamic_providers.clone());
        let plugin_context = PiPluginContext::new(
            self.presentation_mode,
            project_trusted,
            self.plugin_context_binding.clone(),
        )
        .with_model_scope(
            config
                .runtime_settings
                .enabled_models
                .clone()
                .unwrap_or_default(),
        )
        .with_provider_mutations(mutation_access);
        let plugin_context = match &self.plugin_ui_bridge {
            Some(bridge) => plugin_context.with_ui_bridge(Arc::clone(bridge)),
            None => plugin_context,
        };
        let plugin_context = Arc::new(plugin_context);
        let context_access: Arc<dyn PluginContext> = plugin_context.clone();
        let mut js_extensions = Vec::new();
        let mut dynamic_provider_candidate = None;
        let js_generation = if let Some(host) = &self.js_plugin_host {
            let extension_labels = javascript_inventory_labels(&js_resolution.extension_identities);
            let extension_paths = js_resolution.extension_paths;
            let manifest = host
                .prepare_generation(JsGenerationRequest {
                    project_trusted,
                    extension_paths: extension_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                    mode: self.presentation_mode,
                    cwd: config.cwd.display().to_string(),
                    workspace: config
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.spec().clone()),
                    flag_values: config.extension_flag_values.clone(),
                })
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            let candidate = self
                .dynamic_providers
                .candidate(&manifest.provider_registrations)
                .map_err(SessionError::Runtime)?;
            let generation = match JsPluginGeneration::prepare_with_host(manifest, Arc::clone(host))
            {
                Ok(generation) => generation,
                Err(error) => {
                    self.dynamic_providers.reject(&candidate);
                    return Err(SessionError::Runtime(error.to_string()));
                }
            };
            js_extensions = extension_labels;
            dynamic_provider_candidate = Some(candidate);
            Some(generation)
        } else {
            None
        };
        let components = GenerationComponents {
            native: &native_plugins,
            javascript: js_generation.as_ref(),
            mcp: local_mcp.as_ref(),
            memory: memory.as_ref(),
            subagents: &self.subagents,
        };
        let built_runtime = match BuiltinProviderSet::prepare(&config)
            .and_then(|providers| {
                components.build_runtime(
                    &config,
                    project_trusted,
                    context_access,
                    &generation_overlay,
                    dynamic_provider_candidate.as_ref(),
                    providers,
                )
            })
            .map_err(SessionError::from)
            .and_then(|built| {
                if let Some(initial_state) = &initial_state {
                    validate_initial_model_scope(
                        &initial_state.model,
                        initial_state.model_source,
                        config.runtime_settings.enabled_models.as_deref(),
                        &built.runtime.available_models(),
                    )?;
                }
                Ok(built)
            }) {
            Ok(built) => built,
            Err(error) => {
                if let Some(candidate) = &dynamic_provider_candidate {
                    self.dynamic_providers.reject(candidate);
                }
                return Err(error);
            }
        };
        let RuntimeBuildOutcome {
            runtime,
            initial_model_fallback_message,
        } = built_runtime;
        let session_options = session_options(
            &config.runtime_settings,
            initial_model_request(
                config.requested_provider.as_deref(),
                config.model.as_deref(),
                &config.runtime_settings,
            ),
        )
        .initial_model_fallback_message(initial_model_fallback_message)
        .additional_active_tools(additional_active_tools(&runtime))
        .runtime_inventory(SessionRuntimeInventory::new(
            js_extensions,
            configured_native_plugins,
        ));
        let activation = PreparedProductActivation {
            dynamic_providers: self.dynamic_providers.clone(),
            dynamic_provider_candidate,
            dynamic_provider_preparation,
            package_reconciliations,
        };
        Ok(PreparedSessionGeneration::new(runtime, session_options)
            .bind_session(move |session| plugin_context.bind_generation_session(session))
            .with_activation(activation))
    }
}

async fn prepare_native_packages(
    cwd: &Path,
    agent_dir: &Path,
    project_trusted: bool,
) -> Result<Vec<PreparedPluginReconcile>, SessionError> {
    let mut options = PluginManagerOptions::new(cwd, agent_dir);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::test_support::app_config;
    use pi_core::ProviderId;
    use pi_session::{MultiSessionManager, SessionGenerationOverlay};

    #[derive(Default)]
    struct RecordingJsHost {
        generation: AtomicUsize,
        requests: Mutex<Vec<JsGenerationRequest>>,
        retired: Mutex<Vec<String>>,
        provider_registrations: Mutex<Vec<pi_js_plugin::JsProviderRegistration>>,
    }

    #[async_trait]
    impl pi_js_plugin::JsCallbackDispatcher for RecordingJsHost {
        async fn invoke(
            &self,
            _invocation: pi_js_plugin::JsInvocation,
            _context: pi_js_plugin::PluginContextHandle,
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
                provider_registrations: self.provider_registrations.lock().unwrap().clone(),
                session_plugins: Vec::new(),
                diagnostics: Vec::new(),
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
kind = "plugin"
artifact = "plugin.dylib"

[options]
command = "fixture-command"
"#,
        )
        .unwrap();
        package
    }

    #[tokio::test]
    async fn shared_factory_binds_plugin_context_to_each_managed_session() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let first_cwd = directory.path().join("first");
        let second_cwd = directory.path().join("second");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&first_cwd).unwrap();
        std::fs::create_dir_all(&second_cwd).unwrap();
        let mut config = app_config(&agent_dir, None);
        config.trust_override = Some(true);
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let binding = PluginContextBinding::new();
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent_dir))
            .with_plugin_context(PresentationMode::Tui, binding);
        let sessions = pi_session::MultiSessionManager::new(factory);
        let first = sessions
            .create_session(&first_cwd, agent_dir.join("first.jsonl"))
            .await
            .unwrap();
        let second = sessions
            .create_session(&second_cwd, agent_dir.join("second.jsonl"))
            .await
            .unwrap();
        let first_id = first.id();
        let second_id = second.id();

        let context = pi_plugin::CommandContextParts::new(
            first
                .current()
                .runtime()
                .plugin_context_handle(pi_plugin::PluginContextScope::Command),
        );
        let replacement = context
            .session
            .create(pi_plugin::NewSessionOptions::default())
            .await
            .unwrap();
        let pi_plugin::SessionReplacement::Replaced(replacement) = replacement else {
            panic!("new session should replace the first managed handle");
        };

        assert_ne!(first.id(), first_id);
        assert_eq!(replacement.session.id().unwrap(), first.id());
        assert_eq!(second.id(), second_id);
        sessions.shutdown().await.unwrap();
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
            let prepared = prepare_native_packages(&config.cwd, &config.agent_dir, true)
                .await
                .unwrap();
            assert!(activation.join("plugin.dylib").is_file());
            assert!(lock_path.is_file());
            let manifest = std::fs::read_to_string(activation.join("pi-plugin.toml")).unwrap();
            assert!(manifest.contains("command = \"project-command\""));
            drop(prepared);
        }
        assert!(!activation.exists());
        assert!(!lock_path.exists());

        for reconciliation in prepare_native_packages(&config.cwd, &config.agent_dir, true)
            .await
            .unwrap()
        {
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

        for reconciliation in prepare_native_packages(&config.cwd, &config.agent_dir, false)
            .await
            .unwrap()
        {
            reconciliation.commit();
        }

        assert!(!project.join(".pi/plugins.lock").exists());
        assert!(!project.join(".pi/plugins/installed").exists());
    }

    #[tokio::test]
    async fn product_factory_defers_dynamic_provider_commit_until_session_activation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let host = Arc::new(RecordingJsHost::default());
        host.provider_registrations
            .lock()
            .unwrap()
            .push(pi_js_plugin::JsProviderRegistration {
                plugin_id: "js:0:provider.ts".to_string(),
                path: "/provider.ts".to_string(),
                name: "activation-provider".to_string(),
                config: serde_json::json!({
                    "baseUrl": "https://activation.example/v1",
                    "apiKey": "test-key",
                    "api": "openai-responses",
                    "models": [{ "id": "activation-model" }]
                }),
            });
        let mut config = app_config(&agent_dir, None);
        config.cwd = project.clone();
        config.trust_override = Some(true);
        config.discover_extensions = false;
        config.load_mcp_config = false;
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent_dir))
            .with_plugin_context(PresentationMode::Tui, PluginContextBinding::new())
            .with_js_plugin_host(host);
        let session_path = agent_dir.join("activation.jsonl");
        let prepared = factory
            .prepare_generation(SessionGenerationRequest {
                workspace: pi_core::WorkspaceSpec::from_cwd(&project).snapshot(),
                cwd: project.clone(),
                session_path: session_path.clone(),
                reason: pi_session::SessionStartReason::Startup,
                generation_overlay: SessionGenerationOverlay::default(),
                initial_state: None,
                reload_model: None,
            })
            .await
            .unwrap();

        assert!(
            !factory
                .dynamic_providers
                .candidate(&[])
                .unwrap()
                .provider_configs()
                .any(|(provider, _)| provider.as_str() == "activation-provider")
        );
        drop(prepared);

        let manager = MultiSessionManager::new(factory.clone());
        let session = manager
            .create_session(&project, &session_path)
            .await
            .unwrap();

        assert!(
            session
                .current()
                .runtime()
                .has_provider(&ProviderId::new("activation-provider"))
        );
        assert!(
            factory
                .dynamic_providers
                .candidate(&[])
                .unwrap()
                .provider_configs()
                .any(|(provider, _)| provider.as_str() == "activation-provider")
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn whole_session_reload_prepares_a_fresh_javascript_generation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let package_source = "npm:@narumitw/pi-lsp@0.49.5";
        let package = agent_dir.join("npm/node_modules/@narumitw/pi-lsp");
        let extension = package.join("dist/index.ts");
        std::fs::create_dir_all(extension.parent().unwrap()).unwrap();
        std::fs::write(&extension, "export default function () {}\n").unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{
              "name": "@narumitw/pi-lsp",
              "version": "0.49.5",
              "pi": {"extensions": ["./dist/index.ts"]}
            }"#,
        )
        .unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            format!(r#"{{"packages":["{package_source}"]}}"#),
        )
        .unwrap();
        let mut config = app_config(&agent_dir, None);
        config.cwd = project.clone();
        config.session_path = agent_dir.join("reload.jsonl");
        config.trust_override = Some(true);
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let host = Arc::new(RecordingJsHost::default());
        let factory =
            ProductSessionFactory::new(config.clone(), trust, SettingsManager::new(&agent_dir))
                .with_plugin_context(PresentationMode::Tui, PluginContextBinding::new())
                .with_js_plugin_host(host.clone());
        let manager = MultiSessionManager::new(factory);
        let session = manager
            .create_session(&project, &config.session_path)
            .await
            .unwrap();

        assert_eq!(
            session.current().runtime_inventory().js_extensions(),
            [package_source]
        );

        session.reload().await.unwrap();
        assert_eq!(
            session.current().runtime_inventory().js_extensions(),
            [package_source]
        );
        assert_eq!(*host.retired.lock().unwrap(), ["js-1"]);
        manager.shutdown().await.unwrap();

        assert_eq!(host.generation.load(Ordering::SeqCst), 2);
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.mode == PresentationMode::Tui)
        );
        assert!(requests.iter().all(|request| request.project_trusted));
        assert!(
            requests
                .iter()
                .all(|request| request.extension_paths == [extension.display().to_string()])
        );
        drop(requests);
        drop(session);
        drop(manager);
        assert_eq!(*host.retired.lock().unwrap(), ["js-1", "js-2"]);
    }
}
