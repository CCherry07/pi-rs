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
    PiPluginContext, PluginContextBinding, PluginMcpMutationAccess, PluginProviderMutationAccess,
    PluginUiBridge, PreparedSessionGeneration, SessionError, SessionGenerationActivation,
    SessionGenerationFactory, SessionGenerationRequest, SessionRuntimeInventory,
    validate_initial_model_scope,
};
use pi_settings::{SettingsContext, SettingsManager};

use crate::Config;
use crate::builtin_providers::BuiltinProviderSet;
use crate::composition::{
    GenerationComponents, RuntimeBuildOutcome, additional_active_tools, prepare_memory,
};
use crate::configuration::{apply_settings, initial_model_request, session_options};
use crate::dynamic_mcp::{DynamicMcpCandidate, DynamicMcpPreparation, DynamicMcpRegistry};
use crate::dynamic_providers::{
    DynamicProviderCandidate, DynamicProviderOverlay, DynamicProviderPreparation,
};
use crate::features::FeaturesPlugin;
use crate::project_trust::ProjectTrustService;
use crate::runtime_inventory::{configured_native_plugin_ids, javascript_inventory_labels};

type SessionBinding = Arc<dyn Fn(&Arc<pi_session::AgentSession>) + Send + Sync>;

#[derive(Clone)]
pub struct ProductSessionFactory {
    config: Config,
    project_trust: ProjectTrustService,
    settings: SettingsManager,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
    plugin_context_binding: PluginContextBinding,
    plugin_ui_bridge: Option<Arc<dyn PluginUiBridge>>,
    mcp_oauth_host: Option<Arc<dyn pi_plugin_mcp::McpOAuthHost>>,
    mcp_auth: pi_plugin_mcp::McpClientAuth,
    mcp_sessions: crate::mcp::McpSessionCache,
    session_bindings: Vec<SessionBinding>,
    presentation_mode: PresentationMode,
    dynamic_providers: DynamicProviderOverlay,
    dynamic_mcp: DynamicMcpRegistry,
    subagents: SubagentRuntime,
}

/// Generation-external product state staged while a complete session
/// generation is prepared. The contained package reconciliations roll back on
/// drop; provider mutations remain pending only when this candidate commits.
struct PreparedProductActivation {
    dynamic_providers: DynamicProviderOverlay,
    dynamic_provider_candidate: Option<DynamicProviderCandidate>,
    dynamic_provider_preparation: DynamicProviderPreparation,
    dynamic_mcp_candidate: Option<DynamicMcpCandidate>,
    dynamic_mcp_preparation: DynamicMcpPreparation,
    package_reconciliations: Vec<PreparedPluginReconcile>,
}

impl SessionGenerationActivation for PreparedProductActivation {
    fn commit(self: Box<Self>) {
        let Self {
            dynamic_providers,
            dynamic_provider_candidate,
            mut dynamic_provider_preparation,
            dynamic_mcp_candidate,
            dynamic_mcp_preparation,
            package_reconciliations,
        } = *self;
        for reconciliation in package_reconciliations {
            reconciliation.commit();
        }
        if let Some(candidate) = dynamic_provider_candidate {
            dynamic_providers.commit(candidate);
        }
        dynamic_provider_preparation.finish();
        if let Some(candidate) = dynamic_mcp_candidate {
            candidate.commit();
        }
        dynamic_mcp_preparation.finish();
    }

    fn rollback(self: Box<Self>, error: SessionError) -> SessionError {
        let Self {
            dynamic_providers,
            dynamic_provider_candidate,
            dynamic_provider_preparation: _dynamic_provider_preparation,
            dynamic_mcp_candidate,
            dynamic_mcp_preparation: _dynamic_mcp_preparation,
            package_reconciliations,
        } = *self;
        if let Some(candidate) = &dynamic_provider_candidate {
            dynamic_providers.reject(candidate);
        }
        if let Some(candidate) = dynamic_mcp_candidate {
            candidate.reject();
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
        let mcp_auth = crate::mcp_client_auth(&config.agent_dir);
        Self {
            config,
            project_trust,
            settings,
            js_plugin_host: None,
            plugin_context_binding: PluginContextBinding::new(),
            plugin_ui_bridge: None,
            mcp_oauth_host: None,
            mcp_auth,
            mcp_sessions: crate::mcp::McpSessionCache::default(),
            session_bindings: Vec::new(),
            presentation_mode: PresentationMode::Print,
            dynamic_providers: DynamicProviderOverlay::default(),
            dynamic_mcp: DynamicMcpRegistry::default(),
            subagents: SubagentRuntime::default(),
        }
    }

    pub fn with_mcp_oauth_host(mut self, host: Arc<dyn pi_plugin_mcp::McpOAuthHost>) -> Self {
        self.mcp_oauth_host = Some(host);
        self
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

    /// Bind frontend observers to every prepared generation before session_start.
    /// Core plugin capabilities are already bound; activation/rollback stays owned
    /// by the existing session-generation transaction.
    pub fn with_session_binding(
        mut self,
        binding: impl Fn(&Arc<pi_session::AgentSession>) + Send + Sync + 'static,
    ) -> Self {
        self.session_bindings.push(Arc::new(binding));
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
        // Register allowed hosts even when settings currently disable subagents:
        // a later generation can enable them without replacing the stable handle.
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
            session_path: _,
            runtime_scope,
            reason,
            generation_overlay,
            initial_state,
            reload_model,
            reload_registered_tools,
            restored_configuration,
        } = request;
        if workspace.cwd() != cwd {
            return Err(SessionError::Runtime(
                "generation cwd disagrees with workspace".into(),
            ));
        }
        let reloading = reason == pi_session::SessionStartReason::Reload;
        let dynamic_provider_preparation = self.dynamic_providers.begin_preparation();
        let dynamic_mcp_preparation = self.dynamic_mcp.begin_preparation(runtime_scope);
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
        let js_packages = JsPackageManager::with_settings(
            config.javascript_resolve_request(project_trusted),
            self.settings.clone(),
        );
        // Native-only frontends must not silently omit extensions or install
        // packages before discovering that they cannot execute their code.
        if self.js_plugin_host.is_none() && js_packages.requires_javascript_host() {
            return Err(SessionError::Runtime(
                "JavaScript extensions require a JavaScript host; this frontend is native-only"
                    .to_string(),
            ));
        }
        let js_resolution = js_packages
            .resolve()
            .await
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        config
            .settings_skill_paths
            .extend(js_resolution.skill_paths.iter().cloned());
        config
            .settings_prompt_paths
            .extend(js_resolution.prompt_paths.iter().cloned());
        // Extension-owned MCP registrations are generation contributions, so JavaScript loading
        // must finish before the candidate MCP inventory is prepared. Runtime mutations are replayed
        // into the newly loaded extension registry before its manifest crosses the native boundary.
        let extension_paths = js_resolution
            .extension_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        let dynamic_mcp_candidate = self
            .js_plugin_host
            .as_ref()
            .map(|_| self.dynamic_mcp.candidate(runtime_scope, &extension_paths));
        let prepared_js = if let Some(host) = &self.js_plugin_host {
            let extension_labels = javascript_inventory_labels(&js_resolution.extension_identities);
            let manifest = host
                .prepare_generation(JsGenerationRequest {
                    project_trusted,
                    agent_dir: config.agent_dir.display().to_string(),
                    extension_paths: extension_paths.clone(),
                    output_pad: config.runtime_settings.output_pad,
                    show_images: config.runtime_settings.terminal.show_images,
                    mode: self.presentation_mode,
                    cwd: config.cwd.display().to_string(),
                    workspace: config
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.spec().clone()),
                    flag_values: config.extension_flag_values.clone(),
                    mcp_server_mutations: dynamic_mcp_candidate
                        .as_ref()
                        .map(|candidate| candidate.mutations().to_vec())
                        .unwrap_or_default(),
                    mcp_servers_changed: dynamic_mcp_candidate
                        .as_ref()
                        .is_some_and(DynamicMcpCandidate::has_changes),
                })
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            Some((manifest, extension_labels))
        } else {
            None
        };
        let extension_mcp_servers = prepared_js
            .as_ref()
            .map(|(manifest, _)| {
                manifest
                    .mcp_server_registrations
                    .iter()
                    .map(|registration| {
                        pi_plugin_mcp::McpExtensionServerDefinition::new(
                            registration.name.clone(),
                            registration.config.clone(),
                            registration.path.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // Materialize the JavaScript generation now so every later preparation failure drops its
        // lease and retires the host generation instead of leaking callbacks.
        let mut js_extensions = Vec::new();
        let mut dynamic_provider_candidate = None;
        let js_generation = if let Some((manifest, extension_labels)) = prepared_js {
            let host = self
                .js_plugin_host
                .as_ref()
                .expect("prepared JavaScript manifest has a host");
            let generation = JsPluginGeneration::prepare_with_host(manifest, Arc::clone(host))
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            let candidate = self
                .dynamic_providers
                .candidate(generation.provider_registrations())
                .map_err(SessionError::Runtime)?;
            js_extensions = extension_labels;
            dynamic_provider_candidate = Some(candidate);
            Some(generation)
        } else {
            None
        };
        let register_mcp_management = config.load_mcp_config || !extension_mcp_servers.is_empty();
        let selection_binding = crate::mcp::McpToolSelectionBinding::default();
        // A JavaScript host keeps an empty lifecycle-only MCP candidate so removing the last
        // extension registration retires its previous connection on generation activation.
        let (local_mcp, mcp_freshness, mcp_selection) =
            if config.load_mcp_config || self.js_plugin_host.is_some() {
                let selected = match restored_configuration
                    .as_ref()
                    .map(|state| &state.active_tools)
                {
                    Some(pi_session::RestoredToolSelection::Selected(names)) => names.clone(),
                    _ => Vec::new(),
                };
                let candidates = restored_configuration
                    .as_ref()
                    .map(|state| state.historical_selected_tool_names.clone())
                    .unwrap_or_default();
                let permitted = |names: Vec<String>| {
                    names
                        .into_iter()
                        .filter(|name| generation_overlay.allows_tool(name))
                        .collect()
                };
                let mut library = pi_plugin_mcp::McpLibrary::new(
                    &config.agent_dir,
                    Some(&config.cwd),
                    project_trusted,
                )
                .with_tool_search_registration(false)
                .with_command_values(true)
                .with_configured_servers(config.load_mcp_config)
                .with_management_registration(register_mcp_management)
                .with_client_auth(self.mcp_auth.clone())
                .with_preserved_tool_selection(permitted(selected))
                .with_tool_selection_candidates(permitted(candidates))
                .with_tool_selection_reader(selection_binding.reader());
                library = library
                    .with_extension_servers(extension_mcp_servers)
                    .map_err(SessionError::Runtime)?;
                if let Some(host) = &self.mcp_oauth_host {
                    library = library.with_oauth_host(host.clone());
                }
                let credentials = crate::mcp::McpCredentialSnapshot::capture(
                    &config.agent_dir,
                    library
                        .supervised_credential_watch()
                        .map_err(SessionError::Runtime)?,
                );
                let prepared = library
                    .prepare_supervised_generation(
                        self.mcp_sessions.owner(runtime_scope),
                        credentials.provider_revisions(),
                    )
                    .await
                    .map_err(SessionError::Runtime)?;
                let mut selection: std::collections::BTreeMap<_, _> = prepared
                    .temporarily_unavailable_tools
                    .into_iter()
                    .map(|name| {
                        (
                            name,
                            pi_session::ToolSelectionAvailability::TemporarilyUnavailable,
                        )
                    })
                    .collect();
                selection.extend(prepared.unresolved_tool_selection.into_iter().map(|name| {
                    (
                        name,
                        pi_session::ToolSelectionAvailability::UnresolvedUntilInventory,
                    )
                }));
                let readiness = prepared.readiness_watch.ok_or_else(|| {
                    SessionError::Runtime("MCP preparation omitted its readiness owner".into())
                })?;
                self.mcp_sessions
                    .stage(runtime_scope, readiness.downgrade());
                let freshness = credentials.with_readiness(readiness);
                (Some(prepared.plugin), Some(freshness), selection)
            } else {
                (None, None, std::collections::BTreeMap::new())
            };
        let memory = prepare_memory(&config, project_trusted).map_err(SessionError::Runtime)?;
        let package_reconciliations = if config.offline {
            Vec::new()
        } else {
            prepare_native_packages(&config.cwd, &config.agent_dir, project_trusted).await?
        };
        let mut native_options = NativePluginLoaderOptions::new(&config.cwd, &config.agent_dir);
        native_options.project_trusted = project_trusted;
        native_options.explicit_paths = config.native_plugins.clone();
        let native_plugins = NativePluginLoader::new(native_options)
            .discover()
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let configured_native_plugins =
            configured_native_plugin_ids(&package_reconciliations, &native_plugins);
        let mutation_access: Arc<dyn PluginProviderMutationAccess> =
            Arc::new(self.dynamic_providers.clone());
        let mcp_mutation_access: Arc<dyn PluginMcpMutationAccess> =
            self.dynamic_mcp.scoped_access(runtime_scope);
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
        .with_provider_mutations(mutation_access)
        .with_mcp_mutations(mcp_mutation_access);
        let plugin_context = match &self.plugin_ui_bridge {
            Some(bridge) => plugin_context.with_ui_bridge(Arc::clone(bridge)),
            None => plugin_context,
        };
        let plugin_context = Arc::new(plugin_context);
        let context_access: Arc<dyn PluginContext> = plugin_context.clone();
        let components = GenerationComponents {
            features: FeaturesPlugin::new(
                self.settings.clone(),
                SettingsContext::new(&config.cwd, project_trusted),
                config.features,
            ),
            native: &native_plugins,
            javascript: js_generation.as_ref(),
            mcp: local_mcp.as_ref(),
            memory: memory.as_ref(),
            subagents: &self.subagents,
        };
        let built_runtime = match BuiltinProviderSet::prepare_with_catalog(
            &mut config,
            dynamic_provider_candidate.as_ref(),
        )
        .await
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
        let mut session_options = session_options(
            &config.runtime_settings,
            initial_model_request(
                config.requested_provider.as_deref(),
                config.model.as_deref(),
                &config.runtime_settings,
            ),
        )
        .initial_model_fallback_message(initial_model_fallback_message)
        .settings_writer(Arc::new(crate::configuration::CodingSessionSettings(
            self.settings.clone(),
        )))
        .additional_active_tools(additional_active_tools(
            &runtime,
            reload_registered_tools.as_deref(),
        ))
        .tool_selection_availability(mcp_selection)
        .runtime_inventory(SessionRuntimeInventory::new(
            js_extensions,
            configured_native_plugins,
        ));
        session_options.generation_freshness = mcp_freshness;
        let activation = PreparedProductActivation {
            dynamic_providers: self.dynamic_providers.clone(),
            dynamic_provider_candidate,
            dynamic_provider_preparation,
            dynamic_mcp_candidate,
            dynamic_mcp_preparation,
            package_reconciliations,
        };
        let bindings = self.session_bindings.clone();
        Ok(PreparedSessionGeneration::new(runtime, session_options)
            .bind_session(move |session| {
                selection_binding.bind(&session);
                plugin_context.bind_generation_session(session.clone());
                for binding in bindings {
                    binding(&session);
                }
            })
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
    use axum::response::IntoResponse;
    use pi_core::ProviderId;
    use pi_session::{MultiSessionManager, SessionGenerationOverlay};

    #[derive(Default)]
    struct RecordingJsHost {
        generation: AtomicUsize,
        requests: Mutex<Vec<JsGenerationRequest>>,
        retired: Mutex<Vec<String>>,
        provider_registrations: Mutex<Vec<pi_js_plugin::JsProviderRegistration>>,
        mcp_server_registrations: Mutex<Vec<pi_js_plugin::JsMcpServerRegistration>>,
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
            let mut mcp_server_registrations =
                self.mcp_server_registrations.lock().unwrap().clone();
            for mutation in &request.mcp_server_mutations {
                match mutation {
                    pi_js_plugin::JsMcpServerMutation::Register {
                        extension_path,
                        name,
                        config,
                    } => {
                        mcp_server_registrations.retain(|registration| {
                            registration.name != *name || registration.path != *extension_path
                        });
                        mcp_server_registrations.push(pi_js_plugin::JsMcpServerRegistration {
                            plugin_id: "reload-fixture".to_string(),
                            path: extension_path.clone(),
                            name: name.clone(),
                            config: config.clone(),
                        });
                    }
                    pi_js_plugin::JsMcpServerMutation::Unregister {
                        extension_path,
                        name,
                    } => mcp_server_registrations.retain(|registration| {
                        registration.name != *name || registration.path != *extension_path
                    }),
                }
            }
            self.requests.lock().unwrap().push(request);
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(pi_js_plugin::JsGenerationManifest {
                generation_id: format!("js-{generation}"),
                dynamic_hooks: false,
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
                mcp_server_registrations,
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

    fn native_only_config(cwd: &Path, agent_dir: &Path) -> Config {
        std::fs::create_dir_all(agent_dir).unwrap();
        let mut config = app_config(agent_dir, None);
        config.cwd = cwd.to_path_buf();
        config.features = crate::Features::none();
        config.load_mcp_config = false;
        config.trust_override = Some(true);
        config
    }

    fn has_mcp_manager(session: &pi_session::PiSession) -> bool {
        session
            .current()
            .catalog()
            .command_specs()
            .iter()
            .any(|command| command.name == "mcp")
    }

    async fn register_disabled_runtime_mcp(
        session: &pi_session::PiSession,
        extension: &Path,
        name: &str,
    ) {
        let context = session
            .current()
            .plugin_context(pi_plugin::PluginContextScope::Base);
        pi_js_plugin::execute_context_notification(
            &context,
            pi_js_plugin::ExtensionContextNotification::RegisterMcpServer {
                extension_path: extension.display().to_string(),
                name: name.to_string(),
                config: serde_json::json!({"command":"unused", "enabled":false}),
            },
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !has_mcp_manager(session) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("runtime registration did not publish an MCP generation");
    }

    #[tokio::test]
    async fn no_host_rejects_explicit_javascript_before_native_preparation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let mut config = native_only_config(directory.path(), &agent_dir);
        config.discover_extensions = false;
        config.extensions.push("./explicit.ts".into());
        config
            .native_plugins
            .push(directory.path().join("missing-native-plugin"));
        let pi = crate::Pi::builder(config).build().unwrap();
        let error = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions::new(
                directory.path(),
                agent_dir.join("session.jsonl"),
            ))
            .await
            .err()
            .expect("missing JS host must fail");
        assert!(
            error
                .to_string()
                .contains("JavaScript extensions require a JavaScript host")
        );
        assert!(!agent_dir.join("session.jsonl").exists());
        pi.sessions().shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn no_host_applies_project_trust_before_extension_preflight() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        let extensions = cwd.join(".pi/extensions");
        std::fs::create_dir_all(&extensions).unwrap();
        std::fs::write(extensions.join("test.ts"), "export default () => {}; ").unwrap();
        for trusted in [false, true] {
            let agent_dir = directory.path().join(format!("agent-{trusted}"));
            let mut config = native_only_config(&cwd, &agent_dir);
            config.trust_override = Some(trusted);
            let pi = crate::Pi::builder(config).build().unwrap();
            let result = pi
                .sessions()
                .create_session(pi_session::CreateSessionOptions::new(
                    &cwd,
                    agent_dir.join("session.jsonl"),
                ))
                .await;
            if trusted {
                assert!(
                    result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("JavaScript extensions require")
                );
            } else {
                assert!(
                    result.is_ok(),
                    "untrusted extensions must not require a JS host"
                );
            }
            pi.sessions().shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn no_host_uses_effective_extension_settings_without_installing() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let config = native_only_config(directory.path(), &agent_dir);
        std::fs::create_dir_all(agent_dir.join("extensions")).unwrap();
        std::fs::write(
            agent_dir.join("extensions/test.ts"),
            "export default () => {}; ",
        )
        .unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            r#"{"extensions":["-extensions/test.ts"]}"#,
        )
        .unwrap();
        let pi = crate::Pi::builder(config).build().unwrap();
        let result = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions::new(
                directory.path(),
                agent_dir.join("session.jsonl"),
            ))
            .await;
        assert!(
            result.is_ok(),
            "effective disabled extension should need no host: {:?}",
            result.err()
        );
        pi.sessions().shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn no_host_failed_reload_keeps_the_previous_generation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let pi = crate::Pi::builder(native_only_config(directory.path(), &agent_dir))
            .build()
            .unwrap();
        let session = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions::new(
                directory.path(),
                agent_dir.join("session.jsonl"),
            ))
            .await
            .unwrap();
        let previous = session.current();
        std::fs::create_dir_all(agent_dir.join("extensions")).unwrap();
        std::fs::write(
            agent_dir.join("extensions/new.ts"),
            "export default () => {}; ",
        )
        .unwrap();
        assert!(
            session
                .reload()
                .await
                .unwrap_err()
                .to_string()
                .contains("JavaScript extensions require")
        );
        assert!(Arc::ptr_eq(&previous, &session.current()));
        assert!(!session.path().exists());
        pi.sessions().shutdown().await.unwrap();
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
            .create_session(pi_session::CreateSessionOptions::new(
                &first_cwd,
                agent_dir.join("first.jsonl"),
            ))
            .await
            .unwrap();
        let second = sessions
            .create_session(pi_session::CreateSessionOptions::new(
                &second_cwd,
                agent_dir.join("second.jsonl"),
            ))
            .await
            .unwrap();
        let first_id = first.id();
        let second_id = second.id();

        let context = pi_plugin::CommandContextParts::new(
            first
                .current()
                .plugin_context(pi_plugin::PluginContextScope::Command),
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
                runtime_scope: pi_session::LiveRuntimeScopeId::new(),
                reason: pi_session::SessionStartReason::Startup,
                generation_overlay: SessionGenerationOverlay::default(),
                initial_state: None,
                reload_model: None,
                reload_registered_tools: None,
                restored_configuration: None,
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
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                &session_path,
            ))
            .await
            .unwrap();

        assert!(
            session
                .current()
                .catalog()
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
    async fn invalid_extension_mcp_retires_the_prepared_javascript_generation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        let extension = directory.path().join("extension.ts");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&extension, "export default function () {}\n").unwrap();

        let host = Arc::new(RecordingJsHost::default());
        host.mcp_server_registrations
            .lock()
            .unwrap()
            .push(pi_js_plugin::JsMcpServerRegistration {
                plugin_id: "reload-fixture".to_string(),
                path: extension.display().to_string(),
                name: "invalid-extension".to_string(),
                config: serde_json::json!({"url":"not-an-http-url"}),
            });
        let mut config = native_only_config(&project, &agent_dir);
        config.discover_extensions = false;
        config.extensions.push(extension.display().to_string());
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent_dir))
            .with_plugin_context(PresentationMode::Tui, PluginContextBinding::new())
            .with_js_plugin_host(host.clone());
        let manager = MultiSessionManager::new(factory);
        let error = manager
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent_dir.join("invalid-extension-mcp.jsonl"),
            ))
            .await
            .err()
            .expect("invalid extension MCP must reject generation preparation");

        assert!(
            error
                .to_string()
                .contains("expected an absolute http(s) URL"),
            "{error}"
        );
        assert_eq!(*host.retired.lock().unwrap(), ["js-1"]);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn extension_mcp_is_generation_local_and_survives_configured_mcp_disable() {
        let cleaned_up = Arc::new(tokio::sync::Notify::new());
        let cleanup_signal = cleaned_up.clone();
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(
                |axum::Json(request): axum::Json<serde_json::Value>| async move {
                    if request["id"].is_null() {
                        return axum::http::StatusCode::ACCEPTED.into_response();
                    }
                    let result = match request["method"].as_str() {
                        Some("initialize") => serde_json::json!({
                            "protocolVersion":"2025-03-26",
                            "capabilities":{"tools":{}},
                            "serverInfo":{"name":"extension-fixture","version":"1"}
                        }),
                        Some("tools/list") => serde_json::json!({"tools":[{
                            "name":"echo",
                            "description":"Echo extension input",
                            "inputSchema":{"type":"object"}
                        }]}),
                        _ => {
                            return axum::Json(serde_json::json!({
                                "jsonrpc":"2.0",
                                "id":request["id"],
                                "error":{"code":-32601,"message":"unsupported"}
                            }))
                            .into_response();
                        }
                    };
                    let initialize = request["method"] == "initialize";
                    let mut response = axum::Json(serde_json::json!({
                        "jsonrpc":"2.0",
                        "id":request["id"],
                        "result":result
                    }))
                    .into_response();
                    if initialize {
                        response.headers_mut().insert(
                            "mcp-session-id",
                            axum::http::HeaderValue::from_static("extension-session"),
                        );
                    }
                    response
                },
            )
            .delete(move || async move {
                cleanup_signal.notify_one();
                axum::http::StatusCode::OK
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        let extension = directory.path().join("extension.ts");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&extension, "export default function () {}\n").unwrap();
        // This malformed configured file proves --no-mcp-style loading is not re-enabled merely
        // because an explicit extension contributes a generation-local MCP server.
        std::fs::write(agent_dir.join("mcp.json"), "malformed").unwrap();

        let host = Arc::new(RecordingJsHost::default());
        host.mcp_server_registrations
            .lock()
            .unwrap()
            .push(pi_js_plugin::JsMcpServerRegistration {
                plugin_id: "reload-fixture".to_string(),
                path: extension.display().to_string(),
                name: "extension-fixture".to_string(),
                config: serde_json::json!({"url":url, "exposure":"direct"}),
            });
        let mut config = native_only_config(&project, &agent_dir);
        config.discover_extensions = false;
        config.extensions.push(extension.display().to_string());
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent_dir))
            .with_plugin_context(PresentationMode::Tui, PluginContextBinding::new())
            .with_js_plugin_host(host.clone());
        let manager = MultiSessionManager::new(factory);
        let session = manager
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent_dir.join("extension-mcp.jsonl"),
            ))
            .await
            .unwrap();

        assert!(
            session
                .current()
                .catalog()
                .command_specs()
                .iter()
                .any(|command| command.name == "mcp")
        );
        assert!(
            session
                .current()
                .catalog()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == "mcp__extension_fixture__echo"),
            "extension MCP tools must be executable generation contributions"
        );

        host.mcp_server_registrations.lock().unwrap().clear();
        session.reload().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), cleaned_up.notified())
            .await
            .expect("removed extension MCP connection was not cleaned up");
        assert!(
            session
                .current()
                .catalog()
                .command_specs()
                .iter()
                .all(|command| command.name != "mcp"),
            "removing the extension registration must remove its generation-local manager"
        );
        manager.shutdown().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn runtime_extension_mcp_mutations_publish_through_whole_generation_reload() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        let extension = directory.path().join("extension.ts");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&extension, "export default function () {}\n").unwrap();

        let host = Arc::new(RecordingJsHost::default());
        let mut config = native_only_config(&project, &agent_dir);
        config.discover_extensions = false;
        config.extensions.push(extension.display().to_string());
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent_dir))
            .with_plugin_context(PresentationMode::Tui, PluginContextBinding::new())
            .with_js_plugin_host(host.clone());
        let manager = MultiSessionManager::new(factory);
        let session = manager
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent_dir.join("runtime-extension-mcp.jsonl"),
            ))
            .await
            .unwrap();
        assert!(
            session
                .current()
                .catalog()
                .command_specs()
                .iter()
                .all(|command| command.name != "mcp")
        );

        let context = session
            .current()
            .plugin_context(pi_plugin::PluginContextScope::Base);
        pi_js_plugin::execute_context_notification(
            &context,
            pi_js_plugin::ExtensionContextNotification::RegisterMcpServer {
                extension_path: extension.display().to_string(),
                name: "late".to_string(),
                config: serde_json::json!({"command":"unused", "enabled":false}),
            },
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if session
                    .current()
                    .catalog()
                    .command_specs()
                    .iter()
                    .any(|command| command.name == "mcp")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("runtime registration did not publish an MCP generation");

        session.reload().await.unwrap();
        assert!(
            session
                .current()
                .catalog()
                .command_specs()
                .iter()
                .any(|command| command.name == "mcp"),
            "runtime registration must survive an ordinary reload in the same live scope"
        );
        let sibling = manager
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent_dir.join("runtime-extension-mcp-sibling.jsonl"),
            ))
            .await
            .unwrap();
        assert!(
            !has_mcp_manager(&sibling),
            "runtime registration must remain local to its live session scope"
        );

        register_disabled_runtime_mcp(&sibling, &extension, "fresh-transitions").await;
        sibling
            .new_session(&project, agent_dir.join("runtime-extension-mcp-new.jsonl"))
            .await
            .unwrap();
        assert!(
            !has_mcp_manager(&sibling),
            "new-session replacement must not import the prior live scope's runtime MCP overlay"
        );

        let resume_path = agent_dir.join("runtime-extension-mcp-resume.jsonl");
        let resume_store = pi_session::SessionStore::create(
            &resume_path,
            pi_session::SessionHeader::new("resume-target", &project),
        )
        .unwrap();
        let fork_point = resume_store
            .append_message(pi_core::Message::User(pi_core::UserMessage::text(
                "resume and fork boundary",
                1,
            )))
            .unwrap();
        register_disabled_runtime_mcp(&sibling, &extension, "fresh-transitions").await;
        sibling.resume_session(&resume_path).await.unwrap();
        assert!(
            !has_mcp_manager(&sibling),
            "resume replacement must not import the prior live scope's runtime MCP overlay"
        );

        register_disabled_runtime_mcp(&sibling, &extension, "fresh-transitions").await;
        sibling
            .fork_session(fork_point, pi_session::ForkPosition::At)
            .await
            .unwrap();
        assert!(
            !has_mcp_manager(&sibling),
            "fork replacement must not import the prior live scope's runtime MCP overlay"
        );

        let context = session
            .current()
            .plugin_context(pi_plugin::PluginContextScope::Base);
        pi_js_plugin::execute_context_notification(
            &context,
            pi_js_plugin::ExtensionContextNotification::UnregisterMcpServer {
                extension_path: extension.display().to_string(),
                name: "late".to_string(),
            },
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if session
                    .current()
                    .catalog()
                    .command_specs()
                    .iter()
                    .all(|command| command.name != "mcp")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("runtime removal did not publish an MCP generation");

        {
            let requests = host.requests.lock().unwrap();
            assert!(requests.iter().any(|request| request.mcp_servers_changed
                && matches!(
                    request.mcp_server_mutations.as_slice(),
                    [pi_js_plugin::JsMcpServerMutation::Register { name, .. }] if name == "late"
                )));
            assert!(requests.iter().any(|request| !request.mcp_servers_changed
                && matches!(
                    request.mcp_server_mutations.as_slice(),
                    [pi_js_plugin::JsMcpServerMutation::Register { name, .. }] if name == "late"
                )));
            assert!(requests.iter().any(|request| request.mcp_servers_changed
                && matches!(
                    request.mcp_server_mutations.as_slice(),
                    [pi_js_plugin::JsMcpServerMutation::Unregister { name, .. }] if name == "late"
                )));
        }
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_runtime_extension_mcp_mutation_preserves_generation_and_is_not_replayed() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        let extension = directory.path().join("extension.ts");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&extension, "export default function () {}\n").unwrap();

        let host = Arc::new(RecordingJsHost::default());
        let mut config = native_only_config(&project, &agent_dir);
        config.discover_extensions = false;
        config.extensions.push(extension.display().to_string());
        let (trust, _) = ProjectTrustService::new(
            &agent_dir,
            Some(true),
            false,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent_dir))
            .with_plugin_context(PresentationMode::Tui, PluginContextBinding::new())
            .with_js_plugin_host(host.clone());
        let manager = MultiSessionManager::new(factory);
        let session = manager
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent_dir.join("invalid-runtime-extension-mcp.jsonl"),
            ))
            .await
            .unwrap();
        let original = session.current();
        let context = original.plugin_context(pi_plugin::PluginContextScope::Base);
        pi_js_plugin::execute_context_notification(
            &context,
            pi_js_plugin::ExtensionContextNotification::RegisterMcpServer {
                extension_path: extension.display().to_string(),
                name: "invalid".to_string(),
                config: serde_json::json!({"url":"not-an-absolute-url"}),
            },
        )
        .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if host
                    .retired
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|generation| generation == "js-2")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("invalid runtime MCP candidate was not retired");
        assert!(Arc::ptr_eq(&original, &session.current()));
        assert!(
            session
                .current()
                .catalog()
                .command_specs()
                .iter()
                .all(|command| command.name != "mcp")
        );

        session.reload().await.unwrap();
        {
            let requests = host.requests.lock().unwrap();
            assert!(
                requests
                    .last()
                    .is_some_and(|request| request.mcp_server_mutations.is_empty())
            );
        }
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
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                &config.session_path,
            ))
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
                .all(|request| request.agent_dir == agent_dir.display().to_string())
        );
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
