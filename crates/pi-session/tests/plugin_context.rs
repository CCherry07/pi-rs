use std::sync::{Arc, Mutex};

use pi_core::{
    AgentPlugin, CommandContextParts, InputContext, InputEvent, InputPatch, PluginContext,
    PluginContextScope, PluginError, PresentationMode, SessionEntryKind, Usage, UsageCost,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, PiPluginContext, PluginContextBinding, PluginUiBridge, SessionStartEvent,
    SessionStartReason,
};
use pi_test_support::ScriptedProviderPlugin;

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    mode: PresentationMode,
    has_ui: bool,
    trusted: bool,
    session_id: String,
}

struct NativeContextProbe {
    observed: Arc<Mutex<Option<Observation>>>,
    retained: Arc<Mutex<Option<InputContext>>>,
}

#[derive(Default)]
struct RecordingUiBridge {
    confirmations: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl PluginUiBridge for RecordingUiBridge {
    async fn confirm(&self, title: String, message: String) -> Result<bool, String> {
        self.confirmations.lock().unwrap().push((title, message));
        Ok(true)
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for NativeContextProbe {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("native-context-probe")
    }

    async fn input(
        &self,
        context: InputContext,
        _event: InputEvent,
    ) -> Result<InputPatch, PluginError> {
        *self.retained.lock().unwrap() = Some(context.clone());
        *self.observed.lock().unwrap() = Some(Observation {
            mode: context.ui.mode()?,
            has_ui: context.ui.is_available()?,
            trusted: context.session.is_project_trusted()?,
            session_id: context.session.id()?,
        });
        Ok(InputPatch::Continue)
    }
}

#[tokio::test]
async fn pi_plugin_context_binds_the_direct_native_plugin_view() {
    let directory = tempfile::tempdir().unwrap();
    let binding = PluginContextBinding::new();
    let ui_bridge = Arc::new(RecordingUiBridge::default());
    let access = Arc::new(
        PiPluginContext::new(PresentationMode::Tui, true, binding)
            .with_ui_bridge(ui_bridge.clone()),
    );
    let context_access: Arc<dyn PluginContext> = access.clone();
    let observed = Arc::new(Mutex::new(None));
    let retained = Arc::new(Mutex::new(None));
    let runtime = PiRuntime::builder()
        .agent_plugin(NativeContextProbe {
            observed: Arc::clone(&observed),
            retained: Arc::clone(&retained),
        })
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .plugin_context(context_access)
        .build()
        .unwrap();
    let prepared =
        AgentSession::prepare_create(runtime, directory.path().join("native-context.jsonl"))
            .await
            .unwrap();
    access.bind_generation_session(prepared.session());
    let session = prepared
        .activate(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })
        .await;

    session.runtime().process_input("hello").await.unwrap();

    assert_eq!(
        *observed.lock().unwrap(),
        Some(Observation {
            mode: PresentationMode::Tui,
            has_ui: true,
            trusted: true,
            session_id: session.log().header().id.clone(),
        })
    );

    let replacement_context = CommandContextParts::new(
        session
            .runtime()
            .plugin_context_handle(PluginContextScope::Command),
    );
    assert_eq!(
        replacement_context.ui.mode().unwrap(),
        PresentationMode::Tui
    );

    let entry_id = session
        .append_custom_entry("probe", Some(serde_json::json!({"kept": true})))
        .unwrap();
    session
        .set_label(&entry_id, Some("checkpoint".to_string()))
        .unwrap();
    session
        .set_name(Some("Snapshot DX".to_string()))
        .await
        .unwrap();

    let (provider_id, model_id) = session.runtime().agent().model_selection();
    assert_eq!(
        replacement_context.models.current().unwrap(),
        session.runtime().model(&provider_id, &model_id)
    );
    assert_eq!(
        replacement_context.models.thinking_level().unwrap(),
        Some(session.runtime().agent().thinking_level())
    );
    assert!(replacement_context.session.is_idle().unwrap());
    assert!(!replacement_context.session.has_pending_messages().unwrap());
    assert_eq!(
        replacement_context.session.system_prompt().unwrap(),
        session.runtime().agent().runtime().system_prompt()
    );
    assert_eq!(
        replacement_context.session.id().unwrap(),
        session.log().id()
    );
    assert_eq!(
        replacement_context.session.name().unwrap().as_deref(),
        Some("Snapshot DX")
    );
    assert_eq!(
        replacement_context
            .session
            .label(&entry_id)
            .unwrap()
            .as_deref(),
        Some("checkpoint")
    );
    assert_eq!(
        replacement_context
            .session
            .entry(&entry_id)
            .unwrap()
            .unwrap()["data"]["kept"],
        true
    );
    assert_eq!(
        replacement_context.session.leaf_entry().unwrap().unwrap()["id"],
        entry_id
    );
    assert_eq!(
        replacement_context.session.header().unwrap()["id"],
        session.log().id()
    );
    assert_eq!(
        replacement_context.session.active_tools().unwrap(),
        session.runtime().active_tools()
    );
    assert_eq!(
        replacement_context.session.tools().unwrap(),
        session.runtime().tool_specs()
    );
    assert_eq!(
        replacement_context.session.commands().unwrap(),
        session.runtime().command_specs()
    );
    assert!(
        replacement_context
            .ui
            .confirm("Import session?", "The current session will be replaced.")
            .await
            .unwrap()
    );
    assert_eq!(
        ui_bridge.confirmations.lock().unwrap().as_slice(),
        &[(
            "Import session?".to_string(),
            "The current session will be replaced.".to_string()
        )]
    );

    let snapshot = replacement_context.session.snapshot().unwrap();
    assert_eq!(snapshot.id(), session.log().header().id);
    assert_eq!(snapshot.cwd(), session.log().header().cwd);
    assert_eq!(snapshot.name(), Some("Snapshot DX"));
    assert_eq!(snapshot.leaf_id(), Some(entry_id.as_str()));
    assert_eq!(snapshot.label(&entry_id), Some("checkpoint"));
    assert_eq!(snapshot.leaf().unwrap().kind(), &SessionEntryKind::Custom);
    assert_eq!(snapshot.branch().last().unwrap().id(), entry_id);
    assert_eq!(
        snapshot.entry(&entry_id).unwrap().raw()["data"]["kept"],
        true
    );

    replacement_context
        .session
        .record_usage(
            Usage {
                input: 11,
                output: 3,
                cache_read: 17,
                cache_write: 5,
                total_tokens: 36,
                cost: UsageCost {
                    total: 0.25,
                    ..UsageCost::default()
                },
                ..Usage::default()
            },
            Some(serde_json::json!({"task": "background_review"})),
        )
        .unwrap();
    let document = session.log().load().unwrap();
    let usage = document
        .records
        .iter()
        .find_map(|record| match &record.record {
            pi_session::LaneRecordEntry::Usage(usage) => Some(usage),
            _ => None,
        })
        .expect("plugin usage is recorded outside the message tree");
    assert_eq!(usage.usage.input, 11);
    assert!(matches!(
        &usage.attribution,
        pi_session::UsageAttribution::Adjustment { details: Some(details), .. }
            if details["task"] == "background_review"
    ));
    assert_eq!(document.stats.total_tokens, 36);
    assert_eq!(document.stats.cost_total, 0.25);

    session.shutdown().await;
    assert!(matches!(
        retained.lock().unwrap().as_ref().unwrap().ui.mode(),
        Err(pi_core::PluginContextError::Retired)
    ));
    assert!(matches!(
        replacement_context.ui.mode(),
        Err(pi_core::PluginContextError::Retired)
    ));
}
