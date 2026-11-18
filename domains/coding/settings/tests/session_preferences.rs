use pi_settings::{QueueModeSetting, SessionPreference, SettingsContext, SettingsManager};
use serde_json::{Value, json};

#[test]
fn session_preferences_survive_reload_and_preserve_unrelated_settings() {
    let root = tempfile::tempdir().unwrap();
    let manager = SettingsManager::new(root.path());
    let path = root.path().join("settings.json");
    std::fs::write(
        &path,
        json!({
            "theme":"custom", "future":{"retained":true},
            "retry":{"maxRetries":7}, "compaction":{"reserveTokens":1234}
        })
        .to_string(),
    )
    .unwrap();
    for setting in [
        SessionPreference::SteeringMode(QueueModeSetting::All),
        SessionPreference::FollowUpMode(QueueModeSetting::OneAtATime),
        SessionPreference::AutoCompaction(false),
        SessionPreference::AutoRetry(false),
    ] {
        manager.set_session_preference(setting).unwrap();
    }
    let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({
            "theme":"custom", "future":{"retained":true},
            "retry":{"maxRetries":7,"enabled":false},
            "compaction":{"reserveTokens":1234,"enabled":false},
            "steeringMode":"all", "followUpMode":"one-at-a-time"
        })
    );
    let reloaded =
        SettingsManager::new(root.path()).load(&SettingsContext::new(root.path(), false));
    assert_eq!(reloaded.effective().steering_mode, QueueModeSetting::All);
    assert_eq!(
        reloaded.effective().follow_up_mode,
        QueueModeSetting::OneAtATime
    );
    assert!(!reloaded.effective().compaction.enabled);
    assert!(!reloaded.effective().retry.enabled);
}
