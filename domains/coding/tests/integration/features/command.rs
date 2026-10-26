use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use pi_plugin::{
    PresentationMode, UiCommand, UiInteraction, UiNode, UiNodeKind, UiSurfaceEvent, UiSurfaceFrame,
    UiSurfaceSnapshot,
};
use pi_session::{CreateSessionOptions, PluginUiBridge, PromptOutcome};
use serde_json::Value;

use super::*;

#[derive(Default)]
struct SelectionUi {
    steps: Mutex<VecDeque<SelectionAction>>,
    screens: Mutex<Vec<Vec<String>>>,
}

enum SelectionAction {
    Choose(&'static str),
    Toggle(&'static str),
    Space,
    Confirm,
    Cancel,
}

use SelectionAction::{Cancel, Choose, Confirm, Toggle};

impl SelectionUi {
    fn select(&self, steps: impl IntoIterator<Item = SelectionAction>) {
        self.steps.lock().unwrap().extend(steps);
    }
}

fn selection(node: &UiNode) -> Option<(&str, &[String])> {
    match &node.kind {
        UiNodeKind::Select { options, .. } => Some((&node.id, options)),
        UiNodeKind::Column { children, .. } | UiNodeKind::Row { children, .. } => {
            children.iter().find_map(selection)
        }
        UiNodeKind::Panel { child, .. } | UiNodeKind::Scroll { child, .. } => selection(child),
        _ => None,
    }
}

#[async_trait]
impl PluginUiBridge for SelectionUi {
    fn ui_command(&self, _owner: String, _command: UiCommand) -> Result<(), String> {
        Ok(())
    }

    async fn surface_exchange(&self, frame: UiSurfaceFrame) -> Result<UiSurfaceEvent, String> {
        let region = &frame.regions[0];
        let (node_id, options) = selection(&region.root).expect("feature selector");
        self.screens.lock().unwrap().push(options.to_vec());
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted selection");
        let option_index = |label| {
            options
                .iter()
                .position(|option| option.contains(label))
                .unwrap_or_else(|| panic!("missing {label} in {options:?}"))
        };
        let event = match step {
            SelectionAction::Choose(label) => UiInteraction::Submit {
                value: option_index(label).into(),
            },
            SelectionAction::Toggle(label) => {
                self.steps
                    .lock()
                    .unwrap()
                    .push_front(SelectionAction::Space);
                UiInteraction::Change {
                    value: option_index(label).into(),
                }
            }
            SelectionAction::Space => {
                assert!(
                    frame
                        .shortcuts
                        .iter()
                        .any(|shortcut| shortcut.key == "space")
                );
                UiInteraction::Shortcut {
                    key: "space".into(),
                }
            }
            SelectionAction::Confirm => UiInteraction::Submit { value: 0.into() },
            SelectionAction::Cancel => UiInteraction::Cancel,
        };
        Ok(UiSurfaceEvent::Interaction {
            region_id: region.id.clone(),
            node_id: node_id.into(),
            event,
            snapshot: UiSurfaceSnapshot::default(),
            revision: frame.revision,
        })
    }
}

async fn interactive(config: Config, ui: Arc<SelectionUi>) -> (Pi, PiSession) {
    let options = CreateSessionOptions::new(&config.cwd, &config.session_path);
    let pi = Pi::builder(config)
        .presentation_mode(PresentationMode::Tui)
        .plugin_ui_bridge(ui)
        .build()
        .unwrap();
    let session = pi.sessions().create_session(options).await.unwrap();
    (pi, session)
}

fn toggle_all(ui: &SelectionUi) {
    ui.select([
        Toggle("Memory"),
        Toggle("Subagents"),
        Toggle("Schedule"),
        Toggle("Skills"),
        Toggle("Prompt templates"),
        Toggle("Session transfer"),
        Confirm,
    ]);
}

#[tokio::test]
async fn selector_disables_all_features_and_reenables_them_without_changing_other_sessions() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture_config(root.path());
    let cwd = config.cwd.clone();
    let ui = Arc::new(SelectionUi::default());
    let (pi, session) = interactive(config, ui.clone()).await;
    let other = pi
        .sessions()
        .create_session(CreateSessionOptions::new(
            cwd,
            pi.agent_dir().join("sessions/other.jsonl"),
        ))
        .await
        .unwrap();

    for expected in [Features::none(), Features::all()] {
        let original = session.current();
        toggle_all(&ui);
        assert!(matches!(
            original.prompt("/features global").await.unwrap(),
            PromptOutcome::Handled
        ));
        assert!(!Arc::ptr_eq(&original, &session.current()));
        assert!(original.is_closed());
        assert_feature_registrations(&session.current(), &expected);
        assert_feature_registrations(&other.current(), &Features::all());
        assert!(!session.current().history().is_materialized());
        assert!(
            session
                .current()
                .catalog()
                .command_specs()
                .iter()
                .any(|spec| spec.name == "features")
        );
        let saved: Value =
            serde_json::from_slice(&fs::read(pi.agent_dir().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            saved["features"]["promptTemplates"],
            expected.prompt_templates
        );
        assert_eq!(
            saved["features"]["sessionTransfer"],
            expected.session_transfer
        );
    }
    assert!(
        pi.agent_dir()
            .join("skills")
            .join(SKILL_NAME)
            .join("SKILL.md")
            .exists()
    );
    assert!(ui.steps.lock().unwrap().is_empty());
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn selector_chooses_project_scope_and_patches_only_edited_flags() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture_config(root.path());
    let global = config.agent_dir.join("settings.json");
    let project = config.cwd.join(".pi/settings.json");
    let original = r#"{"features":{"memory":false,"schedule":false},"futureSetting":7}"#;
    fs::write(&global, original).unwrap();
    let ui = Arc::new(SelectionUi::default());
    ui.select([Choose("This project"), Toggle("Skills"), Confirm]);
    let (pi, session) = interactive(config, ui.clone()).await;
    assert!(matches!(
        session.current().prompt("/features").await.unwrap(),
        PromptOutcome::Handled
    ));
    assert_eq!(fs::read_to_string(global).unwrap(), original);
    let saved: Value = serde_json::from_slice(&fs::read(project).unwrap()).unwrap();
    assert_eq!(saved, json!({"features": {"skills": false}}));
    assert_feature_registrations(
        &session.current(),
        &Features {
            memory: false,
            schedule: false,
            skills: false,
            ..Features::all()
        },
    );
    assert!(ui.screens.lock().unwrap()[1][0].starts_with("[ ] Memory"));
    assert!(!session.current().history().is_materialized());
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn cancel_and_unchanged_save_leave_files_and_generation_untouched() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture_config(root.path());
    let path = config.agent_dir.join("settings.json");
    let ui = Arc::new(SelectionUi::default());
    let (pi, session) = interactive(config, ui.clone()).await;
    let original = session.current();
    ui.select([Cancel]);
    original.prompt("/features").await.unwrap();
    ui.select([Toggle("Memory"), Cancel]);
    original.prompt("/features global").await.unwrap();
    ui.select([Toggle("Skills"), Toggle("Skills"), Confirm]);
    original.prompt("/features global").await.unwrap();
    // Enter confirms an untouched selection without toggling the focused row.
    ui.select([Confirm]);
    original.prompt("/features global").await.unwrap();
    assert!(Arc::ptr_eq(&original, &session.current()));
    assert!(!path.exists());
    assert!(!original.history().is_materialized());
    assert_feature_registrations(&original, &Features::all());
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn selector_cannot_enable_host_disabled_features_or_persist_them_as_overrides() {
    let root = tempfile::tempdir().unwrap();
    let mut config = fixture_config(root.path());
    config.features.memory = false;
    let ui = Arc::new(SelectionUi::default());
    ui.select([Toggle("Memory"), Toggle("Subagents"), Confirm]);
    let (pi, session) = interactive(config, ui.clone()).await;
    session.current().prompt("/features global").await.unwrap();
    assert_feature_registrations(
        &session.current(),
        &Features {
            memory: false,
            subagents: false,
            ..Features::all()
        },
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(pi.agent_dir().join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved, json!({"features": {"subagents": false}}));
    assert!(ui.screens.lock().unwrap()[0][0].starts_with("[ ] Memory"));
    assert!(ui.screens.lock().unwrap()[0][0].contains("disabled by host"));
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_activation_reports_saved_settings_and_retains_the_working_generation() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture_config(root.path());
    write_features(
        &config.agent_dir.join("settings.json"),
        Features {
            memory: false,
            ..Features::all()
        },
    );
    fs::write(
        config.agent_dir.join("memory.json"),
        "invalid memory config",
    )
    .unwrap();
    let ui = Arc::new(SelectionUi::default());
    ui.select([Toggle("Memory"), Confirm]);
    let (pi, session) = interactive(config, ui).await;
    let original = session.current();
    let mut events = original.subscribe().events;
    let error = original
        .prompt("/features global")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Features saved to"), "{error}");
    assert!(error.contains("reload failed"), "{error}");
    assert!(error.contains("/reload to retry"), "{error}");
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|envelope| matches!(
        envelope.event,
        pi_session::AgentSessionEvent::PluginNotice { level: pi_plugin::NoticeLevel::Error, message }
            if message.contains("reload failed")
    )));
    assert!(Arc::ptr_eq(&original, &session.current()));
    assert!(!original.is_closed());
    assert_feature_registrations(
        &original,
        &Features {
            memory: false,
            ..Features::all()
        },
    );
    assert!(!original.history().is_materialized());
    let saved: Value =
        serde_json::from_slice(&fs::read(pi.agent_dir().join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["features"]["memory"], true);
    pi.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_scope_and_noninteractive_use_do_not_write_settings() {
    let root = tempfile::tempdir().unwrap();
    let mut config = fixture_config(root.path());
    config.trust_override = Some(false);
    let path = config.agent_dir.join("settings.json");
    let project_path = config.cwd.join(".pi/settings.json");
    let ui = Arc::new(SelectionUi::default());
    let (pi, session) = interactive(config.clone(), ui.clone()).await;
    for (command, message) in [
        ("/features project", "project trust"),
        ("/features invalid", "usage:"),
    ] {
        let error = session
            .current()
            .prompt(command)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
    assert!(ui.screens.lock().unwrap().is_empty());
    assert!(!project_path.exists());
    assert!(!path.exists());
    pi.sessions().shutdown().await.unwrap();

    let (pi, session) = create_session(config).await;
    let error = session
        .current()
        .prompt("/features")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("interactive TUI"), "{error}");
    assert!(!path.exists());
    assert!(!session.current().history().is_materialized());
    pi.sessions().shutdown().await.unwrap();
}
