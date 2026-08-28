use std::fs;
use std::path::Path;

use pi_settings::{
    DefaultProjectTrust, PackageFilter, PackageSource, QueueModeSetting, SettingsContext,
    SettingsDiagnosticKind, SettingsError, SettingsManager, SettingsScope, TransportSetting,
};
use serde_json::{Value, json};

fn write_json(path: &Path, value: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut encoded = serde_json::to_string_pretty(&value).unwrap();
    encoded.push('\n');
    fs::write(path, encoded).unwrap();
}

#[test]
fn current_settings_deep_merge_nested_objects_and_replace_arrays() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "defaultProvider": "global-provider",
            "defaultProjectTrust": "always",
            "compaction": {"enabled": false, "reserveTokens": 10},
            "extensions": ["global.ts"],
            "futureUi": {"left": true, "shared": {"global": 1}}
        }),
    );
    write_json(
        &cwd.join(".pi/settings.json"),
        json!({
            "defaultModel": "project-model",
            "defaultProjectTrust": "never",
            "compaction": {"reserveTokens": 20},
            "extensions": ["project.ts"],
            "futureUi": {"right": true, "shared": {"project": 2}}
        }),
    );

    let snapshot = SettingsManager::new(&agent_dir).load(&SettingsContext::new(&cwd, true));

    assert_eq!(
        snapshot.effective().default_provider.as_deref(),
        Some("global-provider")
    );
    assert_eq!(
        snapshot.effective().default_model.as_deref(),
        Some("project-model")
    );
    assert!(!snapshot.effective().compaction.enabled);
    assert_eq!(snapshot.effective().compaction.reserve_tokens, 20);
    assert_eq!(snapshot.effective().extensions, ["project.ts"]);
    assert_eq!(
        snapshot.default_project_trust(),
        DefaultProjectTrust::Always
    );
    assert_eq!(snapshot.raw_effective()["futureUi"]["left"], true);
    assert_eq!(snapshot.raw_effective()["futureUi"]["right"], true);
    assert_eq!(
        snapshot.raw_effective()["futureUi"]["shared"],
        json!({"global": 1, "project": 2})
    );
}

#[test]
fn untrusted_project_settings_are_never_read_and_reads_do_not_create_pi_directory() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let manager = SettingsManager::new(&agent_dir);

    let snapshot = manager.load(&SettingsContext::new(&cwd, false));

    assert!(!cwd.join(".pi").exists());
    assert!(!agent_dir.exists());
    assert!(!snapshot.project_trusted());
    assert!(snapshot.raw_project().is_empty());

    write_json(
        &cwd.join(".pi/settings.json"),
        json!({"defaultProvider": "project-provider"}),
    );
    let untrusted = manager.load(&SettingsContext::new(&cwd, false));
    let trusted = manager.load(&SettingsContext::new(&cwd, true));
    assert_eq!(untrusted.effective().default_provider, None);
    assert_eq!(
        trusted.effective().default_provider.as_deref(),
        Some("project-provider")
    );
}

#[test]
fn malformed_reload_retains_the_last_valid_document_for_that_scope() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let path = agent_dir.join("settings.json");
    write_json(&path, json!({"defaultProvider": "first"}));
    let manager = SettingsManager::new(&agent_dir);
    let context = SettingsContext::new(&cwd, false);
    assert_eq!(
        manager
            .load(&context)
            .effective()
            .default_provider
            .as_deref(),
        Some("first")
    );

    fs::write(&path, "{ definitely-not-json").unwrap();
    let retained = manager.load(&context);

    assert_eq!(
        retained.effective().default_provider.as_deref(),
        Some("first")
    );
    assert!(
        retained
            .diagnostics()
            .iter()
            .any(|error| error.kind == SettingsDiagnosticKind::Parse)
    );

    write_json(&path, json!({"defaultProvider": "second"}));
    assert_eq!(
        manager
            .load(&context)
            .effective()
            .default_provider
            .as_deref(),
        Some("second")
    );
}

#[test]
fn historical_keys_are_preserved_but_have_no_effect() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "queueMode": "all",
            "websockets": true,
            "skills": {"enableSkillCommands": false, "customDirectories": ["old"]},
            "retry": {"maxDelayMs": 12}
        }),
    );

    let snapshot = SettingsManager::new(&agent_dir).load(&SettingsContext::new(&cwd, false));

    assert_eq!(
        snapshot.effective().steering_mode,
        QueueModeSetting::OneAtATime
    );
    assert_eq!(snapshot.effective().transport, TransportSetting::Auto);
    assert!(snapshot.effective().skills.is_empty());
    assert_eq!(
        snapshot.effective().retry.provider.max_retry_delay_ms,
        60_000
    );
    assert_eq!(snapshot.raw_global()["queueMode"], "all");
    assert_eq!(snapshot.raw_global()["websockets"], true);
}

#[test]
fn invalid_current_field_is_localized_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "steeringMode": "several",
            "compaction": {"enabled": "yes"},
            "defaultModel": "still-valid"
        }),
    );

    let snapshot = SettingsManager::new(&agent_dir).load(&SettingsContext::new(&cwd, false));

    assert_eq!(
        snapshot.effective().default_model.as_deref(),
        Some("still-valid")
    );
    assert_eq!(
        snapshot.effective().steering_mode,
        QueueModeSetting::OneAtATime
    );
    assert!(snapshot.effective().compaction.enabled);
    assert_eq!(
        snapshot
            .diagnostics()
            .iter()
            .filter(|error| error.kind == SettingsDiagnosticKind::InvalidValue)
            .count(),
        2
    );
}

#[test]
fn current_timeout_values_accept_disabled_strings_and_floor_finite_numbers() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "httpIdleTimeoutMs": 1234.9,
            "websocketConnectTimeoutMs": "987.6"
        }),
    );

    let manager = SettingsManager::new(&agent_dir);
    let context = SettingsContext::new(&cwd, false);
    let snapshot = manager.load(&context);
    assert_eq!(snapshot.effective().http_idle_timeout_ms, 1_234);
    assert_eq!(snapshot.effective().websocket_connect_timeout_ms, Some(987));

    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "httpIdleTimeoutMs": "disabled",
            "websocketConnectTimeoutMs": "DISABLED"
        }),
    );
    let snapshot = manager.load(&context);
    assert_eq!(snapshot.effective().http_idle_timeout_ms, 0);
    assert_eq!(snapshot.effective().websocket_connect_timeout_ms, Some(0));
}

#[test]
fn package_write_reloads_latest_file_and_preserves_unrelated_and_unknown_fields() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let path = agent_dir.join("settings.json");
    write_json(
        &path,
        json!({"packages": ["old"], "compaction": {"enabled": true}}),
    );
    let manager = SettingsManager::new(&agent_dir);
    let context = SettingsContext::new(&cwd, false);
    manager.load(&context);

    write_json(
        &path,
        json!({
            "packages": ["external"],
            "compaction": {"enabled": true, "reserveTokens": 123},
            "theme": "external-theme",
            "future": {"kept": true}
        }),
    );
    manager
        .replace_packages(
            &context,
            SettingsScope::Global,
            &[PackageSource::String("new".to_string())],
        )
        .unwrap();

    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["packages"], json!(["new"]));
    assert_eq!(saved["compaction"]["enabled"], true);
    assert_eq!(saved["compaction"]["reserveTokens"], 123);
    assert_eq!(saved["theme"], "external-theme");
    assert_eq!(saved["future"]["kept"], true);
    assert!(fs::read_to_string(path).unwrap().ends_with('\n'));
}

#[test]
fn project_write_requires_trust_and_only_then_creates_pi_directory() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let manager = SettingsManager::new(&agent_dir);
    let untrusted = SettingsContext::new(&cwd, false);

    assert!(matches!(
        manager.replace_packages(&untrusted, SettingsScope::Project, &[]),
        Err(SettingsError::ProjectNotTrusted)
    ));
    assert!(!cwd.join(".pi").exists());

    let trusted = SettingsContext::new(&cwd, true);
    manager
        .replace_packages(
            &trusted,
            SettingsScope::Project,
            &[PackageSource::String("local".to_string())],
        )
        .unwrap();
    assert!(cwd.join(".pi/settings.json").exists());
}

#[test]
fn malformed_disk_document_is_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let path = agent_dir.join("settings.json");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(&path, "{ broken").unwrap();
    let manager = SettingsManager::new(&agent_dir);
    let context = SettingsContext::new(&cwd, false);

    assert!(matches!(
        manager.replace_packages(&context, SettingsScope::Global, &[]),
        Err(SettingsError::InvalidDocument { .. })
    ));
    assert_eq!(fs::read_to_string(path).unwrap(), "{ broken");
}

#[test]
fn package_filter_round_trips_current_resource_fields() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let manager = SettingsManager::new(&agent_dir);
    let context = SettingsContext::new(&cwd, false);
    let package = PackageSource::Filter(PackageFilter {
        source: "example".to_string(),
        autoload: Some(false),
        extensions: Some(vec!["extensions/*.ts".to_string()]),
        skills: Some(vec!["skills/*".to_string()]),
        prompts: Some(vec!["prompts/*".to_string()]),
        themes: Some(vec!["themes/*".to_string()]),
        extra: [("futureFilter".to_string(), json!(true))]
            .into_iter()
            .collect(),
    });

    let snapshot = manager
        .replace_packages(
            &context,
            SettingsScope::Global,
            std::slice::from_ref(&package),
        )
        .unwrap();

    assert_eq!(snapshot.global().packages, [package]);
    assert_eq!(snapshot.raw_global()["packages"][0]["futureFilter"], true);
}

#[test]
fn errors_can_be_drained_without_repeating_old_entries() {
    let root = tempfile::tempdir().unwrap();
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(agent_dir.join("settings.json"), "[").unwrap();
    let manager = SettingsManager::new(&agent_dir);

    manager.load(&SettingsContext::new(&cwd, false));

    assert_eq!(manager.drain_errors().len(), 1);
    assert!(manager.drain_errors().is_empty());
}
