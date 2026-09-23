//! Product settings adapters. Domain crates consume their own typed options;
//! loading, trust resolution and generation activation remain with the factory.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pi_agent::QueueMode;
use pi_core::{ProviderId, ThinkingBudgets, ThinkingLevel};
use pi_memory_loader::MemoryLoaderOptions;
use pi_plugin_openai::{CodexTransport, CodexTransportOptions};
use pi_provider::ReqwestTransportConfig;
use pi_session::{AgentSessionOptions, AutoRetrySettings, CompactionSettings, InitialModelRequest};
use pi_settings::{
    QueueModeSetting, SettingsSnapshot, SettingsValues, ThinkingLevelSetting, TransportSetting,
};

use crate::{Config, expand_tilde_path};

pub(crate) fn apply_settings(config: &mut Config, settings: &SettingsSnapshot) {
    config.runtime_settings = settings.effective().clone();
    // Pi treats the proxy as process/bootstrap configuration. A trusted
    // project may tune request behavior, but cannot redirect HTTP traffic.
    config.runtime_settings.http_proxy = settings.global().http_proxy.clone();
    config.settings_diagnostics = settings
        .diagnostics()
        .iter()
        .map(|diagnostic| pi_runtime::ResourceDiagnostic {
            kind: pi_runtime::DiagnosticKind::Warning,
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
}

pub(crate) fn scoped_setting_paths(
    global: &[String],
    global_base: &Path,
    project: &[String],
    project_base: &Path,
) -> Vec<PathBuf> {
    global
        .iter()
        .map(|path| setting_path(path, global_base))
        .chain(project.iter().map(|path| setting_path(path, project_base)))
        .collect()
}

fn setting_path(path: &str, base: &Path) -> PathBuf {
    let path = expand_tilde_path(path);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

pub(crate) fn provider_transport_config(settings: &SettingsValues) -> ReqwestTransportConfig {
    let provider_retry = settings.retry.provider;
    let timeout_ms = provider_retry
        .timeout_ms
        .unwrap_or(settings.http_idle_timeout_ms);
    ReqwestTransportConfig {
        timeout: Some(Duration::from_millis(timeout_ms)),
        user_agent: Some(format!("pi-rs/{}", env!("CARGO_PKG_VERSION"))),
        proxy: settings.http_proxy.clone(),
        max_retries: provider_retry.max_retries.unwrap_or(0),
        max_retry_delay: Duration::from_millis(provider_retry.max_retry_delay_ms),
    }
}

pub(crate) fn codex_transport_options(settings: &SettingsValues) -> CodexTransportOptions {
    let timeout_ms = settings
        .retry
        .provider
        .timeout_ms
        .unwrap_or(settings.http_idle_timeout_ms);
    CodexTransportOptions {
        transport: match settings.transport {
            TransportSetting::Sse => CodexTransport::Sse,
            TransportSetting::Websocket => CodexTransport::Websocket,
            TransportSetting::WebsocketCached => CodexTransport::WebsocketCached,
            TransportSetting::Auto => CodexTransport::Auto,
        },
        websocket_connect_timeout: settings
            .websocket_connect_timeout_ms
            .map_or(Some(Duration::from_secs(15)), |timeout_ms| {
                (timeout_ms != 0).then(|| Duration::from_millis(timeout_ms))
            }),
        websocket_idle_timeout: (timeout_ms != 0).then(|| Duration::from_millis(timeout_ms)),
        http_proxy_configured: settings
            .http_proxy
            .as_deref()
            .is_some_and(|proxy| !proxy.trim().is_empty()),
        base_url: None,
    }
}

pub(crate) fn initial_model_request(
    requested_provider: Option<&str>,
    requested_model: Option<&str>,
    settings: &SettingsValues,
) -> InitialModelRequest {
    InitialModelRequest {
        requested_provider: requested_provider.map(ProviderId::new),
        requested_model: requested_model.map(str::to_string),
        settings_provider: settings.default_provider.clone().map(ProviderId::new),
        settings_model: settings.default_model.clone(),
        ..InitialModelRequest::default()
    }
}

pub(crate) fn settings_queue_mode(mode: QueueModeSetting) -> QueueMode {
    match mode {
        QueueModeSetting::All => QueueMode::All,
        QueueModeSetting::OneAtATime => QueueMode::OneAtATime,
    }
}

pub(crate) fn settings_thinking_level(
    explicit: Option<ThinkingLevel>,
    configured: Option<ThinkingLevelSetting>,
) -> ThinkingLevel {
    if let Some(level) = explicit {
        return level;
    }
    match configured {
        Some(ThinkingLevelSetting::Off) => ThinkingLevel::Off,
        Some(ThinkingLevelSetting::Minimal) => ThinkingLevel::Minimal,
        Some(ThinkingLevelSetting::Low) => ThinkingLevel::Low,
        Some(ThinkingLevelSetting::Medium) => ThinkingLevel::Medium,
        Some(ThinkingLevelSetting::High) => ThinkingLevel::High,
        Some(ThinkingLevelSetting::XHigh) => ThinkingLevel::XHigh,
        Some(ThinkingLevelSetting::Max) => ThinkingLevel::Max,
        None => ThinkingLevel::Medium,
    }
}

pub(crate) fn settings_thinking_budgets(settings: &SettingsValues) -> Option<ThinkingBudgets> {
    settings.thinking_budgets.map(|budgets| ThinkingBudgets {
        minimal: budgets.minimal,
        low: budgets.low,
        medium: budgets.medium,
        high: budgets.high,
    })
}

pub(crate) fn session_compaction_settings(
    settings: pi_settings::CompactionSettings,
) -> CompactionSettings {
    CompactionSettings {
        enabled: settings.enabled,
        reserve_tokens: settings.reserve_tokens,
        keep_recent_tokens: settings.keep_recent_tokens,
    }
}

pub(crate) fn memory_options(
    cwd: &Path,
    agent_dir: &Path,
    configured_session_path: &Path,
    active_session_path: Option<&Path>,
    project_trusted: bool,
) -> MemoryLoaderOptions {
    let mut options = MemoryLoaderOptions::new(cwd, agent_dir);
    options.project_trusted = project_trusted;
    if let Some(root) = configured_session_path.parent() {
        options.session_roots.push(root.to_path_buf());
    }
    if let Some(root) = active_session_path.and_then(Path::parent) {
        options.session_roots.push(root.to_path_buf());
    }
    options
}

pub(crate) fn session_options(
    settings: &SettingsValues,
    initial_model: InitialModelRequest,
) -> AgentSessionOptions {
    AgentSessionOptions::default()
        .compaction(session_compaction_settings(settings.compaction))
        .branch_summary_reserve_tokens(settings.branch_summary.reserve_tokens)
        .retry(AutoRetrySettings {
            enabled: settings.retry.enabled,
            max_retries: settings.retry.max_retries,
            base_delay_ms: settings.retry.base_delay_ms,
        })
        .initial_model(initial_model)
        .shell_executor(Arc::new(crate::session::CodingShellExecutor::new(
            settings.shell_path.as_deref().map(expand_tilde_path),
            settings.shell_command_prefix.clone(),
        )))
        .compaction_policy(Arc::new(crate::session::CodingCompactionPolicy))
}

#[cfg(test)]
mod tests {
    use pi_settings::{SettingsContext, SettingsManager};
    use serde_json::json;

    use super::*;

    #[test]
    fn current_network_settings_build_generation_local_transport_configuration() {
        let mut settings = SettingsValues {
            http_proxy: Some("http://proxy.example:8080".to_string()),
            http_idle_timeout_ms: 321_000,
            transport: TransportSetting::WebsocketCached,
            websocket_connect_timeout_ms: Some(9_876),
            ..SettingsValues::default()
        };
        settings.retry.provider = pi_settings::ProviderRetrySettings {
            timeout_ms: Some(12_345),
            max_retries: Some(2),
            max_retry_delay_ms: 4_567,
        };

        let transport = provider_transport_config(&settings);
        assert_eq!(transport.timeout, Some(Duration::from_millis(12_345)));
        assert_eq!(
            transport.proxy.as_deref(),
            Some("http://proxy.example:8080")
        );
        assert_eq!(transport.max_retries, 2);
        assert_eq!(transport.max_retry_delay, Duration::from_millis(4_567));
        let codex = codex_transport_options(&settings);
        assert_eq!(codex.transport, CodexTransport::WebsocketCached);
        assert_eq!(
            codex.websocket_connect_timeout,
            Some(Duration::from_millis(9_876))
        );
        assert_eq!(
            codex.websocket_idle_timeout,
            Some(Duration::from_millis(12_345))
        );
        assert!(codex.http_proxy_configured);

        settings.retry.provider.timeout_ms = None;
        assert_eq!(
            provider_transport_config(&settings).timeout,
            Some(Duration::from_millis(321_000))
        );
        settings.websocket_connect_timeout_ms = Some(0);
        assert_eq!(
            codex_transport_options(&settings).websocket_connect_timeout,
            None
        );
    }

    #[test]
    fn network_defaults_and_disabled_timeouts_keep_transport_semantics() {
        let mut settings = SettingsValues::default();
        let transport = provider_transport_config(&settings);
        assert_eq!(transport.timeout, Some(Duration::from_secs(300)));
        assert_eq!(transport.max_retries, 0);
        assert_eq!(transport.max_retry_delay, Duration::from_secs(60));
        assert_eq!(
            transport.user_agent,
            Some(format!("pi-rs/{}", env!("CARGO_PKG_VERSION")))
        );
        assert!(transport.proxy.is_none());
        let codex = codex_transport_options(&settings);
        assert_eq!(codex.transport, CodexTransport::Auto);
        assert_eq!(
            codex.websocket_connect_timeout,
            Some(Duration::from_secs(15))
        );
        assert!(!codex.http_proxy_configured);
        assert!(codex.base_url.is_none());

        settings.http_idle_timeout_ms = 0;
        settings.http_proxy = Some(" \t".into());
        // The HTTP transport interprets Some(0) itself; the WebSocket option uses None.
        assert_eq!(
            provider_transport_config(&settings).timeout,
            Some(Duration::ZERO)
        );
        let codex = codex_transport_options(&settings);
        assert_eq!(codex.websocket_idle_timeout, None);
        assert!(!codex.http_proxy_configured);
        for (setting, expected) in [
            (TransportSetting::Sse, CodexTransport::Sse),
            (TransportSetting::Websocket, CodexTransport::Websocket),
        ] {
            settings.transport = setting;
            assert_eq!(codex_transport_options(&settings).transport, expected);
        }
    }

    #[test]
    fn product_thinking_defaults_to_pi_medium() {
        assert_eq!(settings_thinking_level(None, None), ThinkingLevel::Medium);
    }

    #[test]
    fn explicit_thinking_wins_and_settings_budgets_and_queues_are_preserved() {
        for (configured, expected) in [
            (ThinkingLevelSetting::Off, ThinkingLevel::Off),
            (ThinkingLevelSetting::Minimal, ThinkingLevel::Minimal),
            (ThinkingLevelSetting::Low, ThinkingLevel::Low),
            (ThinkingLevelSetting::Medium, ThinkingLevel::Medium),
            (ThinkingLevelSetting::High, ThinkingLevel::High),
            (ThinkingLevelSetting::XHigh, ThinkingLevel::XHigh),
            (ThinkingLevelSetting::Max, ThinkingLevel::Max),
        ] {
            assert_eq!(settings_thinking_level(None, Some(configured)), expected);
            assert_eq!(
                settings_thinking_level(Some(ThinkingLevel::Off), Some(configured)),
                ThinkingLevel::Off
            );
        }
        let settings = SettingsValues {
            thinking_budgets: Some(pi_settings::ThinkingBudgetsSettings {
                minimal: Some(111),
                low: None,
                medium: Some(555),
                high: Some(999),
            }),
            ..SettingsValues::default()
        };
        assert_eq!(
            settings_thinking_budgets(&settings),
            Some(ThinkingBudgets {
                minimal: Some(111),
                low: None,
                medium: Some(555),
                high: Some(999),
            })
        );
        assert_eq!(settings_thinking_budgets(&SettingsValues::default()), None);
        assert_eq!(settings_queue_mode(QueueModeSetting::All), QueueMode::All);
        assert_eq!(
            settings_queue_mode(QueueModeSetting::OneAtATime),
            QueueMode::OneAtATime
        );
    }

    #[test]
    fn current_setting_paths_use_scope_bases_and_expand_tilde() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("agent");
        let project = directory.path().join("project/.pi");
        let absolute = directory.path().join("absolute/skill");
        let resolved = scoped_setting_paths(
            &["skills/global".to_string()],
            &global,
            &[
                "../shared".to_string(),
                absolute.to_string_lossy().into_owned(),
            ],
            &project,
        );
        assert_eq!(resolved[0], global.join("skills/global"));
        assert_eq!(resolved[1], project.join("../shared"));
        assert_eq!(resolved[2], absolute);
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            assert_eq!(expand_tilde_path("~"), home);
            assert_eq!(setting_path("~/skills", &global), home.join("skills"));
        }
    }

    #[test]
    fn apply_settings_retains_global_proxy_scoped_roots_and_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let cwd = directory.path().join("project");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(cwd.join(".pi")).unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            json!({
                "httpProxy": "http://global.example:8080",
                "httpIdleTimeoutMs": 111,
                "skills": ["global-skills"],
                "prompts": ["global-prompts"],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            cwd.join(".pi/settings.json"),
            json!({
                "httpProxy": "http://project.example:8080",
                "httpIdleTimeoutMs": 222,
                "skills": ["project-skills"],
                "prompts": ["project-prompts"],
                "shellPath": 7,
            })
            .to_string(),
        )
        .unwrap();
        let manager = SettingsManager::new(&agent_dir);
        let snapshot = manager.load(&SettingsContext::new(&cwd, true));
        assert!(!snapshot.diagnostics().is_empty());
        let mut config = Config::new(cwd.clone(), agent_dir.clone());
        config.features = crate::Features::none();
        config.model = Some("explicit-model".into());
        let old_path = config.session_path.clone();
        apply_settings(&mut config, &snapshot);
        let mut expected = snapshot.effective().clone();
        expected.http_proxy = snapshot.global().http_proxy.clone();
        assert_eq!(config.runtime_settings, expected);
        assert_eq!(config.runtime_settings.http_idle_timeout_ms, 222);
        assert_eq!(
            config.settings_skill_paths,
            [
                agent_dir.join("global-skills"),
                cwd.join(".pi/project-skills")
            ]
        );
        assert_eq!(
            config.settings_prompt_paths,
            [
                agent_dir.join("global-prompts"),
                cwd.join(".pi/project-prompts")
            ]
        );
        assert_eq!(
            config.settings_diagnostics.len(),
            snapshot.diagnostics().len()
        );
        for (actual, original) in config
            .settings_diagnostics
            .iter()
            .zip(snapshot.diagnostics())
        {
            assert_eq!(actual.kind, pi_runtime::DiagnosticKind::Warning);
            assert_eq!(actual.path, original.path);
            assert_eq!(actual.message, original.message);
        }
        assert_eq!(config.features, crate::Features::none());
        assert_eq!(config.model.as_deref(), Some("explicit-model"));
        assert_eq!(config.session_path, old_path);

        apply_settings(
            &mut config,
            &manager.load(&SettingsContext::new(&cwd, false)),
        );
        assert_eq!(config.runtime_settings.http_idle_timeout_ms, 111);
        assert_eq!(
            config.settings_skill_paths,
            [agent_dir.join("global-skills")]
        );
        assert_eq!(
            config.settings_prompt_paths,
            [agent_dir.join("global-prompts")]
        );
        assert!(config.settings_diagnostics.is_empty());
    }

    #[test]
    fn memory_options_preserve_both_configured_and_active_session_roots() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        let agent = directory.path().join("agent");
        let configured = directory.path().join("configured/startup.jsonl");
        let active = directory.path().join("isolated/child.jsonl");
        let options = memory_options(&cwd, &agent, &configured, Some(&active), true);
        assert_eq!(options.cwd, cwd);
        assert_eq!(options.agent_dir, agent);
        assert!(options.project_trusted);
        assert_eq!(
            options.session_roots,
            [
                agent.join("sessions"),
                configured.parent().unwrap().to_path_buf(),
                active.parent().unwrap().to_path_buf()
            ]
        );
        let options = memory_options(&cwd, &agent, &configured, None, false);
        assert!(!options.project_trusted);
        assert_eq!(
            options.session_roots,
            [
                agent.join("sessions"),
                configured.parent().unwrap().to_path_buf()
            ]
        );
    }

    #[test]
    fn session_options_only_adapt_settings_and_keep_model_inputs_distinct() {
        let settings = SettingsValues {
            default_provider: Some("settings-provider".into()),
            default_model: Some("settings-model".into()),
            compaction: pi_settings::CompactionSettings {
                enabled: false,
                reserve_tokens: 123,
                keep_recent_tokens: 456,
            },
            branch_summary: pi_settings::BranchSummarySettings {
                reserve_tokens: 789,
                skip_prompt: false,
            },
            retry: pi_settings::RetrySettings {
                enabled: false,
                max_retries: 8,
                base_delay_ms: 25,
                provider: pi_settings::ProviderRetrySettings::default(),
            },
            shell_path: Some("~/bin/shell".into()),
            shell_command_prefix: Some("source init.sh".into()),
            ..SettingsValues::default()
        };
        let request =
            initial_model_request(Some("explicit-provider"), Some("explicit-model"), &settings);
        assert_eq!(
            request.requested_provider,
            Some(ProviderId::new("explicit-provider"))
        );
        assert_eq!(request.requested_model.as_deref(), Some("explicit-model"));
        assert_eq!(
            request.settings_provider,
            Some(ProviderId::new("settings-provider"))
        );
        assert_eq!(request.settings_model.as_deref(), Some("settings-model"));
        assert!(request.session_model.is_none());
        let options = session_options(&settings, request.clone());
        assert_eq!(options.initial_model, request);
        assert_eq!(
            options.compaction,
            CompactionSettings {
                enabled: false,
                reserve_tokens: 123,
                keep_recent_tokens: 456,
            }
        );
        assert_eq!(options.branch_summary_reserve_tokens, Some(789));
        assert_eq!(
            options.retry,
            AutoRetrySettings {
                enabled: false,
                max_retries: 8,
                base_delay_ms: 25,
            }
        );
        assert!(options.shell_executor.is_some());
        assert!(options.compaction_policy.is_some());
        assert!(options.additional_active_tools.is_empty());
        assert_eq!(options.runtime_inventory, Default::default());
        assert!(options.context_window.is_none());
        let request = initial_model_request(None, None, &settings);
        assert!(request.requested_provider.is_none());
        assert!(request.requested_model.is_none());
        assert_eq!(request.settings_model, settings.default_model);
    }
}
