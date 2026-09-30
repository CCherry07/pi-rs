use super::*;
use pi_plugin::{Plugin, PrepareContext};

fn context(root: &Path) -> PrepareContext {
    PrepareContext::new(
        pi_core::WorkspaceSpec::from_cwd(root).snapshot(),
        root.join("agent"),
        false,
    )
}

fn write_profile(root: &Path, raw: &str) {
    std::fs::create_dir_all(root.join("agent")).unwrap();
    std::fs::write(root.join("agent/memory.json"), raw).unwrap();
}

#[test]
fn profile_selection_is_read_only_and_disabled_preparation_has_no_storage_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    assert!(HermesMemoryPlugin::configured_enabled(&agent).unwrap());
    assert!(!agent.exists());

    for raw in [
        r#"{"enabled":false}"#,
        r#"{"enabled":false,"provider":"local"}"#,
    ] {
        write_profile(root.path(), raw);
        assert!(!HermesMemoryPlugin::configured_enabled(&agent).unwrap());
        assert!(
            HermesMemoryPlugin::prepare(&context(root.path()), Default::default())
                .unwrap()
                .is_none()
        );
        assert!(!agent.join("pi-hermes-memory").exists());
    }

    for provider in ["local", "remote"] {
        write_profile(root.path(), &json!({"provider": provider}).to_string());
        // Desktop's selection probe continues to tolerate unregistered providers.
        assert!(!HermesMemoryPlugin::configured_enabled(&agent).unwrap());
        let error = HermesMemoryPlugin::prepare(&context(root.path()), Default::default())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains(&format!("unknown provider {provider}"))
        );
        assert!(!agent.join("pi-hermes-memory").exists());
    }
}

#[test]
fn factory_preserves_profile_defaults_precedence_and_session_roots() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    let prepare = || {
        HermesMemoryPlugin::prepare(
            &context(root.path()),
            HermesMemoryOptions {
                session_roots: Some(vec![root.path().join("isolated")]),
            },
        )
        .unwrap()
        .unwrap()
    };

    let plugin = prepare();
    assert_eq!(
        plugin.config.memory_char_limit,
        crate::config::DEFAULT_MEMORY_CHAR_LIMIT
    );
    assert_eq!(
        plugin.store.database().session_roots(),
        &[agent.join("sessions"), root.path().join("isolated")]
    );
    assert!(!agent.join("memory.json").exists());
    assert!(plugin.live_index.lock().unwrap().is_none());
    assert!(plugin.backfill.lock().unwrap().is_none());
    assert!(plugin.curator_worker.lock().unwrap().is_none());
    drop(plugin);

    std::fs::write(
        agent.join("settings.json"),
        r#"{"memory":{"enabled":false}}"#,
    )
    .unwrap();
    write_profile(
        root.path(),
        r#"{
        "providers":{"local":["ignored"],"hermes":{"memoryCharLimit":99}},
        "recall":{"maxRecords":12,"tokenBudget":2400,"timeoutMs":0}
    }"#,
    );
    assert_eq!(prepare().config.memory_char_limit, 99);
    std::fs::write(
        agent.join("hermes-memory-config.json"),
        r#"{"memoryCharLimit":123}"#,
    )
    .unwrap();
    assert_eq!(prepare().config.memory_char_limit, 123);
}

#[test]
fn user_session_roots_are_plugin_owned_and_explicit_options_override_the_profile() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    let external = root.path().join("external-sessions");
    write_profile(
        root.path(),
        &json!({"providers":{"hermes":{"sessionRoots":[
            "archive", external, "sessions", "archive"
        ]}}})
        .to_string(),
    );
    let prepare = |options| {
        HermesMemoryPlugin::prepare(&context(root.path()), options)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        prepare(Default::default()).store.database().session_roots(),
        &[agent.join("sessions"), agent.join("archive"), external]
    );

    std::fs::write(
        agent.join("hermes-memory-config.json"),
        r#"{"sessionRoots":["dedicated"]}"#,
    )
    .unwrap();
    assert_eq!(
        prepare(Default::default()).store.database().session_roots(),
        &[agent.join("sessions"), agent.join("dedicated")]
    );
    let options = serde_json::from_value(json!({"sessionRoots":["explicit"]})).unwrap();
    assert_eq!(
        prepare(options).store.database().session_roots(),
        &[agent.join("sessions"), agent.join("explicit")]
    );
    let options = serde_json::from_value(json!({"sessionRoots":[]})).unwrap();
    assert_eq!(
        prepare(options).store.database().session_roots(),
        &[agent.join("sessions")]
    );

    for session_roots in [json!("archive"), json!(["archive", 42]), json!([""])] {
        let invalid = tempfile::tempdir().unwrap();
        write_profile(
            invalid.path(),
            &json!({"providers":{"hermes":{"sessionRoots":session_roots}}}).to_string(),
        );
        let error = HermesMemoryPlugin::prepare(&context(invalid.path()), Default::default())
            .err()
            .unwrap();
        assert!(error.to_string().contains("sessionRoots"), "{error}");
        assert!(!invalid.path().join("agent/pi-hermes-memory").exists());
    }
}

#[test]
fn invalid_profile_retains_path_diagnostics_and_never_initializes_storage() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    for raw in [
        "not json".to_string(),
        r#"{"version":2}"#.to_string(),
        r#"{"unexpected":true}"#.to_string(),
        r#"{"provider":" "}"#.to_string(),
        r#"{"enabled":false,"recall":{"maxRecords":0}}"#.to_string(),
        r#"{"recall":{"tokenBudget":0}}"#.to_string(),
        r#"{"recall":{"timeoutMs":-1}}"#.to_string(),
        r#"{"recall":{"unexpected":1}}"#.to_string(),
        " ".repeat(256 * 1024 + 1),
    ] {
        write_profile(root.path(), &raw);
        let error = HermesMemoryPlugin::prepare(&context(root.path()), Default::default())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains(agent.join("memory.json").to_str().unwrap()),
            "{error}"
        );
        assert!(HermesMemoryPlugin::configured_enabled(&agent).is_err());
        assert!(!agent.join("pi-hermes-memory").exists());
    }
}

#[test]
fn factory_uses_workspace_cwd_and_explicit_project_trust() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let context = |trusted| {
        PrepareContext::new(
            pi_core::WorkspaceSpec::from_cwd(&project).snapshot(),
            root.path().join("agent"),
            trusted,
        )
    };
    let create = || crate::skills::SkillCreate {
        scope: crate::skills::SkillScope::Project,
        name: "prepared-skill".into(),
        description: "Prepared skill".into(),
        body: "Use the resolved workspace.".into(),
    };
    let untrusted = HermesMemoryPlugin::prepare(&context(false), Default::default())
        .unwrap()
        .unwrap();
    assert!(untrusted.store.create_skill(create()).is_err());
    assert!(!project.join(".hermes").exists());

    let trusted = HermesMemoryPlugin::prepare(&context(true), Default::default())
        .unwrap()
        .unwrap();
    let skill = trusted.store.create_skill(create()).unwrap();
    assert_eq!(
        skill.path,
        std::fs::canonicalize(project)
            .unwrap()
            .join(".hermes/skills/prepared-skill/SKILL.md")
    );
}

#[tokio::test]
async fn ordinary_runtime_factory_reloads_policy_and_retains_the_previous_generation_on_error() {
    let root = tempfile::tempdir().unwrap();
    write_profile(
        root.path(),
        r#"{"providers":{"hermes":{"memoryCharLimit":80}}}"#,
    );
    let runtime = pi_runtime::PiRuntime::builder()
        .workspace(pi_core::WorkspaceSpec::from_cwd(root.path()).snapshot())
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .prepare_plugin::<HermesMemoryPlugin>(root.path().join("agent"), false, Default::default())
        .build()
        .unwrap();
    let execute = || {
        let tool = runtime
            .agent()
            .runtime()
            .registries()
            .tool("memory")
            .unwrap();
        let cwd = root.path().to_path_buf();
        async move {
            tool.execute(
                ToolContext::standalone(cwd, AbortHandle::new().1),
                ToolCallId::new("prepare-memory"),
                json!({"action":"add","content":"x".repeat(100)}),
                ToolUpdateSink::channel().0,
            )
            .await
            .unwrap()
        }
    };
    assert!(execute().await.is_error);

    write_profile(
        root.path(),
        r#"{"providers":{"hermes":{"memoryCharLimit":200}}}"#,
    );
    runtime.reload().await.unwrap();
    assert!(!execute().await.is_error);
    let generation = runtime.generation();
    let driver = runtime.plugin_driver();
    for invalid in ["not json", r#"{"provider":"local"}"#] {
        write_profile(root.path(), invalid);
        assert!(runtime.reload().await.is_err());
        assert_eq!(runtime.generation(), generation);
        assert!(Arc::ptr_eq(&driver, &runtime.plugin_driver()));
        assert!(!execute().await.is_error);
    }
}
