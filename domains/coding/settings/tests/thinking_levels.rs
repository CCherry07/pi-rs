use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pi_settings::{
    SettingsContext, SettingsDiagnosticKind, SettingsManager, SettingsScope, ThinkingLevelSetting,
};
use serde_json::{Value, json};

fn write_json(path: &Path, value: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn model_thinking_levels_use_the_same_values_as_the_default() {
    let root = tempfile::tempdir().unwrap();
    let manager = SettingsManager::new(root.path().join("agent"));
    let context = SettingsContext::new(root.path().join("project"), false);
    let cases = [
        ("off", ThinkingLevelSetting::Off),
        ("minimal", ThinkingLevelSetting::Minimal),
        ("low", ThinkingLevelSetting::Low),
        ("medium", ThinkingLevelSetting::Medium),
        ("high", ThinkingLevelSetting::High),
        ("xhigh", ThinkingLevelSetting::XHigh),
        ("max", ThinkingLevelSetting::Max),
    ];
    let levels: BTreeMap<_, _> = cases
        .iter()
        .map(|(name, _)| (format!("provider/model-{name}"), *name))
        .collect();
    let expected: BTreeMap<_, _> = cases
        .iter()
        .map(|(name, level)| (format!("provider/model-{name}"), *level))
        .collect();

    for (name, level) in cases {
        write_json(
            &manager.agent_dir().join("settings.json"),
            json!({"defaultThinkingLevel": name, "modelThinkingLevels": levels}),
        );
        let snapshot = manager.load(&context);
        assert_eq!(snapshot.effective().default_thinking_level, Some(level));
        assert_eq!(snapshot.effective().model_thinking_levels, expected);
        assert!(snapshot.diagnostics().is_empty());
    }
}

#[test]
fn invalid_model_thinking_levels_are_localized_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let manager = SettingsManager::new(root.path().join("agent"));
    let context = SettingsContext::new(root.path().join("project"), false);
    let path = manager.agent_dir().join("settings.json");
    write_json(
        &path,
        json!({
            "defaultThinkingLevel": "low",
            "modelThinkingLevels": {
                "provider/valid": "high",
                "provider/unknown": "extreme",
                "provider/wrong-case": "High",
                "provider/wrong-type": 3,
                "provider/null": null
            }
        }),
    );
    let snapshot = manager.load(&context);
    assert_eq!(
        snapshot.effective().default_thinking_level,
        Some(ThinkingLevelSetting::Low)
    );
    assert_eq!(
        snapshot.effective().model_thinking_levels,
        BTreeMap::from([("provider/valid".to_string(), ThinkingLevelSetting::High)])
    );
    assert_eq!(snapshot.diagnostics().len(), 4);
    for diagnostic in snapshot.diagnostics() {
        assert_eq!(diagnostic.kind, SettingsDiagnosticKind::InvalidValue);
        assert_eq!(diagnostic.scope, SettingsScope::Global);
        assert_eq!(diagnostic.path, path);
        assert!(diagnostic.message.contains("provider/"));
    }

    for value in [json!(false), json!([]), json!(null), json!("high")] {
        write_json(&path, json!({"modelThinkingLevels": value}));
        let snapshot = manager.load(&context);
        assert!(snapshot.effective().model_thinking_levels.is_empty());
        assert_eq!(snapshot.diagnostics().len(), 1);
        assert_eq!(
            snapshot.diagnostics()[0].message,
            "modelThinkingLevels must be an object"
        );
    }
}

#[test]
fn model_thinking_levels_merge_only_trusted_project_overrides() {
    let root = tempfile::tempdir().unwrap();
    let manager = SettingsManager::new(root.path().join("agent"));
    let cwd = root.path().join("project");
    assert!(
        manager
            .load(&SettingsContext::new(&cwd, false))
            .effective()
            .model_thinking_levels
            .is_empty()
    );
    write_json(
        &manager.agent_dir().join("settings.json"),
        json!({"modelThinkingLevels": {"provider/shared": "low", "provider/global": "off"}}),
    );
    write_json(
        &cwd.join(".pi/settings.json"),
        json!({"modelThinkingLevels": {"provider/shared": "high", "provider/project": "max"}}),
    );
    let untrusted = manager.load(&SettingsContext::new(&cwd, false));
    assert_eq!(
        untrusted.effective().model_thinking_levels,
        BTreeMap::from([
            ("provider/shared".to_string(), ThinkingLevelSetting::Low),
            ("provider/global".to_string(), ThinkingLevelSetting::Off),
        ])
    );
    let trusted = manager.load(&SettingsContext::new(&cwd, true));
    assert_eq!(
        trusted.effective().model_thinking_levels,
        BTreeMap::from([
            ("provider/shared".to_string(), ThinkingLevelSetting::High),
            ("provider/global".to_string(), ThinkingLevelSetting::Off),
            ("provider/project".to_string(), ThinkingLevelSetting::Max),
        ])
    );
    assert!(trusted.diagnostics().is_empty());
}
