use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use pi_coding::{Config, Features, Pi};
use pi_core::{AbortHandle, RunId};
use pi_plugin::BeforeAgentStartEvent;
use pi_session::{AgentSession, PiSession};
use serde_json::json;

const CORE_TOOLS: &[&str] = &[
    "read",
    "grep",
    "find",
    "ls",
    "write",
    "edit",
    "hashline_edit",
    "bash",
];
const MEMORY_TOOLS: &[&str] = &[
    "memory",
    "memory_search",
    "session_search",
    "skill_view",
    "skills_list",
    "skill_manage",
];
const SUBAGENT_TOOLS: &[&str] = &[
    "spawn_agent",
    "send_message",
    "followup_task",
    "wait_agent",
    "interrupt_agent",
    "list_agents",
];
const SKILL_NAME: &str = "sdk-feature-skill";

fn feature_values(features: &Features) -> [bool; 6] {
    [
        features.memory,
        features.subagents,
        features.schedule,
        features.skills,
        features.prompt_templates,
        features.session_transfer,
    ]
}

fn write_features(path: &Path, features: Features) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        json!({"features": {
            "memory": features.memory,
            "subagents": features.subagents,
            "schedule": features.schedule,
            "skills": features.skills,
            "promptTemplates": features.prompt_templates,
            "sessionTransfer": features.session_transfer,
        }})
        .to_string(),
    )
    .unwrap();
}

fn write_skill(agent_dir: &Path, name: &str) {
    let directory = agent_dir.join("skills").join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: SDK feature selection fixture\n---\nRun the fixture checks.\n"),
    )
    .unwrap();
}

fn fixture_config(root: &Path) -> Config {
    let cwd = root.join("project");
    let agent_dir = root.join("agent");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(agent_dir.join("prompts")).unwrap();
    write_skill(&agent_dir, SKILL_NAME);
    fs::write(
        agent_dir.join("prompts/sdk-feature-prompt.md"),
        "---\ndescription: SDK prompt template fixture\n---\nCheck $ARGUMENTS.\n",
    )
    .unwrap();
    // Keep the memory provider enabled, but do not start autonomous model work.
    fs::write(
        agent_dir.join("memory.json"),
        r#"{"version":1,"provider":"hermes","providers":{"hermes":{"reviewEnabled":false,"curator":{"enabled":false}}}}"#,
    )
    .unwrap();
    fs::write(
        agent_dir.join("models.json"),
        r#"{"providers":{"features-fixture":{"baseUrl":"https://features.invalid/v1","api":"openai-completions","apiKey":"fixture-key","models":[{"id":"fixture-model"}]}}}"#,
    )
    .unwrap();
    let mut config = Config::new(cwd, agent_dir);
    config.provider = "features-fixture".to_string();
    config.model = Some("fixture-model".to_string());
    config.trust_override = Some(true);
    config.discover_extensions = false;
    config.load_mcp_config = false;
    config
}

async fn create_session(config: Config) -> (Pi, PiSession) {
    let cwd = config.cwd.clone();
    let path = config.session_path.clone();
    let pi = Pi::builder(config).build().unwrap();
    let session = pi.sessions().create_session(cwd, path).await.unwrap();
    (pi, session)
}

fn assert_feature_registrations(session: &AgentSession, features: &Features) {
    let runtime = session.runtime();
    let tools = runtime
        .tool_specs()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<BTreeSet<_>>();
    let active = runtime.active_tools().into_iter().collect::<BTreeSet<_>>();
    let commands = runtime
        .command_specs()
        .into_iter()
        .map(|command| command.name)
        .collect::<BTreeSet<_>>();
    for (names, enabled) in [
        (CORE_TOOLS, true),
        (MEMORY_TOOLS, features.memory),
        (SUBAGENT_TOOLS, features.subagents),
        (&["schedule"][..], features.schedule),
    ] {
        for name in names {
            assert_eq!(tools.contains(*name), enabled, "registered tool {name}");
            assert_eq!(active.contains(*name), enabled, "active tool {name}");
        }
    }
    for (names, enabled) in [
        (
            &[
                "memory-consolidate",
                "memory-index-sessions",
                "memory-insights",
                "memory-interview",
                "learn-memory-tool",
                "memory-preview-context",
                "refine",
                "memory-skills",
                "memory-sync-markdown",
            ][..],
            features.memory,
        ),
        (
            &["subagents:interrupt", "subagents:followup"][..],
            features.subagents,
        ),
        (&["schedule"][..], features.schedule),
        (&["skill:sdk-feature-skill"][..], features.skills),
        (&["sdk-feature-prompt"][..], features.prompt_templates),
        (
            &["export", "import", "share"][..],
            features.session_transfer,
        ),
    ] {
        for name in names {
            assert_eq!(commands.contains(*name), enabled, "command {name}");
        }
    }
    let feature_plugins = [
        ("prompt-templates", features.prompt_templates),
        ("memory-hermes", features.memory),
        ("subagents", features.subagents),
        ("skills", features.skills),
        ("session-transfer", features.session_transfer),
        ("schedule", features.schedule),
    ];
    let plugins = runtime.plugin_order();
    let actual = plugins
        .iter()
        .map(|id| id.as_str())
        .filter(|name| feature_plugins.iter().any(|(feature, _)| feature == name))
        .collect::<Vec<_>>();
    let expected = feature_plugins
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "feature agent plugin registration order");
}

async fn projected_prompt(session: &AgentSession) -> String {
    let runtime = session.runtime();
    let state = runtime.agent().state();
    let (_, signal) = AbortHandle::new();
    // Exercise the real generation's bound hook driver without a provider request.
    let patch = runtime
        .agent()
        .runtime()
        .plugins()
        .before_agent_start(
            &RunId::new("features-fixture"),
            runtime.cwd(),
            &signal,
            BeforeAgentStartEvent {
                system_prompt: state.system_prompt,
                input_messages: Vec::new(),
                active_tools: state.active_tools,
                provider_id: state.provider_id,
                model_id: state.model_id,
            },
        )
        .await
        .unwrap();
    assert!(runtime.plugin_diagnostics().is_empty());
    patch.system_prompt.unwrap()
}

#[test]
fn features_default_and_all_enable_every_feature_and_none_disables_every_feature() {
    assert_eq!(feature_values(&Features::default()), [true; 6]);
    assert_eq!(feature_values(&Features::all()), [true; 6]);
    assert_eq!(feature_values(&Features::none()), [false; 6]);
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        feature_values(&fixture_config(root.path()).features),
        [true; 6]
    );
}

#[tokio::test]
async fn default_features_retain_existing_product_registrations_and_skill_prompt() {
    let root = tempfile::tempdir().unwrap();
    let (pi, session) = create_session(fixture_config(root.path())).await;
    assert_feature_registrations(&session.current(), &Features::all());
    assert!(
        projected_prompt(&session.current())
            .await
            .contains(SKILL_NAME)
    );
    assert!(!session.current().log().is_materialized());
    session.reload().await.unwrap();
    assert_feature_registrations(&session.current(), &Features::all());
    assert!(!session.current().log().is_materialized());
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn no_features_remove_feature_plugins_tools_commands_and_skill_prompt() {
    let root = tempfile::tempdir().unwrap();
    let mut config = fixture_config(root.path());
    config.features = Features::none();
    let (pi, session) = create_session(config).await;
    assert_feature_registrations(&session.current(), &Features::none());
    assert!(session.current().runtime().command_specs().is_empty());
    assert!(
        !projected_prompt(&session.current())
            .await
            .contains(SKILL_NAME)
    );
    assert!(!pi.agent_dir().join("pi-hermes-memory").exists());
    assert!(!session.current().log().is_materialized());
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn each_feature_can_be_enabled_without_enabling_the_others() {
    let selections = [
        Features {
            memory: true,
            ..Features::none()
        },
        Features {
            subagents: true,
            ..Features::none()
        },
        Features {
            schedule: true,
            ..Features::none()
        },
        Features {
            skills: true,
            ..Features::none()
        },
        Features {
            prompt_templates: true,
            ..Features::none()
        },
        Features {
            session_transfer: true,
            ..Features::none()
        },
    ];
    for features in selections {
        let root = tempfile::tempdir().unwrap();
        let mut config = fixture_config(root.path());
        config.features = features;
        let (pi, session) = create_session(config).await;
        assert_feature_registrations(&session.current(), &features);
        assert_eq!(
            projected_prompt(&session.current())
                .await
                .contains(SKILL_NAME),
            features.skills,
            "skills prompt with selection {features:?}",
        );
        pi.sessions().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn disabled_memory_skips_invalid_configuration_on_create_and_reload() {
    for invalid in ["not json", r#"{"version":1,"provider":"missing-provider"}"#] {
        let root = tempfile::tempdir().unwrap();
        let mut config = fixture_config(root.path());
        fs::write(config.agent_dir.join("memory.json"), invalid).unwrap();
        config.features.memory = false;
        let enabled_config = Config {
            features: Features::all(),
            ..config.clone()
        };
        let (pi, session) = create_session(config.clone()).await;
        assert_feature_registrations(&session.current(), &config.features);
        session.reload().await.unwrap();
        assert_feature_registrations(&session.current(), &config.features);
        assert!(!pi.agent_dir().join("pi-hermes-memory").exists());
        pi.sessions().shutdown().await.unwrap();

        let enabled = Pi::builder(enabled_config).build().unwrap();
        let error = match enabled
            .sessions()
            .create_session(&config.cwd, &config.session_path)
            .await
        {
            Ok(_) => panic!("enabled memory accepted invalid configuration {invalid}"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("memory"), "{error}");
        enabled.sessions().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn memory_profile_reload_keeps_tools_skills_and_frozen_context_together() {
    let root = tempfile::tempdir().unwrap();
    let mut config = fixture_config(root.path());
    config.features = Features {
        memory: true,
        skills: true,
        ..Features::none()
    };
    let enabled_features = config.features;
    let profile_path = config.agent_dir.join("memory.json");
    let profile = fs::read(&profile_path).unwrap();
    let memory_dir = config.agent_dir.join("pi-hermes-memory");
    write_skill(&memory_dir, "memory-managed-skill");
    let memory_path = memory_dir.join("MEMORY.md");
    fs::write(&memory_path, "Prefers concise responses.").unwrap();
    let (pi, session) = create_session(config).await;
    assert_feature_registrations(&session.current(), &enabled_features);
    let prompt = projected_prompt(&session.current()).await;
    assert!(prompt.contains("Prefers concise responses."));
    assert!(prompt.contains("memory-managed-skill"));

    let original = session.current();
    fs::write(&memory_path, "Prefers source-backed responses.").unwrap();
    for invalid in [
        "not json",
        r#"{"provider":"local"}"#,
        r#"{"providers":{"hermes":{"sessionRoots":[""]}}}"#,
    ] {
        fs::write(&profile_path, invalid).unwrap();
        assert!(session.reload().await.is_err());
        assert!(Arc::ptr_eq(&original, &session.current()));
        assert!(!original.is_closed());
        assert_feature_registrations(&session.current(), &enabled_features);
        assert_eq!(projected_prompt(&session.current()).await, prompt);
    }

    fs::write(&profile_path, r#"{"enabled":false}"#).unwrap();
    session.reload().await.unwrap();
    assert!(original.is_closed());
    assert_feature_registrations(
        &session.current(),
        &Features {
            memory: false,
            ..enabled_features
        },
    );
    let disabled_prompt = projected_prompt(&session.current()).await;
    assert!(!disabled_prompt.contains("Prefers concise responses."));
    assert!(!disabled_prompt.contains("Prefers source-backed responses."));
    assert!(!disabled_prompt.contains("memory-managed-skill"));
    assert_eq!(
        fs::read_to_string(&memory_path).unwrap(),
        "Prefers source-backed responses."
    );

    fs::write(&profile_path, profile).unwrap();
    session.reload().await.unwrap();
    assert_feature_registrations(&session.current(), &enabled_features);
    let refreshed = projected_prompt(&session.current()).await;
    assert!(refreshed.contains("Prefers source-backed responses."));
    assert!(refreshed.contains("memory-managed-skill"));
    assert!(!refreshed.contains("Prefers concise responses."));
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_preserves_feature_selection_while_refreshing_enabled_resources() {
    for skills in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = fixture_config(root.path());
        config.features = Features {
            skills,
            ..Features::none()
        };
        let features = config.features;
        let (pi, session) = create_session(config).await;
        let original = session.current();
        write_skill(pi.agent_dir(), "sdk-feature-added");
        fs::write(
            pi.agent_dir().join("prompts/sdk-feature-added.md"),
            "This template must remain disabled.",
        )
        .unwrap();
        fs::write(pi.agent_dir().join("memory.json"), "not json").unwrap();
        // Settings cannot widen the SDK host's allowed features.
        write_features(&pi.agent_dir().join("settings.json"), Features::all());

        session.reload().await.unwrap();

        let reloaded = session.current();
        assert!(!Arc::ptr_eq(&original, &reloaded));
        assert!(original.is_closed());
        assert_feature_registrations(&reloaded, &features);
        let commands = reloaded.runtime().command_specs();
        assert_eq!(
            commands
                .iter()
                .any(|spec| spec.name == "skill:sdk-feature-added"),
            skills,
        );
        assert!(!commands.iter().any(|spec| spec.name == "sdk-feature-added"));
        assert_eq!(
            projected_prompt(&reloaded)
                .await
                .contains("sdk-feature-added"),
            skills
        );
        assert!(!reloaded.log().is_materialized());
        assert!(!pi.agent_dir().join("pi-hermes-memory").exists());
        pi.sessions().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn settings_features_apply_project_trust_and_the_host_ceiling() {
    for trusted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = fixture_config(root.path());
        config.trust_override = Some(trusted);
        config.features.memory = false;
        config.features.session_transfer = false;
        fs::write(config.agent_dir.join("memory.json"), "not json").unwrap();
        write_features(
            &config.agent_dir.join("settings.json"),
            Features {
                memory: true,
                ..Features::none()
            },
        );
        fs::create_dir_all(config.cwd.join(".pi")).unwrap();
        fs::write(
            config.cwd.join(".pi/settings.json"),
            json!({"features": {
                "subagents": true, "skills": true, "sessionTransfer": true
            }})
            .to_string(),
        )
        .unwrap();

        let (pi, session) = create_session(config).await;
        let expected = Features {
            subagents: trusted,
            skills: trusted,
            ..Features::none()
        };
        assert_feature_registrations(&session.current(), &expected);
        assert_eq!(
            projected_prompt(&session.current())
                .await
                .contains(SKILL_NAME),
            trusted
        );
        session.reload().await.unwrap();
        assert_feature_registrations(&session.current(), &expected);
        assert!(!pi.agent_dir().join("pi-hermes-memory").exists());
        pi.sessions().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn settings_features_can_be_reenabled_without_mutating_other_sessions_or_data() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture_config(root.path());
    let settings_path = config.agent_dir.join("settings.json");
    let memory_path = config.agent_dir.join("memory.json");
    let memory_config = fs::read(&memory_path).unwrap();
    let cwd = config.cwd.clone();
    write_features(&settings_path, Features::none());
    // Disabled memory must not even parse its provider configuration.
    fs::write(&memory_path, "not json").unwrap();
    let (pi, session) = create_session(config).await;
    let other = pi
        .sessions()
        .create_session(&cwd, pi.agent_dir().join("sessions/other.jsonl"))
        .await
        .unwrap();
    assert_feature_registrations(&session.current(), &Features::none());
    assert!(!pi.agent_dir().join("pi-hermes-memory").exists());
    fs::write(&memory_path, &memory_config).unwrap();

    for enabled in [true, false, true] {
        let previous = session.current();
        let previous_plugins = previous.runtime().plugin_order();
        let expected = if enabled {
            Features::all()
        } else {
            Features::none()
        };
        if enabled {
            // Removing an override restores defaults, including after a previous reload.
            fs::write(&settings_path, "{}").unwrap();
        } else {
            write_features(&settings_path, expected);
        }
        assert_eq!(session.current().runtime().plugin_order(), previous_plugins);
        session.reload().await.unwrap();
        assert!(!Arc::ptr_eq(&previous, &session.current()));
        assert!(previous.is_closed());
        assert_feature_registrations(&session.current(), &expected);
        assert_feature_registrations(&other.current(), &Features::none());
        assert_eq!(
            projected_prompt(&session.current())
                .await
                .contains(SKILL_NAME),
            enabled
        );
        assert!(!session.current().log().is_materialized());
        assert_eq!(fs::read(&memory_path).unwrap(), memory_config);
        assert!(
            pi.agent_dir()
                .join("skills")
                .join(SKILL_NAME)
                .join("SKILL.md")
                .exists()
        );
    }
    // New sessions use the current settings too, rather than the first generation's selection.
    let fresh = pi
        .sessions()
        .create_session(&cwd, pi.agent_dir().join("sessions/fresh.jsonl"))
        .await
        .unwrap();
    assert_feature_registrations(&fresh.current(), &Features::all());
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_feature_activation_preserves_the_previous_generation() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture_config(root.path());
    let path = config.agent_dir.join("settings.json");
    let memory_path = config.agent_dir.join("memory.json");
    let memory_config = fs::read(&memory_path).unwrap();
    let original_features = Features {
        memory: false,
        ..Features::all()
    };
    write_features(&path, original_features);
    fs::write(&memory_path, "not json").unwrap();
    let (pi, session) = create_session(config).await;
    let original = session.current();
    let requested = Features {
        memory: true,
        ..Features::none()
    };
    write_features(&path, requested);

    let error = session.reload().await.unwrap_err();
    assert!(error.to_string().contains("memory"), "{error}");
    assert!(Arc::ptr_eq(&original, &session.current()));
    assert!(!original.is_closed());
    assert_feature_registrations(&session.current(), &original_features);
    assert!(!pi.agent_dir().join("pi-hermes-memory").exists());

    fs::write(&memory_path, memory_config).unwrap();
    session.reload().await.unwrap();
    assert_feature_registrations(&session.current(), &requested);
    assert!(original.is_closed());
    pi.sessions().shutdown().await.unwrap();
}
