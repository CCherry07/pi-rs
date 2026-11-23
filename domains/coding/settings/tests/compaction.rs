use std::fs;

use pi_settings::{CompactionSettings, SessionPreference, SettingsContext, SettingsManager};
use serde_json::{Value, json};

fn load(compaction: Value) -> pi_settings::SettingsSnapshot {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("settings.json"),
        json!({"compaction": compaction}).to_string(),
    )
    .unwrap();
    SettingsManager::new(root.path()).load(&SettingsContext::new(root.path(), false))
}

const MODEL: Option<(&str, &str)> = Some(("provider", "family/model"));
const KEY: &str = "provider/family/model";

fn resolved(
    settings: &pi_settings::SettingsSnapshot,
    model: Option<(&str, &str)>,
) -> CompactionSettings {
    settings.effective().compaction_for_model(model).unwrap()
}

#[test]
fn defaults_and_each_field_fall_back_independently() {
    assert_eq!(
        resolved(&load(json!({})), MODEL),
        CompactionSettings::default()
    );
    assert_eq!(
        resolved(&load(json!({})), None),
        CompactionSettings::default()
    );
    let settings = load(json!({
        "reserveTokens": 8192,
        "keepRecentTokens": 10000,
        "modelOverrides": {KEY: {"reserveTokens": 400000}}
    }));
    assert_eq!(
        resolved(&settings, MODEL),
        CompactionSettings {
            enabled: true,
            reserve_tokens: 400000,
            keep_recent_tokens: 10000,
        }
    );
    assert_eq!(
        resolved(&settings, None),
        CompactionSettings {
            enabled: true,
            reserve_tokens: 8192,
            keep_recent_tokens: 10000,
        }
    );
    assert_eq!(
        settings
            .effective()
            .compaction_tokens
            .reserve_tokens(0, MODEL)
            .unwrap(),
        400000
    );
    assert_eq!(
        settings
            .effective()
            .compaction_tokens
            .keep_recent_tokens(0, MODEL)
            .unwrap(),
        10000
    );
    let settings = load(json!({"modelOverrides": {KEY: {"keepRecentTokens": 1024}}}));
    assert_eq!(
        resolved(&settings, MODEL),
        CompactionSettings {
            keep_recent_tokens: 1024,
            ..CompactionSettings::default()
        }
    );
}

#[test]
fn matches_only_exact_provider_and_model_including_slashes() {
    let settings = load(json!({"modelOverrides": {
        KEY: {"reserveTokens": 400000},
        "provider/*": {"reserveTokens": 1},
        "family/model": {"reserveTokens": 2}
    }}));
    assert_eq!(resolved(&settings, MODEL).reserve_tokens, 400000);
    for model in [
        ("other", "family/model"),
        ("provider", "other"),
        ("provider", "family/Model"),
        ("Provider", "family/model"),
    ] {
        assert_eq!(
            resolved(&settings, Some(model)),
            CompactionSettings::default()
        );
    }
}

#[test]
fn project_deep_merge_trust_reload_and_toggle_preserve_overrides() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(project.join(".pi")).unwrap();
    fs::write(
        root.path().join("settings.json"),
        json!({"compaction": {
            "reserveTokens": 8192,
            "modelOverrides": {
                KEY: {"enabled": false, "reserveTokens": 400000, "keepRecentTokens": 30000},
                "provider/other": {"keepRecentTokens": 4096}
            }
        }})
        .to_string(),
    )
    .unwrap();
    fs::write(
        project.join(".pi/settings.json"),
        json!({"compaction": {
            "reserveTokens": 1024,
            "modelOverrides": {KEY: {"keepRecentTokens": 2000}}
        }})
        .to_string(),
    )
    .unwrap();
    let manager = SettingsManager::new(root.path());
    let trusted = SettingsContext::new(&project, true);
    for _ in 0..2 {
        let settings = manager.load(&trusted);
        assert_eq!(
            resolved(&settings, MODEL),
            CompactionSettings {
                enabled: true,
                reserve_tokens: 400000,
                keep_recent_tokens: 2000,
            }
        );
        assert_eq!(
            resolved(&settings, Some(("provider", "other"))),
            CompactionSettings {
                enabled: true,
                reserve_tokens: 1024,
                keep_recent_tokens: 4096,
            }
        );
    }
    let untrusted = SettingsContext::new(&project, false);
    assert_eq!(
        resolved(&manager.load(&untrusted), MODEL).keep_recent_tokens,
        30000
    );
    manager
        .set_session_preference(SessionPreference::AutoCompaction(false))
        .unwrap();
    assert_eq!(
        resolved(&manager.load(&trusted), MODEL),
        CompactionSettings {
            enabled: false,
            reserve_tokens: 400000,
            keep_recent_tokens: 2000,
        }
    );
    assert_eq!(
        manager.load(&untrusted).raw_global()["compaction"]["modelOverrides"][KEY]["enabled"],
        false
    );
}

#[test]
fn invalid_model_values_are_lazy_but_invalid_ordinary_values_win_over_valid_overrides() {
    for field in ["reserveTokens", "keepRecentTokens"] {
        for (value, display) in [
            (json!(null), "null"),
            (json!(-1), "-1"),
            (json!(1.5), "1.5"),
            (json!("400000"), "400000"),
            (json!(true), "true"),
            (json!({}), "[object Object]"),
            (json!([]), ""),
            (json!(9_007_199_254_740_992_u64), "9007199254740992"),
        ] {
            let settings = load(json!({"modelOverrides": {KEY: {field: value}}}));
            assert_eq!(
                settings
                    .effective()
                    .compaction_for_model(MODEL)
                    .unwrap_err()
                    .to_string(),
                format!(
                    "Invalid compaction.modelOverrides[\"{KEY}\"].{field} setting: {display}. Expected a non-negative safe integer."
                )
            );
            assert_eq!(resolved(&settings, None), CompactionSettings::default());
            assert_eq!(
                resolved(&settings, Some(("other", "family/model"))),
                CompactionSettings::default()
            );
            let settings = load(json!({field: value, "modelOverrides": {KEY: {field: 4096}}}));
            for model in [None, MODEL] {
                assert_eq!(
                    settings
                        .effective()
                        .compaction_for_model(model)
                        .unwrap_err()
                        .to_string(),
                    format!(
                        "Invalid compaction.{field} setting: {display}. Expected a non-negative safe integer."
                    )
                );
            }
        }
    }
}

#[test]
fn malformed_selected_entries_fail_without_affecting_other_models() {
    for (entry, display) in [
        (json!(null), "null"),
        (json!(false), "false"),
        (json!(42), "42"),
        (json!("invalid"), "invalid"),
        (json!([]), ""),
    ] {
        let settings = load(json!({"modelOverrides": {KEY: entry}}));
        assert_eq!(
            settings
                .effective()
                .compaction_for_model(MODEL)
                .unwrap_err()
                .to_string(),
            format!(
                "Invalid compaction.modelOverrides[\"{KEY}\"] setting: {display}. Expected an object."
            )
        );
        assert_eq!(resolved(&settings, None), CompactionSettings::default());
    }
}

#[test]
fn zero_and_integral_json_floats_are_accepted_without_truthiness_fallback() {
    for tokens in [0, 9_007_199_254_740_991_u64] {
        let settings = load(json!({"reserveTokens": tokens, "keepRecentTokens": tokens}));
        assert_eq!(
            resolved(&settings, MODEL),
            CompactionSettings {
                enabled: true,
                reserve_tokens: tokens,
                keep_recent_tokens: tokens,
            }
        );
        let settings = load(json!({"reserveTokens": 1000, "keepRecentTokens": 1000,
            "modelOverrides": {KEY: {"reserveTokens": tokens, "keepRecentTokens": tokens}}}));
        assert_eq!(
            resolved(&settings, MODEL),
            CompactionSettings {
                enabled: true,
                reserve_tokens: tokens,
                keep_recent_tokens: tokens,
            }
        );
    }
    let settings = load(json!({"reserveTokens": 4096.0, "keepRecentTokens": -0.0}));
    assert_eq!(resolved(&settings, MODEL).reserve_tokens, 4096);
    assert_eq!(resolved(&settings, MODEL).keep_recent_tokens, 0);
}

#[test]
fn compaction_rejects_integers_outside_javascript_safe_range() {
    let settings = load(json!({"reserveTokens": 9_007_199_254_740_992_u64}));
    assert!(settings.diagnostics().iter().any(|diagnostic| {
        diagnostic.message.contains("compaction.reserveTokens")
            && diagnostic.message.contains("non-negative safe integer")
    }));
}
