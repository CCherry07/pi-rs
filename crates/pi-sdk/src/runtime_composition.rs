//! First-party runtime composition and cross-plugin wiring for one candidate generation.

use std::collections::HashSet;
use std::sync::Arc;

use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId};
use pi_js_plugin::JsPluginGeneration;
use pi_memory_loader::{MemoryLoader, MemoryLoaderOptions, PreparedMemoryProvider};
use pi_plugin::PluginContext;
use pi_plugin_bash::{BashToolOptions, ConfiguredBashPlugin};
use pi_plugin_edit::EditPlugin;
use pi_plugin_find::FindPlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_hashline_edit::HashlineEditPlugin;
use pi_plugin_ls::LsPlugin;
use pi_plugin_manager::loader::NativePlugins;
use pi_plugin_memory_hermes::{HermesMemoryProviderFactory, managed_skill_roots};
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use pi_plugin_prompts::{PromptTemplateLoaderOptions, PromptTemplatesPlugin};
use pi_plugin_read::ConfiguredReadPlugin;
use pi_plugin_schedule::{ScheduleOptions, SchedulePlugin};
use pi_plugin_session_transfer::SessionTransferPlugin;
use pi_plugin_skills::SkillsPlugin;
use pi_plugin_subagents::{
    SubagentLoaderOptions, SubagentRuntime, SubagentSkillPromptProjector, SubagentsPlugin,
};
use pi_plugin_write::WritePlugin;
use pi_resources::ResourceLoaderOptions;
use pi_runtime::{CompletionRetryPolicy, PiRuntime, RuntimeError, SystemPrompt};
use pi_session::SessionGenerationOverlay;

use crate::builtin_providers::BuiltinProviderSet;
use crate::configuration::{
    initial_model_request, settings_queue_mode, settings_thinking_budgets, settings_thinking_level,
};
use crate::dynamic_providers::DynamicProviderCandidate;
use crate::{Config, expand_tilde_path};

const BUILTIN_TOOL_NAMES: [&str; 17] = [
    "read",
    "grep",
    "find",
    "ls",
    "write",
    "edit",
    "hashline_edit",
    "bash",
    "spawn_agent",
    "send_message",
    "followup_task",
    "wait_agent",
    "interrupt_agent",
    "list_agents",
    "memory",
    "session_search",
    "schedule",
];

/// Borrowed, already-prepared components for unified runtime registration.
/// Configuration, runtime capabilities and activation guards are deliberately separate.
pub(crate) struct GenerationComponents<'a> {
    pub(crate) native: &'a NativePlugins,
    pub(crate) javascript: Option<&'a JsPluginGeneration>,
    pub(crate) mcp: Option<&'a Arc<dyn pi_plugin::Plugin>>,
    pub(crate) memory: Option<&'a PreparedMemoryProvider>,
    pub(crate) subagents: &'a SubagentRuntime,
}

pub(crate) struct RuntimeBuildOutcome {
    pub(crate) runtime: PiRuntime,
    pub(crate) initial_model_fallback_message: Option<String>,
}

impl GenerationComponents<'_> {
    pub(crate) fn build_runtime(
        &self,
        config: &Config,
        project_trusted: bool,
        context: Arc<dyn PluginContext>,
        overlay: &SessionGenerationOverlay,
        dynamic_providers: Option<&DynamicProviderCandidate>,
        builtin_providers: BuiltinProviderSet,
    ) -> Result<RuntimeBuildOutcome, RuntimeError> {
        let memory_enabled = self.memory.is_some();
        let memory_is_hermes = self
            .memory
            .is_some_and(|memory| memory.provider_id() == "hermes");
        let mut skill_activity_observer = None;
        let transport = builtin_providers.transport();
        let effective_api_key = builtin_providers.effective_api_key().map(str::to_string);
        let skill_options =
            crate::skills::runtime_skill_options(config, project_trusted, memory_is_hermes);
        if config.features.skills && memory_is_hermes {
            skill_activity_observer = Some(pi_plugin_memory_hermes::curator::activity_observer(
                managed_skill_roots(&config.agent_dir, &config.cwd, project_trusted),
            ));
        }
        let mut prompt_template_options =
            PromptTemplateLoaderOptions::new(&config.cwd, &config.agent_dir);
        prompt_template_options.project_trusted = project_trusted;
        prompt_template_options
            .additional_paths
            .extend(config.settings_prompt_paths.iter().cloned());
        let mut subagent_options = SubagentLoaderOptions::new(&config.cwd, &config.agent_dir);
        subagent_options.project_trusted = project_trusted;
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
        let builder = builtin_providers.register(builder.plugin_context(context), config);
        let skill_prompt_projector = config
            .features
            .subagents
            .then(|| Arc::new(SubagentSkillPromptProjector::new(self.subagents.clone())));
        let mut builder = builder.try_provider_plugin_factory({
            let model_options = model_options.clone();
            let transport = Arc::clone(&transport);
            move || ModelsPlugin::load_with_transport(model_options.clone(), Arc::clone(&transport))
        });
        if config.features.prompt_templates {
            builder = builder.plugin_factory({
                let prompt_template_options = prompt_template_options.clone();
                move || PromptTemplatesPlugin::load(prompt_template_options.clone())
            });
        }
        let builder = match self.memory {
            Some(memory) => {
                let plugin = memory.plugin();
                builder.try_plugin_arc_factory(move || Ok::<_, String>(Arc::clone(&plugin)))
            }
            None => builder,
        };
        let mut builder = builder;
        if config.features.subagents {
            builder = builder.try_plugin_factory({
                let subagents = self.subagents.clone();
                let subagent_options = subagent_options.clone();
                move || SubagentsPlugin::load(subagents.clone(), subagent_options.clone())
            });
        }
        if config.features.skills {
            builder = builder.plugin_factory({
                let skill_options = skill_options.clone();
                let skill_activity_observer = skill_activity_observer.clone();
                move || {
                    let plugin = match &skill_prompt_projector {
                        Some(projector) => SkillsPlugin::load_with_prompt_projector(
                            skill_options.clone(),
                            projector.clone(),
                        ),
                        None => SkillsPlugin::load(skill_options.clone()),
                    };
                    plugin.with_activity_observer(skill_activity_observer.clone())
                }
            });
        }
        if config.features.session_transfer {
            builder = builder.plugin_factory(SessionTransferPlugin::default);
        }
        if config.features.schedule {
            builder = builder.plugin_factory({
                let options = ScheduleOptions::new(&config.cwd, &config.agent_dir, project_trusted);
                move || SchedulePlugin::new(options.clone())
            });
        }
        let builder = builder
            .plugin_factory({
                let auto_resize_images = config.runtime_settings.images.auto_resize;
                move || ConfiguredReadPlugin::new(auto_resize_images)
            })
            .plugin_factory(|| GrepPlugin)
            .plugin_factory(|| FindPlugin)
            .plugin_factory(|| LsPlugin)
            .plugin_factory(|| WritePlugin)
            .plugin_factory(|| EditPlugin)
            .plugin_factory(|| HashlineEditPlugin)
            .plugin_factory(move || ConfiguredBashPlugin::new(bash_options.clone()));
        let mut builder = self.native.apply_runtime(builder);
        if let Some(plugin) = self.mcp {
            let plugin = Arc::clone(plugin);
            builder = builder.try_plugin_arc_factory(move || Ok::<_, String>(Arc::clone(&plugin)));
        }
        if let Some(js_generation) = self.javascript {
            for plugin in js_generation.plugins() {
                builder = builder.try_plugin_arc_factory({
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
        builder = overlay.apply_to(builder);

        let mut resources = ResourceLoaderOptions::new(&config.cwd, &config.agent_dir);
        resources.project_trusted = project_trusted;

        let runtime = builder
            .agent_options(AgentOptions {
                provider_id: ProviderId::new(config.provider.clone()),
                model_id: ModelId::new(config.model.as_deref().unwrap_or(&config.fallback_model)),
                thinking_level: settings_thinking_level(
                    config.thinking,
                    config.runtime_settings.default_thinking_level,
                ),
                thinking_budgets: settings_thinking_budgets(&config.runtime_settings),
                block_images: config.runtime_settings.images.block_images,
                active_tools: BUILTIN_TOOL_NAMES
                    .into_iter()
                    .filter(|tool| match *tool {
                        "memory" | "session_search" => memory_enabled,
                        "spawn_agent" | "send_message" | "followup_task" | "wait_agent"
                        | "interrupt_agent" | "list_agents" => config.features.subagents,
                        "schedule" => config.features.schedule,
                        _ => true,
                    })
                    .map(str::to_string)
                    .collect(),
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

        let selection = initial_model_request(
            config.requested_provider.as_deref(),
            config.model.as_deref(),
            &config.runtime_settings,
        )
        .select(&runtime)
        .map_err(|error| RuntimeError::Build(error.to_string()))?;

        Ok(RuntimeBuildOutcome {
            runtime,
            initial_model_fallback_message: selection.fallback_message,
        })
    }
}

/// Product extension activation policy; managed children retain their tool ceiling.
pub(crate) fn additional_active_tools(runtime: &PiRuntime) -> Vec<String> {
    runtime
        .active_tools()
        .into_iter()
        .filter(|name| {
            runtime.execution_origin() == pi_plugin::SessionExecutionOrigin::User
                && !BUILTIN_TOOL_NAMES.contains(&name.as_str())
        })
        .collect()
}

pub(crate) async fn prepare_memory_provider(
    enabled: bool,
    options: MemoryLoaderOptions,
) -> Result<Option<PreparedMemoryProvider>, String> {
    if !enabled {
        return Ok(None);
    }
    MemoryLoader::new(options)
        .provider_factory(HermesMemoryProviderFactory)
        .load()
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_providers::DynamicProviderOverlay;
    use crate::test_support::app_config;
    use pi_agent::QueueMode;
    use pi_core::{ThinkingBudgets, ThinkingLevel};
    use pi_session::{
        AgentSession, AgentSessionOptions, CompactionSettings as SessionCompactionSettings,
    };
    use pi_settings::{QueueModeSetting, ThinkingLevelSetting};

    fn build_test_runtime(
        config: &Config,
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
        config: &Config,
        project_trusted: bool,
        native_plugins: &NativePlugins,
        js_generation: Option<&JsPluginGeneration>,
        dynamic_providers: Option<&DynamicProviderCandidate>,
        codex_credentials: Option<pi_plugin_openai::CodexCredentials>,
    ) -> Result<PiRuntime, RuntimeError> {
        GenerationComponents {
            native: native_plugins,
            javascript: js_generation,
            mcp: None,
            memory: None,
            subagents: &SubagentRuntime::default(),
        }
        .build_runtime(
            config,
            project_trusted,
            Arc::new(pi_plugin::UnavailablePluginContext),
            &SessionGenerationOverlay::default(),
            dynamic_providers,
            BuiltinProviderSet::prepare_for_test(config, codex_credentials, None)?,
        )
        .map(|built| built.runtime)
    }

    async fn build_runtime_with_first_party_memory(
        config: &Config,
    ) -> Result<PiRuntime, RuntimeError> {
        let memory = prepare_memory_provider(
            config.features.memory,
            crate::configuration::memory_options(
                &config.cwd,
                &config.agent_dir,
                &config.session_path,
                None,
                false,
            ),
        )
        .await
        .map_err(RuntimeError::Build)?;
        GenerationComponents {
            native: &NativePlugins::default(),
            javascript: None,
            mcp: None,
            memory: memory.as_ref(),
            subagents: &SubagentRuntime::default(),
        }
        .build_runtime(
            config,
            false,
            Arc::new(pi_plugin::UnavailablePluginContext),
            &SessionGenerationOverlay::default(),
            None,
            BuiltinProviderSet::prepare_for_test(
                config,
                Some(pi_plugin_openai::CodexCredentials::default()),
                None,
            )?,
        )
        .map(|built| built.runtime)
    }

    #[tokio::test]
    async fn prepared_memory_is_retained_by_both_runtime_and_session_registrations() {
        for drop_runtime_first in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut config = app_config(directory.path(), None);
            config.features = crate::Features {
                memory: true,
                ..crate::Features::none()
            };
            let (runtime, session_plugins, provider) = {
                let memory = prepare_memory_provider(
                    true,
                    crate::configuration::memory_options(
                        &config.cwd,
                        &config.agent_dir,
                        &config.session_path,
                        None,
                        false,
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                let provider = Arc::downgrade(&memory.plugin());
                let components = GenerationComponents {
                    native: &NativePlugins::default(),
                    javascript: None,
                    mcp: None,
                    memory: Some(&memory),
                    subagents: &SubagentRuntime::default(),
                };
                let runtime = components
                    .build_runtime(
                        &config,
                        false,
                        Arc::new(pi_plugin::UnavailablePluginContext),
                        &SessionGenerationOverlay::default(),
                        None,
                        BuiltinProviderSet::prepare_for_test(
                            &config,
                            Some(pi_plugin_openai::CodexCredentials::default()),
                            None,
                        )
                        .unwrap(),
                    )
                    .unwrap()
                    .runtime;
                let session_plugins = runtime.plugin_driver();
                // The runtime and this view retain the same driver and prepared provider.
                (runtime, session_plugins, provider)
            };
            assert!(provider.upgrade().is_some());
            if drop_runtime_first {
                drop(runtime);
                assert!(
                    provider.upgrade().is_some(),
                    "session registrations must retain the same provider"
                );
                drop(session_plugins);
            } else {
                drop(session_plugins);
                assert!(
                    provider.upgrade().is_some(),
                    "runtime registrations must retain the same provider"
                );
                drop(runtime);
            }
            assert!(
                provider.upgrade().is_none(),
                "component view must not create an extra lifetime owner"
            );
        }
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

    #[test]
    fn product_runtime_registers_schedule_without_creating_storage() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = build_runtime_with_codex_credentials(
            &app_config(directory.path(), None),
            false,
            &NativePlugins::default(),
            None,
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();
        assert!(runtime.active_tools().iter().any(|tool| tool == "schedule"));
        assert!(
            runtime
                .command_specs()
                .iter()
                .any(|command| command.name == "schedule")
        );
        assert!(!directory.path().join("schedule").exists());
    }

    #[test]
    fn product_runtime_registers_the_agent_collaboration_tools() {
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

        for name in [
            "spawn_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "interrupt_agent",
            "list_agents",
        ] {
            assert!(runtime.active_tools().iter().any(|tool| tool == name));
        }
        assert!(!runtime.active_tools().iter().any(|tool| tool == "subagent"));
        let spec = runtime
            .tool_specs()
            .into_iter()
            .find(|spec| spec.name == "spawn_agent")
            .expect("spawn_agent should be registered");
        assert_eq!(
            spec.parameters["properties"]["agent"]["enum"],
            serde_json::json!([
                "scout",
                "worker",
                "developer",
                "coder",
                "implementer",
                "develop",
                "reviewer",
                "oracle",
                "advisor",
                "delegate"
            ])
        );
        assert!(spec.parameters.get("oneOf").is_none());
        assert!(spec.parameters["properties"].get("stages").is_none());
    }

    #[tokio::test]
    async fn product_runtime_registers_hermes_memory_tools_commands_and_skills() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("pi-hermes-memory/skills/deploy-demo"))
            .unwrap();
        std::fs::write(
            directory
                .path()
                .join("pi-hermes-memory/skills/deploy-demo/SKILL.md"),
            "---\nname: deploy-demo\ndescription: Deploy the demo safely\n---\n\n## Procedure\n1. Run tests\n",
        )
        .unwrap();
        let config = app_config(directory.path(), None);
        let runtime = build_runtime_with_first_party_memory(&config)
            .await
            .unwrap();
        assert!(runtime.active_tools().iter().any(|tool| tool == "memory"));
        assert!(
            runtime
                .active_tools()
                .iter()
                .any(|tool| tool == "session_search")
        );
        assert!(
            runtime
                .active_tools()
                .iter()
                .any(|tool| tool == "skill_manage")
        );
        assert!(
            runtime
                .command_specs()
                .iter()
                .any(|command| command.name == "skill:deploy-demo")
        );
        for command in [
            "memory-consolidate",
            "memory-index-sessions",
            "memory-insights",
            "memory-interview",
            "learn-memory-tool",
            "memory-preview-context",
            "memory-skills",
            "memory-sync-markdown",
        ] {
            assert!(
                runtime
                    .command_specs()
                    .iter()
                    .any(|registered| registered.name == command),
                "expected Hermes command {command}"
            );
        }
        std::fs::write(
            directory.path().join("memory.json"),
            r#"{"version": 1, "enabled": false}"#,
        )
        .unwrap();
        let disabled = build_runtime_with_first_party_memory(&config)
            .await
            .unwrap();
        assert!(
            !disabled
                .tool_specs()
                .iter()
                .any(|tool| tool.name == "memory")
        );
        assert!(
            !disabled
                .command_specs()
                .iter()
                .any(|command| command.name.starts_with("memory-"))
        );
    }

    #[tokio::test]
    async fn removed_local_memory_provider_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("memory.json"),
            r#"{"version": 1, "provider": "local"}"#,
        )
        .unwrap();
        let config = app_config(directory.path(), None);

        let error = match build_runtime_with_first_party_memory(&config).await {
            Ok(_) => panic!("the removed local provider must not be accepted"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "runtime build failed: memory.json selects unknown provider local; registered providers: hermes"
        );
    }

    #[test]
    fn trusted_project_markdown_agents_extend_the_subagent_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(project.join(".pi/agents")).unwrap();
        std::fs::write(
            project.join(".pi/agents/project-scout.md"),
            "---\nname: project-scout\ndescription: Project-only scout\nsystemPromptMode: append\n---\nInspect this project.",
        )
        .unwrap();
        let mut config = app_config(&agent_dir, None);
        config.cwd = project;

        let build = |project_trusted| {
            build_runtime_with_codex_credentials(
                &config,
                project_trusted,
                &NativePlugins::default(),
                None,
                None,
                Some(pi_plugin_openai::CodexCredentials::default()),
            )
            .unwrap()
        };
        let trusted = build(true);
        let trusted_spec = trusted
            .tool_specs()
            .into_iter()
            .find(|spec| spec.name == "spawn_agent")
            .unwrap();
        assert!(
            trusted_spec.parameters["properties"]["agent"]["enum"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("project-scout"))
        );

        let untrusted = build(false);
        let untrusted_spec = untrusted
            .tool_specs()
            .into_iter()
            .find(|spec| spec.name == "spawn_agent")
            .unwrap();
        assert!(
            !untrusted_spec.parameters["properties"]["agent"]["enum"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("project-scout"))
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
            crate::configuration::session_compaction_settings(config.runtime_settings.compaction),
            SessionCompactionSettings {
                enabled: false,
                reserve_tokens: 123,
                keep_recent_tokens: 456,
            }
        );
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
    fn expanded_provider_catalogs_use_stored_auth_and_copilot_account_filtering() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("auth.json"),
            serde_json::json!({
                "amazon-bedrock": {"type": "api_key", "key": "bedrock-bearer"},
                "google-vertex": {"type": "api_key", "key": "vertex-key"},
                "mistral": {"type": "api_key", "key": "mistral-key"},
                "openrouter": {
                    "type": "oauth",
                    "access": "openrouter-key",
                    "refresh": "",
                    "expires": 9_007_199_254_740_991_f64
                },
                "github-copilot": {
                    "type": "oauth",
                    "access": "copilot-token",
                    "refresh": "github-token",
                    "expires": 4_102_444_800_000_f64,
                    "availableModelIds": ["gpt-4.1"]
                }
            })
            .to_string(),
        )
        .unwrap();

        let runtime = build_runtime_with_codex_credentials(
            &app_config(directory.path(), None),
            false,
            &NativePlugins::default(),
            None,
            None,
            Some(pi_plugin_openai::CodexCredentials::default()),
        )
        .unwrap();

        for (provider, model) in [
            ("amazon-bedrock", "amazon.nova-2-lite-v1:0"),
            ("azure-openai-responses", "gpt-5.4"),
            ("google-vertex", "gemini-2.5-flash"),
            ("mistral", "mistral-small-latest"),
            ("openrouter", "openai/gpt-5.4"),
            ("github-copilot", "gpt-4.1"),
        ] {
            assert!(
                runtime
                    .model(&ProviderId::new(provider), &ModelId::new(model))
                    .is_some(),
                "missing {provider}/{model}"
            );
        }
        for provider in [
            "amazon-bedrock",
            "google-vertex",
            "mistral",
            "openrouter",
            "github-copilot",
        ] {
            assert!(
                runtime
                    .available_models()
                    .iter()
                    .any(|model| model.provider == ProviderId::new(provider)),
                "{provider} should be available"
            );
        }
        assert!(
            runtime
                .model(&ProviderId::new("github-copilot"), &ModelId::new("gpt-5.4"))
                .is_none()
        );
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

        let runtime = build_test_runtime(
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

        let runtime = build_test_runtime(
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
            build_test_runtime(
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
            build_test_runtime(&config, true, &NativePlugins::default(), None, None).unwrap(),
            &path,
            AgentSessionOptions::default().initial_model(initial_model_request(
                config.requested_provider.as_deref(),
                config.model.as_deref(),
                &config.runtime_settings,
            )),
        )
        .await
        .unwrap();
        let state = resumed.runtime().agent().state();

        assert_eq!(state.provider_id.as_str(), "catalog-provider");
        assert_eq!(state.model_id.as_str(), "alpha");
        resumed.shutdown().await;
    }
}
