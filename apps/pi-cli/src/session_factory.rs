use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent::{AgentOptions, QueueMode};
use pi_core::{ModelId, PluginId, ProviderId, ThinkingBudgets, ThinkingLevel};
use pi_js_package_manager::{PackageManager as JsPackageManager, ResolvedExtensionIdentity};
use pi_js_plugin::{
    ExtensionContextAccess, ExtensionProviderMutationAccess, ExtensionSessionBinding,
    JsGenerationRequest, JsHostMode, JsPluginGeneration, JsPluginHost,
    SessionExtensionContextAccess,
};
use pi_plugin_anthropic::AnthropicPlugin;
use pi_plugin_bash::{BashToolOptions, ConfiguredBashPlugin};
use pi_plugin_edit::EditPlugin;
use pi_plugin_find::FindPlugin;
use pi_plugin_google::GooglePlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_hashline_edit::HashlineEditPlugin;
use pi_plugin_loader::{NativePluginLoader, NativePluginLoaderOptions, NativePlugins};
use pi_plugin_ls::LsPlugin;
use pi_plugin_manager::{
    InstallScope, PluginManager, PluginManagerOptions, PreparedPluginReconcile,
};
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use pi_plugin_openai::{
    CodexTransport, CodexTransportOptions, OpenAiCodexPlugin, OpenAiCompatibleConfig,
    OpenAiCompatiblePlugin,
};
use pi_plugin_prompts::{PromptTemplateLoaderOptions, PromptTemplatesPlugin};
use pi_plugin_read::ConfiguredReadPlugin;
use pi_plugin_skills::{SkillLoaderOptions, SkillsPlugin};
use pi_plugin_write::WritePlugin;
use pi_plugin_xai::XAiPlugin;
use pi_provider::{HttpTransport, ReqwestTransport, ReqwestTransportConfig};
use pi_resources::ResourceLoaderOptions;
use pi_runtime::{CompletionRetryPolicy, PiRuntime, RuntimeError, SystemPrompt};
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
    AgentSessionRuntimeTarget, AutoRetrySettings, CompactionSettings as SessionCompactionSettings,
    InitialModelRequest, ModelRuntimeServices, PreparedAgentSession, SessionError, SessionPlugins,
    SessionRuntimeInventory,
};
use pi_settings::{
    QueueModeSetting, SettingsContext, SettingsManager, ThinkingLevelSetting, TransportSetting,
};

use crate::auth::{StoredCredential, read_stored_credential};
use crate::config::AppConfig;
use crate::dynamic_providers::{DynamicProviderCandidate, DynamicProviderOverlay};
use crate::project_trust::ProjectTrustService;

const BUILTIN_TOOL_NAMES: [&str; 8] = [
    "read",
    "grep",
    "find",
    "ls",
    "write",
    "edit",
    "hashline_edit",
    "bash",
];

#[derive(Clone)]
pub(crate) struct ProductSessionFactory {
    config: AppConfig,
    project_trust: ProjectTrustService,
    settings: SettingsManager,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
    js_session_binding: Option<ExtensionSessionBinding>,
    js_host_mode: JsHostMode,
    dynamic_providers: DynamicProviderOverlay,
}

impl ProductSessionFactory {
    pub(crate) fn new(
        config: AppConfig,
        project_trust: ProjectTrustService,
        settings: SettingsManager,
    ) -> Self {
        Self {
            config,
            project_trust,
            settings,
            js_plugin_host: None,
            js_session_binding: None,
            js_host_mode: JsHostMode::Print,
            dynamic_providers: DynamicProviderOverlay::default(),
        }
    }

    pub(crate) fn with_js_plugin_host(
        mut self,
        host: Arc<dyn JsPluginHost>,
        mode: JsHostMode,
        session_binding: ExtensionSessionBinding,
    ) -> Self {
        self.js_plugin_host = Some(host);
        self.js_session_binding = Some(session_binding);
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
        let (path, create, cwd, reused_log, parent_session) = match request.target {
            AgentSessionRuntimeTarget::Create {
                cwd,
                path,
                parent_session,
            } => (path, true, cwd, None, parent_session),
            AgentSessionRuntimeTarget::Open { path } => {
                let (_, document) = pi_session::SessionLog::open(&path)?;
                (path, false, document.header.cwd, None, None)
            }
            AgentSessionRuntimeTarget::Reuse { log } => {
                let document = log.load()?;
                (
                    log.path().to_path_buf(),
                    false,
                    document.header.cwd,
                    Some(log),
                    None,
                )
            }
        };
        let mut dynamic_provider_preparation = self.dynamic_providers.begin_preparation();
        let mut config = self.config.clone();
        config.cwd = cwd;
        let project_trusted = self
            .project_trust
            .resolve(&config.cwd)
            .await
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let settings = self
            .settings
            .load(&SettingsContext::new(&config.cwd, project_trusted));
        config.runtime_settings = settings.effective().clone();
        // Pi treats the proxy as process/bootstrap configuration. A trusted
        // project may tune request behavior, but cannot redirect HTTP traffic.
        config.runtime_settings.http_proxy = settings.global().http_proxy.clone();
        config.settings_diagnostics = settings
            .diagnostics()
            .iter()
            .map(|diagnostic| pi_resources::ResourceDiagnostic {
                kind: pi_resources::DiagnosticKind::Warning,
                message: diagnostic.message.clone(),
                path: diagnostic.path.clone(),
            })
            .collect();
        config.settings_skill_paths = scoped_setting_paths(
            &settings.global().skills,
            &config.agent_dir,
            &settings.project().skills,
            &config.cwd.join(".pi"),
        );
        config.settings_prompt_paths = scoped_setting_paths(
            &settings.global().prompts,
            &config.agent_dir,
            &settings.project().prompts,
            &config.cwd.join(".pi"),
        );
        let package_reconciliations = prepare_native_packages(&config, project_trusted).await?;
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
        let mut js_context = None;
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
                    mode: self.js_host_mode,
                    cwd: config.cwd.display().to_string(),
                    flag_values: config.extension_flag_values.clone(),
                })
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            let candidate = self
                .dynamic_providers
                .candidate(&manifest.provider_registrations)
                .map_err(SessionError::Runtime)?;
            let binding = match self.js_session_binding.clone() {
                Some(binding) => binding,
                None => {
                    self.dynamic_providers.reject(&candidate);
                    return Err(SessionError::Runtime(
                        "JavaScript plugin host is missing its session binding".to_string(),
                    ));
                }
            };
            let mutation_access: Arc<dyn ExtensionProviderMutationAccess> =
                Arc::new(self.dynamic_providers.clone());
            let context = Arc::new(
                SessionExtensionContextAccess::new(project_trusted, binding)
                    .with_provider_mutations(mutation_access),
            );
            let context_access: Arc<dyn ExtensionContextAccess> = context.clone();
            let generation = match JsPluginGeneration::prepare_with_host_and_context(
                manifest,
                Arc::clone(host),
                context_access,
            ) {
                Ok(generation) => generation,
                Err(error) => {
                    self.dynamic_providers.reject(&candidate);
                    return Err(SessionError::Runtime(error.to_string()));
                }
            };
            js_extensions = extension_labels;
            js_context = Some(context);
            dynamic_provider_candidate = Some(candidate);
            Some(generation)
        } else {
            None
        };
        let runtime = match build_runtime(
            &config,
            project_trusted,
            &native_plugins,
            js_generation.as_ref(),
            dynamic_provider_candidate.as_ref(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                if let Some(candidate) = &dynamic_provider_candidate {
                    self.dynamic_providers.reject(candidate);
                }
                return Err(error.into());
            }
        };
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
            .compaction(session_compaction_settings(&config))
            .branch_summary_reserve_tokens(config.runtime_settings.branch_summary.reserve_tokens)
            .retry(AutoRetrySettings {
                enabled: config.runtime_settings.retry.enabled,
                max_retries: config.runtime_settings.retry.max_retries,
                base_delay_ms: config.runtime_settings.retry.base_delay_ms,
            })
            .initial_model(initial_model_request(&config))
            .runtime_inventory(SessionRuntimeInventory::new(
                js_extensions,
                configured_native_plugins,
            ))
            .shell(
                config
                    .runtime_settings
                    .shell_path
                    .as_deref()
                    .map(expand_tilde_path),
                config.runtime_settings.shell_command_prefix.clone(),
            )
            .parent_session_path(parent_session);
        let prepared = if create {
            AgentSession::prepare_create_with_options(runtime, path, session_options).await
        } else if let Some(log) = reused_log {
            AgentSession::prepare_reuse_with_options(runtime, log, session_options).await
        } else {
            AgentSession::prepare_open_with_options(runtime, path, session_options).await
        };
        match prepared {
            Ok(prepared) => {
                if let Some(context) = &js_context {
                    context.bind_generation_session(prepared.session());
                }
                for reconciliation in package_reconciliations {
                    reconciliation.commit();
                }
                if let Some(candidate) = dynamic_provider_candidate {
                    self.dynamic_providers.commit(candidate);
                }
                dynamic_provider_preparation.finish();
                Ok(prepared)
            }
            Err(error) => {
                if let Some(candidate) = &dynamic_provider_candidate {
                    self.dynamic_providers.reject(candidate);
                }
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

fn configured_native_plugin_ids(
    reconciliations: &[PreparedPluginReconcile],
    native_plugins: &NativePlugins,
) -> Vec<PluginId> {
    retain_loaded_configured_native_plugins(
        reconciliations
            .iter()
            .flat_map(PreparedPluginReconcile::installed)
            .map(|plugin| plugin.id),
        native_plugins
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id),
    )
}

fn retain_loaded_configured_native_plugins(
    configured: impl IntoIterator<Item = String>,
    loaded: impl IntoIterator<Item = String>,
) -> Vec<PluginId> {
    let loaded = loaded.into_iter().collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    configured
        .into_iter()
        .filter(|id| loaded.contains(id) && seen.insert(id.clone()))
        .map(PluginId::new)
        .collect()
}

fn javascript_inventory_labels(identities: &[ResolvedExtensionIdentity]) -> Vec<String> {
    let paths = identities
        .iter()
        .filter_map(|identity| match identity {
            ResolvedExtensionIdentity::Package(_) => None,
            ResolvedExtensionIdentity::Path(path) => Some(path.clone()),
        })
        .collect::<Vec<_>>();
    let mut path_labels = compact_extension_labels(&paths).into_iter();
    let mut seen = HashSet::new();

    identities
        .iter()
        .filter_map(|identity| {
            let label = match identity {
                ResolvedExtensionIdentity::Package(source) => source.clone(),
                ResolvedExtensionIdentity::Path(_) => path_labels.next()?,
            };
            seen.insert(label.clone()).then_some(label)
        })
        .collect()
}

fn compact_extension_labels(paths: &[PathBuf]) -> Vec<String> {
    let segments = paths
        .iter()
        .map(|path| {
            let mut segments = path
                .iter()
                .map(|segment| segment.to_string_lossy().into_owned())
                .filter(|segment| !segment.is_empty() && segment != "/")
                .collect::<Vec<_>>();
            if segments.len() > 1
                && matches!(
                    segments.last().map(String::as_str),
                    Some("index.ts" | "index.js")
                )
            {
                segments.pop();
            }
            if segments.is_empty() {
                segments.push(path.display().to_string());
            }
            segments
        })
        .collect::<Vec<_>>();

    segments
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (1..=path.len())
                .find_map(|count| {
                    let candidate = &path[path.len() - count..];
                    segments
                        .iter()
                        .enumerate()
                        .all(|(other_index, other)| {
                            other_index == index
                                || other.len() < count
                                || !other.ends_with(candidate)
                        })
                        .then(|| candidate.join("/"))
                })
                .unwrap_or_else(|| path.join("/"))
        })
        .collect()
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
    dynamic_providers: Option<&DynamicProviderCandidate>,
) -> Result<PiRuntime, RuntimeError> {
    build_runtime_with_codex_credentials(
        config,
        project_trusted,
        native_plugins,
        js_generation,
        dynamic_providers,
        None,
    )
}

fn build_runtime_with_codex_credentials(
    config: &AppConfig,
    project_trusted: bool,
    native_plugins: &NativePlugins,
    js_generation: Option<&JsPluginGeneration>,
    dynamic_providers: Option<&DynamicProviderCandidate>,
    codex_credentials: Option<pi_plugin_openai::CodexCredentials>,
) -> Result<PiRuntime, RuntimeError> {
    let transport = provider_transport(config)?;
    let codex_transport_options = codex_transport_options(config);
    let stored_anthropic =
        read_stored_credential(&config.agent_dir, "anthropic").map_err(RuntimeError::Build)?;
    let stored_google =
        read_stored_credential(&config.agent_dir, "google").map_err(RuntimeError::Build)?;
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
    skill_options.enable_commands = config.runtime_settings.enable_skill_commands;
    skill_options
        .additional_paths
        .extend(config.settings_skill_paths.iter().cloned());
    let mut prompt_template_options =
        PromptTemplateLoaderOptions::new(&config.cwd, &config.agent_dir);
    prompt_template_options.project_trusted = project_trusted;
    prompt_template_options
        .additional_paths
        .extend(config.settings_prompt_paths.iter().cloned());
    if let Some(home) = std::env::var_os("HOME") {
        skill_options
            .additional_paths
            .push(std::path::PathBuf::from(home).join(".agents/skills"));
    }
    let mut model_options = ModelsPluginOptions::for_agent_dir(&config.agent_dir);
    if let Some(api_key) = &effective_api_key {
        model_options = model_options.runtime_api_key(config.provider.clone(), api_key.clone());
    }
    if let Some(dynamic_providers) = dynamic_providers {
        for (provider, provider_config) in dynamic_providers.provider_configs() {
            model_options = model_options.extension_provider_config(provider, provider_config);
        }
    }
    let bash_options = BashToolOptions::new(
        config
            .runtime_settings
            .shell_path
            .as_deref()
            .map(expand_tilde_path),
        config.runtime_settings.shell_command_prefix.clone(),
    );

    let builder = PiRuntime::builder()
        .supplemental_diagnostics(config.settings_diagnostics.clone())
        .completion_retry_policy(CompletionRetryPolicy {
            enabled: config.runtime_settings.retry.enabled,
            max_retries: config.runtime_settings.retry.max_retries,
            base_delay_ms: config.runtime_settings.retry.base_delay_ms,
        });
    let codex_credentials = codex_credentials.unwrap_or_else(|| {
        stored_codex
            .as_ref()
            .and_then(StoredCredential::secret)
            .map(pi_plugin_openai::CodexCredentials::from_access_token)
            .unwrap_or_else(pi_plugin_openai::CodexCredentials::discover)
    });
    let builder = if config.provider == "openai-codex" {
        let credentials = codex_credentials.clone();
        let transport = Arc::clone(&transport);
        let transport_options = codex_transport_options.clone();
        builder.provider_plugin_factory(move || {
            OpenAiCodexPlugin::with_transport_options(
                credentials.clone(),
                Arc::clone(&transport),
                transport_options.clone(),
            )
        })
    } else if config.provider == "xai" {
        let api_key = config.api_key.clone();
        let selected_xai = stored_xai.clone();
        let transport = Arc::clone(&transport);
        builder.provider_plugin_factory(move || match &api_key {
            Some(api_key) => {
                XAiPlugin::new_with_transport(Some(api_key.clone()), Arc::clone(&transport))
            }
            None => XAiPlugin::from_stored_with_transport(
                selected_xai
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_string),
                Arc::clone(&transport),
            ),
        })
    } else if matches!(config.provider.as_str(), "anthropic" | "google") {
        builder
    } else {
        builder.try_provider_plugin_factory({
            let provider_config = provider_config.clone();
            let transport = Arc::clone(&transport);
            move || {
                OpenAiCompatiblePlugin::with_transport(
                    provider_config.clone(),
                    Arc::clone(&transport),
                )
            }
        })
    };
    let builder = if config.provider == "anthropic" {
        let api_key = config.api_key.clone();
        let transport = Arc::clone(&transport);
        builder.provider_plugin_factory(move || match &api_key {
            Some(api_key) => {
                AnthropicPlugin::with_api_key_and_transport(api_key.clone(), Arc::clone(&transport))
            }
            None => AnthropicPlugin::from_stored_with_transport(
                stored_anthropic.as_ref().and_then(|credential| {
                    credential
                        .secret()
                        .map(|secret| (secret, credential.is_oauth()))
                }),
                Arc::clone(&transport),
            ),
        })
    } else {
        let stored_anthropic = stored_anthropic.clone();
        let transport = Arc::clone(&transport);
        builder.provider_plugin_factory(move || {
            AnthropicPlugin::from_stored_with_transport(
                stored_anthropic.as_ref().and_then(|credential| {
                    credential
                        .secret()
                        .map(|secret| (secret, credential.is_oauth()))
                }),
                Arc::clone(&transport),
            )
        })
    };
    let builder = if config.provider == "openai-codex" {
        builder
    } else {
        let transport = Arc::clone(&transport);
        let transport_options = codex_transport_options;
        builder.provider_plugin_factory(move || {
            OpenAiCodexPlugin::with_transport_options(
                codex_credentials.clone(),
                Arc::clone(&transport),
                transport_options.clone(),
            )
        })
    };
    let builder = if config.provider == "xai" {
        builder
    } else {
        let stored_xai = stored_xai.clone();
        let transport = Arc::clone(&transport);
        builder.provider_plugin_factory(move || {
            XAiPlugin::from_stored_with_transport(
                stored_xai
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_string),
                Arc::clone(&transport),
            )
        })
    };
    let builder = if config.provider == "google" {
        let explicit_api_key = config.api_key.clone();
        let stored_google = stored_google.clone();
        let transport = Arc::clone(&transport);
        builder.try_provider_plugin_factory(move || match &explicit_api_key {
            Some(api_key) => {
                GooglePlugin::new_with_transport(Some(api_key.clone()), Arc::clone(&transport))
            }
            None => GooglePlugin::from_stored_with_transport(
                stored_google
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_owned),
                Arc::clone(&transport),
            ),
        })
    } else {
        let transport = Arc::clone(&transport);
        builder.try_provider_plugin_factory(move || {
            GooglePlugin::from_stored_with_transport(
                stored_google
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_owned),
                Arc::clone(&transport),
            )
        })
    };
    let builder = builder
        .try_provider_plugin_factory({
            let model_options = model_options.clone();
            let transport = Arc::clone(&transport);
            move || ModelsPlugin::load_with_transport(model_options.clone(), Arc::clone(&transport))
        })
        .agent_plugin_factory({
            let prompt_template_options = prompt_template_options.clone();
            move || PromptTemplatesPlugin::load(prompt_template_options.clone())
        })
        .agent_plugin_factory({
            let skill_options = skill_options.clone();
            move || SkillsPlugin::load(skill_options.clone())
        })
        .agent_plugin_factory({
            let auto_resize_images = config.runtime_settings.images.auto_resize;
            move || ConfiguredReadPlugin::new(auto_resize_images)
        })
        .agent_plugin_factory(|| GrepPlugin)
        .agent_plugin_factory(|| FindPlugin)
        .agent_plugin_factory(|| LsPlugin)
        .agent_plugin_factory(|| WritePlugin)
        .agent_plugin_factory(|| EditPlugin)
        .agent_plugin_factory(|| HashlineEditPlugin)
        .agent_plugin_factory(move || ConfiguredBashPlugin::new(bash_options.clone()));
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
            thinking_level: settings_thinking_level(config),
            thinking_budgets: settings_thinking_budgets(config),
            block_images: config.runtime_settings.images.block_images,
            active_tools: BUILTIN_TOOL_NAMES.map(str::to_string).to_vec(),
            cwd: config.cwd.clone(),
            max_tool_iterations: 100,
            steering_mode: settings_queue_mode(config.runtime_settings.steering_mode),
            follow_up_mode: settings_queue_mode(config.runtime_settings.follow_up_mode),
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Pi(Box::default()))
        .resources(resources)
        .build()?;

    let registered_tools = runtime
        .tool_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<HashSet<_>>();
    let mut active_tools = config
        .runtime_settings
        .default_tools
        .as_ref()
        .map_or_else(|| runtime.active_tools(), |configured| configured.clone());
    active_tools.retain(|tool| registered_tools.contains(tool));
    let mut seen = active_tools.iter().cloned().collect::<HashSet<_>>();
    for spec in runtime.tool_specs() {
        if !BUILTIN_TOOL_NAMES.contains(&spec.name.as_str()) && seen.insert(spec.name.clone()) {
            active_tools.push(spec.name);
        }
    }
    runtime.set_active_tools(active_tools)?;

    ModelRuntimeServices::new(&runtime)
        .select_initial_model(initial_model_request(config))
        .map_err(|error| RuntimeError::Build(error.to_string()))?;

    Ok(runtime)
}

fn provider_transport(config: &AppConfig) -> Result<Arc<dyn HttpTransport>, RuntimeError> {
    let transport = ReqwestTransport::with_config(provider_transport_config(config))
        .map_err(|error| RuntimeError::Build(error.to_string()))?;
    Ok(Arc::new(transport))
}

fn provider_transport_config(config: &AppConfig) -> ReqwestTransportConfig {
    let provider_retry = config.runtime_settings.retry.provider;
    let timeout_ms = provider_retry
        .timeout_ms
        .unwrap_or(config.runtime_settings.http_idle_timeout_ms);
    ReqwestTransportConfig {
        timeout: Some(std::time::Duration::from_millis(timeout_ms)),
        user_agent: Some(format!("pi-rs/{}", env!("CARGO_PKG_VERSION"))),
        proxy: config.runtime_settings.http_proxy.clone(),
        max_retries: provider_retry.max_retries.unwrap_or(0),
        max_retry_delay: std::time::Duration::from_millis(provider_retry.max_retry_delay_ms),
    }
}

fn codex_transport_options(config: &AppConfig) -> CodexTransportOptions {
    let timeout_ms = config
        .runtime_settings
        .retry
        .provider
        .timeout_ms
        .unwrap_or(config.runtime_settings.http_idle_timeout_ms);
    CodexTransportOptions {
        transport: match config.runtime_settings.transport {
            TransportSetting::Sse => CodexTransport::Sse,
            TransportSetting::Websocket => CodexTransport::Websocket,
            TransportSetting::WebsocketCached => CodexTransport::WebsocketCached,
            TransportSetting::Auto => CodexTransport::Auto,
        },
        websocket_connect_timeout: config
            .runtime_settings
            .websocket_connect_timeout_ms
            .map_or(Some(std::time::Duration::from_secs(15)), |timeout_ms| {
                (timeout_ms != 0).then(|| std::time::Duration::from_millis(timeout_ms))
            }),
        websocket_idle_timeout: (timeout_ms != 0)
            .then(|| std::time::Duration::from_millis(timeout_ms)),
        http_proxy_configured: config
            .runtime_settings
            .http_proxy
            .as_deref()
            .is_some_and(|proxy| !proxy.trim().is_empty()),
        base_url: None,
    }
}

fn initial_model_request(config: &AppConfig) -> InitialModelRequest {
    InitialModelRequest {
        requested_provider: config.requested_provider.clone().map(ProviderId::new),
        requested_model: config.model.clone(),
        settings_provider: config
            .runtime_settings
            .default_provider
            .clone()
            .map(ProviderId::new),
        settings_model: config.runtime_settings.default_model.clone(),
        ..InitialModelRequest::default()
    }
}

fn settings_queue_mode(mode: QueueModeSetting) -> QueueMode {
    match mode {
        QueueModeSetting::All => QueueMode::All,
        QueueModeSetting::OneAtATime => QueueMode::OneAtATime,
    }
}

fn settings_thinking_level(config: &AppConfig) -> ThinkingLevel {
    match config.runtime_settings.default_thinking_level {
        Some(ThinkingLevelSetting::Off) | None => ThinkingLevel::Off,
        Some(ThinkingLevelSetting::Minimal) => ThinkingLevel::Minimal,
        Some(ThinkingLevelSetting::Low) => ThinkingLevel::Low,
        Some(ThinkingLevelSetting::Medium) => ThinkingLevel::Medium,
        Some(ThinkingLevelSetting::High) => ThinkingLevel::High,
        Some(ThinkingLevelSetting::XHigh) => ThinkingLevel::XHigh,
        Some(ThinkingLevelSetting::Max) => ThinkingLevel::Max,
    }
}

fn settings_thinking_budgets(config: &AppConfig) -> Option<ThinkingBudgets> {
    config
        .runtime_settings
        .thinking_budgets
        .map(|budgets| ThinkingBudgets {
            minimal: budgets.minimal,
            low: budgets.low,
            medium: budgets.medium,
            high: budgets.high,
        })
}

fn session_compaction_settings(config: &AppConfig) -> SessionCompactionSettings {
    let settings = config.runtime_settings.compaction;
    SessionCompactionSettings {
        enabled: settings.enabled,
        reserve_tokens: settings.reserve_tokens,
        keep_recent_tokens: settings.keep_recent_tokens,
    }
}

fn scoped_setting_paths(
    global: &[String],
    global_base: &std::path::Path,
    project: &[String],
    project_base: &std::path::Path,
) -> Vec<PathBuf> {
    global
        .iter()
        .map(|path| setting_path(path, global_base))
        .chain(project.iter().map(|path| setting_path(path, project_base)))
        .collect()
}

fn setting_path(path: &str, base: &std::path::Path) -> PathBuf {
    let path = expand_tilde_path(path);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn expand_tilde_path(path: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        if path == "~" {
            return PathBuf::from(home);
        }
        if let Some(relative) = path.strip_prefix("~/") {
            return PathBuf::from(home).join(relative);
        }
    }
    PathBuf::from(path)
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
            _context: pi_js_plugin::ExtensionContextHandle,
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
                provider_registrations: Vec::new(),
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
kind = "agent"
artifact = "plugin.dylib"

[options]
command = "fixture-command"
"#,
        )
        .unwrap();
        package
    }

    #[test]
    fn native_inventory_only_keeps_loaded_plugins_from_plugins_json() {
        let plugins = retain_loaded_configured_native_plugins(
            [
                "configured".to_string(),
                "not-loaded".to_string(),
                "configured".to_string(),
            ],
            ["configured".to_string(), "explicit-path".to_string()],
        );

        assert_eq!(plugins, [PluginId::new("configured")]);
    }

    #[test]
    fn javascript_inventory_uses_package_sources_without_hiding_local_extensions() {
        let labels = javascript_inventory_labels(&[
            ResolvedExtensionIdentity::Package("npm:@counterposition/pi-web-search".to_string()),
            ResolvedExtensionIdentity::Package("npm:@narumitw/pi-lsp@0.49.5".to_string()),
            ResolvedExtensionIdentity::Path(PathBuf::from(
                "/workspace/.pi/extensions/clipboard.ts",
            )),
            ResolvedExtensionIdentity::Path(PathBuf::from(
                "/workspace/local/session-tools/index.ts",
            )),
        ]);

        assert_eq!(
            labels,
            [
                "npm:@counterposition/pi-web-search",
                "npm:@narumitw/pi-lsp@0.49.5",
                "clipboard.ts",
                "session-tools",
            ]
        );
    }

    #[test]
    fn javascript_provider_candidate_is_compiled_into_the_runtime_generation() {
        let directory = tempfile::tempdir().unwrap();
        let overlay = DynamicProviderOverlay::default();
        let candidate = overlay
            .candidate(&[pi_js_plugin::JsProviderRegistration {
                plugin_id: "js:0:provider.ts".to_string(),
                path: "/provider.ts".to_string(),
                name: "extension-provider".to_string(),
                config: serde_json::json!({
                    "baseUrl": "https://extension.example/v1",
                    "apiKey": "test-key",
                    "api": "openai-responses",
                    "models": [{
                        "id": "extension-model",
                        "name": "Extension Model",
                        "reasoning": true,
                        "input": ["text"],
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                        "contextWindow": 64000,
                        "maxTokens": 4096
                    }]
                }),
            }])
            .unwrap();

        let runtime = build_runtime_with_codex_credentials(
            &app_config(directory.path(), None),
            false,
            &NativePlugins::default(),
            None,
            Some(&candidate),
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();
        let model = runtime
            .model(
                &ProviderId::new("extension-provider"),
                &ModelId::new("extension-model"),
            )
            .unwrap();

        assert_eq!(model.name, "Extension Model");
        assert_eq!(model.context_window, 64_000);
        assert_eq!(
            model.base_url.as_deref(),
            Some("https://extension.example/v1")
        );
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
            extension_flag_values: std::collections::BTreeMap::new(),
            runtime_settings: pi_settings::SettingsValues::default(),
            settings_skill_paths: Vec::new(),
            settings_prompt_paths: Vec::new(),
            settings_diagnostics: Vec::new(),
        }
    }

    #[test]
    fn current_network_settings_build_generation_local_transport_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = app_config(directory.path(), None);
        config.runtime_settings.http_proxy = Some("http://proxy.example:8080".to_string());
        config.runtime_settings.http_idle_timeout_ms = 321_000;
        config.runtime_settings.transport = TransportSetting::WebsocketCached;
        config.runtime_settings.websocket_connect_timeout_ms = Some(9_876);
        config.runtime_settings.retry.provider = pi_settings::ProviderRetrySettings {
            timeout_ms: Some(12_345),
            max_retries: Some(2),
            max_retry_delay_ms: 4_567,
        };

        let transport = provider_transport_config(&config);

        assert_eq!(
            transport.timeout,
            Some(std::time::Duration::from_millis(12_345))
        );
        assert_eq!(
            transport.proxy.as_deref(),
            Some("http://proxy.example:8080")
        );
        assert_eq!(transport.max_retries, 2);
        assert_eq!(
            transport.max_retry_delay,
            std::time::Duration::from_millis(4_567)
        );
        let codex = codex_transport_options(&config);
        assert_eq!(codex.transport, CodexTransport::WebsocketCached);
        assert_eq!(
            codex.websocket_connect_timeout,
            Some(std::time::Duration::from_millis(9_876))
        );
        assert_eq!(
            codex.websocket_idle_timeout,
            Some(std::time::Duration::from_millis(12_345))
        );
        assert!(codex.http_proxy_configured);

        config.runtime_settings.retry.provider.timeout_ms = None;
        assert_eq!(
            provider_transport_config(&config).timeout,
            Some(std::time::Duration::from_millis(321_000))
        );
        config.runtime_settings.websocket_connect_timeout_ms = Some(0);
        assert_eq!(
            codex_transport_options(&config).websocket_connect_timeout,
            None
        );
    }

    #[test]
    fn current_settings_configure_runtime_tools_thinking_queues_and_compaction() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = app_config(directory.path(), None);
        config.runtime_settings.default_thinking_level = Some(ThinkingLevelSetting::High);
        config.runtime_settings.thinking_budgets = Some(pi_settings::ThinkingBudgetsSettings {
            minimal: Some(111),
            low: None,
            medium: None,
            high: Some(999),
        });
        config.runtime_settings.default_tools =
            Some(vec!["read".to_string(), "not-registered".to_string()]);
        config.runtime_settings.steering_mode = QueueModeSetting::All;
        config.runtime_settings.follow_up_mode = QueueModeSetting::OneAtATime;
        config.runtime_settings.compaction = pi_settings::CompactionSettings {
            enabled: false,
            reserve_tokens: 123,
            keep_recent_tokens: 456,
        };

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();
        let state = runtime.agent().state();

        assert_eq!(state.thinking_level, ThinkingLevel::High);
        assert_eq!(
            runtime.agent().thinking_budgets(),
            Some(ThinkingBudgets {
                minimal: Some(111),
                low: None,
                medium: None,
                high: Some(999),
            })
        );
        assert_eq!(state.active_tools, ["read"]);
        assert_eq!(
            settings_queue_mode(config.runtime_settings.steering_mode),
            QueueMode::All
        );
        assert_eq!(
            settings_queue_mode(config.runtime_settings.follow_up_mode),
            QueueMode::OneAtATime
        );
        assert_eq!(
            session_compaction_settings(&config),
            SessionCompactionSettings {
                enabled: false,
                reserve_tokens: 123,
                keep_recent_tokens: 456,
            }
        );
    }

    #[test]
    fn current_setting_paths_use_scope_bases_and_expand_tilde() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("agent");
        let project = directory.path().join("project/.pi");
        let resolved = scoped_setting_paths(
            &["skills/global".to_string()],
            &global,
            &["../shared".to_string(), "/absolute/skill".to_string()],
            &project,
        );

        assert_eq!(resolved[0], global.join("skills/global"));
        assert_eq!(resolved[1], project.join("../shared"));
        assert_eq!(resolved[2], PathBuf::from("/absolute/skill"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_tilde_path("~"), PathBuf::from(home));
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
    fn stored_google_api_key_registers_and_selects_builtin_catalog() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("auth.json"),
            serde_json::json!({
                "google": {"type": "api_key", "key": "gemini-test-key"}
            })
            .to_string(),
        )
        .unwrap();
        let mut config = app_config(directory.path(), Some("gemini-3.1-pro-preview"));
        config.provider = "google".to_string();
        config.requested_provider = Some("google".to_string());

        let runtime = build_runtime_with_codex_credentials(
            &config,
            false,
            &NativePlugins::default(),
            None,
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        let model = runtime
            .model(
                &ProviderId::new("google"),
                &ModelId::new("gemini-3.1-pro-preview"),
            )
            .expect("the built-in Google catalog must be registered");
        assert_eq!(model.context_window, 1_048_576);
        assert_eq!(runtime.agent().state().provider_id.as_str(), "google");
        assert!(runtime.available_models().iter().any(|model| {
            model.provider == ProviderId::new("google")
                && model.id == ModelId::new("gemini-3.1-pro-preview")
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
                .with_js_plugin_host(
                    host.clone(),
                    JsHostMode::Tui,
                    ExtensionSessionBinding::new(),
                );
        let runtime = pi_session::AgentSessionRuntime::create(
            factory,
            AgentSessionRuntimeTarget::create(&project, &config.session_path),
        )
        .await
        .unwrap();

        assert_eq!(
            runtime.session().runtime_inventory().js_extensions(),
            [package_source]
        );

        runtime.reload().await.unwrap();
        assert_eq!(
            runtime.session().runtime_inventory().js_extensions(),
            [package_source]
        );
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
        assert!(requests.iter().all(|request| request.project_trusted));
        assert!(
            requests
                .iter()
                .all(|request| request.extension_paths == [extension.display().to_string()])
        );
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
            build_runtime(&config, true, &NativePlugins::default(), None, None).unwrap(),
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
