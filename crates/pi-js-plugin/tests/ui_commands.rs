use std::sync::{Arc, Mutex};

use pi_js_plugin::{ExtensionContextNotification, execute_context_notification};
use pi_plugin::{
    ModelsContextAccess, PluginContextEpoch, PluginContextError, PluginContextResult,
    PluginContextScope, SessionContextAccess, UiCommand, UiContextAccess,
};
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingUi(Mutex<Vec<(String, UiCommand)>>);

impl ModelsContextAccess for RecordingUi {}
impl SessionContextAccess for RecordingUi {}
impl UiContextAccess for RecordingUi {
    fn ui_command(&self, owner: String, command: UiCommand) -> PluginContextResult<()> {
        self.0.lock().unwrap().push((owner, command));
        Ok(())
    }
}

#[test]
fn javascript_notifications_preserve_commands_and_reject_retired_generations() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../pi-plugin/tests/fixtures/ui-commands.json"
    ))
    .unwrap();
    let access = Arc::new(RecordingUi::default());
    let epoch = PluginContextEpoch::new(access.clone());
    let handle = epoch.handle(PluginContextScope::Base);
    for case in fixtures["valid"].as_array().unwrap() {
        let notification = json!({
            "type": "uiCommand", "owner": "js-generation", "command": case["value"]
        });
        let decoded: ExtensionContextNotification =
            serde_json::from_value(notification.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), notification);
        execute_context_notification(&handle, decoded).unwrap();
        let (owner, command) = access.0.lock().unwrap().pop().unwrap();
        assert_eq!(owner, "js-generation");
        assert_eq!(serde_json::to_value(command).unwrap(), case["value"]);
    }
    epoch.retire();
    for case in fixtures["valid"].as_array().unwrap() {
        let decoded = serde_json::from_value(json!({
            "type": "uiCommand", "owner": "js-generation", "command": case["value"]
        }))
        .unwrap();
        assert!(matches!(
            execute_context_notification(&handle, decoded),
            Err(PluginContextError::Retired)
        ));
    }
    assert!(access.0.lock().unwrap().is_empty());
}
