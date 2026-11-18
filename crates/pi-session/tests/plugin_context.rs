use std::sync::{Arc, Mutex};

use pi_core::{Usage, UsageCost};
use pi_plugin::{
    CommandContextParts, InputContext, InputEvent, InputPatch, Plugin, PluginContext,
    PluginContextScope, PluginError, PresentationMode, SessionEntryKind, UiDialogRequest,
    UiDialogResponse,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, MultiSessionManager, PiPluginContext, PluginContextBinding,
    PluginUiBridge, PreparedSessionGeneration, SessionGenerationRequest,
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
    fn ui_command(&self, _owner: String, _command: pi_plugin::UiCommand) -> Result<(), String> {
        Ok(())
    }
    async fn surface_exchange(
        &self,
        frame: pi_plugin::UiSurfaceFrame,
    ) -> Result<pi_plugin::UiSurfaceEvent, String> {
        let pi_plugin::UiNodeKind::Panel { title, child } = &frame.regions[0].root.kind else {
            panic!("expected dialog panel");
        };
        let pi_plugin::UiNodeKind::Column { children, .. } = &child.kind else {
            panic!("expected dialog content");
        };
        let pi_plugin::UiNodeKind::Text { text: message, .. } = &children[0].kind else {
            panic!("expected confirmation description");
        };
        self.confirmations
            .lock()
            .unwrap()
            .push((title.into(), message.into()));
        Ok(pi_plugin::UiSurfaceEvent::Interaction {
            region_id: "dialog".into(),
            node_id: "value".into(),
            event: pi_plugin::UiInteraction::Submit { value: 0.into() },
            snapshot: pi_plugin::UiSurfaceSnapshot::default(),
            revision: frame.revision,
        })
    }
}

#[tokio::test]
async fn ui_availability_requires_a_frontend_capability_for_its_mode() {
    let without_bridge = pi_plugin::PluginContextEpoch::new(Arc::new(PiPluginContext::new(
        PresentationMode::Tui,
        true,
        PluginContextBinding::new(),
    )));
    assert!(!without_bridge.context().ui.is_available().unwrap());

    let with_bridge = pi_plugin::PluginContextEpoch::new(Arc::new(
        PiPluginContext::new(PresentationMode::Tui, true, PluginContextBinding::new())
            .with_ui_bridge(Arc::new(RecordingUiBridge::default())),
    ));
    assert!(with_bridge.context().ui.is_available().unwrap());

    let noninteractive = pi_plugin::PluginContextEpoch::new(Arc::new(
        PiPluginContext::new(PresentationMode::Print, true, PluginContextBinding::new())
            .with_ui_bridge(Arc::new(RecordingUiBridge::default())),
    ));
    assert!(!noninteractive.context().ui.is_available().unwrap());

    let rpc_without_confirmation = pi_plugin::PluginContextEpoch::new(Arc::new(
        PiPluginContext::new(PresentationMode::Rpc, true, PluginContextBinding::new())
            .with_ui_bridge(Arc::new(RecordingUiBridge::default())),
    ));
    assert!(
        !rpc_without_confirmation
            .context()
            .ui
            .is_available()
            .unwrap()
    );
}

/// A graphical frontend that answers yes/no natively but renders no plugin
/// surfaces, as Desktop does in RPC presentation mode.
struct SemanticConfirmBridge {
    answer: bool,
    asked: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl PluginUiBridge for SemanticConfirmBridge {
    async fn confirm(&self, title: String, message: String) -> Result<Option<bool>, String> {
        self.asked.lock().unwrap().push((title, message));
        Ok(Some(self.answer))
    }

    fn supports_confirm(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn rpc_frontends_answer_semantic_confirmations_without_plugin_surfaces() {
    for answer in [true, false] {
        let bridge = Arc::new(SemanticConfirmBridge {
            answer,
            asked: Mutex::new(Vec::new()),
        });
        let epoch = pi_plugin::PluginContextEpoch::new(Arc::new(
            PiPluginContext::new(PresentationMode::Rpc, true, PluginContextBinding::new())
                .with_ui_bridge(bridge.clone()),
        ));
        let ui = epoch.context().ui;
        assert!(ui.is_available().unwrap());
        assert!(ui.can_confirm().unwrap());
        assert_eq!(
            ui.select("Unsupported", vec!["Choice".into()])
                .await
                .unwrap(),
            None
        );
        assert_eq!(ui.confirm("Send?", "向张三发送").await.unwrap(), answer);
        assert_eq!(
            bridge.asked.lock().unwrap().as_slice(),
            [("Send?".to_string(), "向张三发送".to_string())]
        );
    }

    // Without a bridge, an RPC confirmation can never be answered yes.
    let unbridged = pi_plugin::PluginContextEpoch::new(Arc::new(PiPluginContext::new(
        PresentationMode::Rpc,
        true,
        PluginContextBinding::new(),
    )));
    let ui = unbridged.context().ui;
    assert!(!ui.is_available().unwrap());
    assert!(!ui.can_confirm().unwrap());
    assert!(!ui.confirm("Send?", "x").await.unwrap());
}

struct SemanticDialogBridge {
    answer: Mutex<UiDialogResponse>,
    asked: Mutex<Vec<(UiDialogRequest, Option<u64>)>>,
    notices: Mutex<Vec<(pi_plugin::NoticeLevel, String)>>,
}

#[async_trait::async_trait]
impl PluginUiBridge for SemanticDialogBridge {
    async fn dialog(
        &self,
        request: UiDialogRequest,
        timeout_ms: Option<u64>,
    ) -> Result<Option<UiDialogResponse>, String> {
        self.asked.lock().unwrap().push((request, timeout_ms));
        Ok(Some(self.answer.lock().unwrap().clone()))
    }

    fn supports_confirm(&self) -> bool {
        true
    }

    fn notify(&self, level: pi_plugin::NoticeLevel, message: String) -> Result<bool, String> {
        self.notices.lock().unwrap().push((level, message));
        Ok(true)
    }
}

#[tokio::test]
async fn rpc_native_dialogs_preserve_results_and_notify_before_session_binding() {
    let bridge = Arc::new(SemanticDialogBridge {
        answer: Mutex::new(UiDialogResponse::Confirmed(true)),
        asked: Mutex::default(),
        notices: Mutex::default(),
    });
    let epoch = pi_plugin::PluginContextEpoch::new(Arc::new(
        PiPluginContext::new(PresentationMode::Rpc, true, PluginContextBinding::new())
            .with_ui_bridge(bridge.clone()),
    ));
    let ui = epoch.context().ui;
    assert!(ui.confirm("Continue?", "启动").await.unwrap());
    *bridge.answer.lock().unwrap() = UiDialogResponse::Value("Production".into());
    assert_eq!(
        ui.select("Environment", vec!["Test".into(), "Production".into()])
            .await
            .unwrap(),
        Some(1)
    );
    *bridge.answer.lock().unwrap() = UiDialogResponse::Value(String::new());
    assert_eq!(
        ui.input("Name", "可留空").await.unwrap(),
        Some(String::new())
    );
    *bridge.answer.lock().unwrap() = UiDialogResponse::Value("一行\n二行".into());
    assert_eq!(
        ui.editor("Notes", "初始文本").await.unwrap().as_deref(),
        Some("一行\n二行")
    );
    assert_eq!(
        bridge.asked.lock().unwrap().as_slice(),
        &[
            (
                UiDialogRequest::Confirm {
                    title: "Continue?".into(),
                    message: "启动".into()
                },
                None
            ),
            (
                UiDialogRequest::Select {
                    title: "Environment".into(),
                    options: vec!["Test".into(), "Production".into()]
                },
                None
            ),
            (
                UiDialogRequest::Input {
                    title: "Name".into(),
                    placeholder: "可留空".into()
                },
                None
            ),
            (
                UiDialogRequest::Editor {
                    title: "Notes".into(),
                    prefill: "初始文本".into()
                },
                None
            ),
        ]
    );

    // A supported cancellation must complete instead of trying a terminal surface.
    *bridge.answer.lock().unwrap() = UiDialogResponse::Cancelled;
    assert!(!ui.confirm("Continue?", "x").await.unwrap());
    assert_eq!(ui.select("Pick", vec!["One".into()]).await.unwrap(), None);
    assert_eq!(ui.input("Name", "").await.unwrap(), None);
    assert_eq!(ui.editor("Notes", "").await.unwrap(), None);
    *bridge.answer.lock().unwrap() = UiDialogResponse::Value("not offered".into());
    assert_eq!(ui.select("Pick", vec!["One".into()]).await.unwrap(), None);

    ui.notify(pi_plugin::NoticeLevel::Warning, "startup notice")
        .unwrap();
    assert_eq!(
        bridge.notices.lock().unwrap().as_slice(),
        &[(pi_plugin::NoticeLevel::Warning, "startup notice".into())]
    );
}

#[tokio::test]
async fn retiring_semantic_dialogs_keeps_the_next_generations_requests_alive() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{mpsc, oneshot};

    struct PendingDialogs {
        requests: mpsc::UnboundedSender<oneshot::Sender<UiDialogResponse>>,
        active: Arc<AtomicUsize>,
    }
    struct ActiveDialog(Arc<AtomicUsize>);
    impl Drop for ActiveDialog {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl PluginUiBridge for PendingDialogs {
        async fn dialog(
            &self,
            _request: UiDialogRequest,
            _timeout_ms: Option<u64>,
        ) -> Result<Option<UiDialogResponse>, String> {
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveDialog(self.active.clone());
            let (sender, receiver) = oneshot::channel();
            self.requests.send(sender).unwrap();
            Ok(Some(receiver.await.unwrap_or(UiDialogResponse::Cancelled)))
        }
    }

    let (requests, mut receiver) = mpsc::unbounded_channel();
    let active = Arc::new(AtomicUsize::new(0));
    let bridge = Arc::new(PendingDialogs {
        requests,
        active: active.clone(),
    });
    let make_epoch = || {
        pi_plugin::PluginContextEpoch::new(Arc::new(
            PiPluginContext::new(PresentationMode::Rpc, true, PluginContextBinding::new())
                .with_ui_bridge(bridge.clone()),
        ))
    };
    let old = make_epoch();
    let next = make_epoch();
    let old_ui = old.context().ui;
    let next_ui = next.context().ui;
    let old_task = tokio::spawn(async move { old_ui.input("Old", "").await });
    let old_response = receiver.recv().await.unwrap();
    let next_task = tokio::spawn(async move { next_ui.input("Next", "").await });
    let next_response = receiver.recv().await.unwrap();
    assert_eq!(active.load(Ordering::SeqCst), 2);
    old.retire();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(3), old_task)
            .await
            .unwrap()
            .unwrap(),
        Err(pi_plugin::PluginContextError::Retired)
    ));
    assert_eq!(active.load(Ordering::SeqCst), 1);
    assert!(old_response.is_closed());
    assert!(!next_task.is_finished());
    next_response
        .send(UiDialogResponse::Value("next response".into()))
        .unwrap();
    assert_eq!(
        next_task.await.unwrap().unwrap().as_deref(),
        Some("next response")
    );
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn semantic_dialog_reply_rechecks_retirement_after_polling_the_bridge() {
    struct RetiringBridge(Mutex<Option<pi_plugin::PluginContextEpoch>>);

    #[async_trait::async_trait]
    impl PluginUiBridge for RetiringBridge {
        async fn dialog(
            &self,
            _request: UiDialogRequest,
            _timeout_ms: Option<u64>,
        ) -> Result<Option<UiDialogResponse>, String> {
            // The retirement waiter was polled before this ready response.
            // Retiring here reproduces a reply racing with a generation swap.
            let epoch = self.0.lock().unwrap().take().unwrap();
            epoch.retire();
            Ok(Some(UiDialogResponse::Confirmed(true)))
        }
    }

    let bridge = Arc::new(RetiringBridge(Mutex::default()));
    let epoch = pi_plugin::PluginContextEpoch::new(Arc::new(
        PiPluginContext::new(PresentationMode::Rpc, true, PluginContextBinding::new())
            .with_ui_bridge(bridge.clone()),
    ));
    *bridge.0.lock().unwrap() = Some(epoch.clone());
    assert!(matches!(
        epoch.context().ui.confirm("Continue?", "x").await,
        Err(pi_plugin::PluginContextError::Retired)
    ));
}

#[pi_plugin::plugin]
impl Plugin for NativeContextProbe {
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
        .plugin(NativeContextProbe {
            observed: Arc::clone(&observed),
            retained: Arc::clone(&retained),
        })
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .plugin_context(context_access)
        .build()
        .unwrap();
    let manager = MultiSessionManager::new({
        let runtime = runtime.clone();
        let access = access.clone();
        move |_: SessionGenerationRequest| {
            let runtime = runtime.clone();
            let access = access.clone();
            async move {
                Ok(
                    PreparedSessionGeneration::new(runtime, AgentSessionOptions::default())
                        .bind_session(move |session| access.bind_generation_session(session)),
                )
            }
        }
    });
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            runtime.cwd(),
            directory.path().join("native-context.jsonl"),
        ))
        .await
        .unwrap()
        .current();

    runtime.process_input("hello").await.unwrap();

    assert_eq!(
        *observed.lock().unwrap(),
        Some(Observation {
            mode: PresentationMode::Tui,
            has_ui: true,
            trusted: true,
            session_id: session.history().header().id.clone(),
        })
    );

    let replacement_context =
        CommandContextParts::new(session.plugin_context(PluginContextScope::Command));
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
    // Like Pi's _appendEntry, label/name metadata also advances the current leaf.
    let leaf_id = session.history().leaf_id().unwrap();

    let state = session.snapshot().agent;
    let (provider_id, model_id) = (state.provider_id, state.model_id);
    assert_eq!(
        replacement_context.models.current().unwrap(),
        session.catalog().model(&provider_id, &model_id)
    );
    assert_eq!(
        replacement_context.models.thinking_level().unwrap(),
        Some(session.snapshot().agent.thinking_level)
    );
    assert!(replacement_context.session.is_idle().unwrap());
    assert!(!replacement_context.session.has_pending_messages().unwrap());
    assert_eq!(
        replacement_context.session.system_prompt().unwrap(),
        session.snapshot().agent.system_prompt
    );
    assert_eq!(
        replacement_context.session.id().unwrap(),
        session.history().id()
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
        leaf_id
    );
    assert_eq!(
        replacement_context.session.header().unwrap()["id"],
        session.history().id()
    );
    assert_eq!(
        replacement_context.session.active_tools().unwrap(),
        session.snapshot().agent.active_tools
    );
    assert_eq!(
        replacement_context.session.tools().unwrap(),
        session.catalog().tool_specs()
    );
    assert_eq!(
        replacement_context.session.commands().unwrap(),
        session.catalog().command_specs()
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
    assert_eq!(snapshot.id(), session.history().header().id);
    assert_eq!(snapshot.cwd(), session.history().header().cwd);
    assert_eq!(snapshot.name(), Some("Snapshot DX"));
    assert_eq!(snapshot.leaf_id(), Some(leaf_id.as_str()));
    assert_eq!(snapshot.label(&entry_id), Some("checkpoint"));
    assert_eq!(snapshot.leaf().unwrap().kind(), &SessionEntryKind::Custom);
    assert_eq!(snapshot.branch().last().unwrap().id(), leaf_id);
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
    let document = session.history().load().unwrap();
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
        Err(pi_plugin::PluginContextError::Retired)
    ));
    assert!(matches!(
        replacement_context.ui.mode(),
        Err(pi_plugin::PluginContextError::Retired)
    ));
}

#[tokio::test]
async fn standard_native_dialogs_close_on_completion_or_generation_retirement() {
    use pi_plugin::{
        PluginContextEpoch, PluginContextError, UiCommand, UiSurfaceCommand, UiSurfaceEvent,
        UiSurfaceFrame,
    };
    use std::collections::HashMap;
    use tokio::sync::{mpsc, oneshot};

    struct PendingUiBridge {
        frames: mpsc::UnboundedSender<UiSurfaceFrame>,
        pending: Mutex<HashMap<String, oneshot::Sender<UiSurfaceEvent>>>,
        closed: Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl PluginUiBridge for PendingUiBridge {
        fn ui_command(&self, owner: String, command: UiCommand) -> Result<(), String> {
            if command == UiCommand::Surface(UiSurfaceCommand::Close) {
                self.closed.lock().unwrap().push(owner.clone());
                if let Some(sender) = self.pending.lock().unwrap().remove(&owner) {
                    let _ = sender.send(UiSurfaceEvent::Closed);
                }
            }
            Ok(())
        }
        async fn surface_exchange(&self, frame: UiSurfaceFrame) -> Result<UiSurfaceEvent, String> {
            let (sender, receiver) = oneshot::channel();
            self.pending
                .lock()
                .unwrap()
                .insert(frame.owner.clone(), sender);
            self.frames.send(frame).unwrap();
            Ok(receiver.await.unwrap_or(UiSurfaceEvent::Closed))
        }
    }

    for (confirm, retire_pending) in [(false, false), (false, true), (true, false), (true, true)] {
        let (frames, mut receiver) = mpsc::unbounded_channel();
        let bridge = Arc::new(PendingUiBridge {
            frames,
            pending: Mutex::new(HashMap::new()),
            closed: Mutex::new(Vec::new()),
        });
        let context = Arc::new(
            PiPluginContext::new(PresentationMode::Tui, true, PluginContextBinding::new())
                .with_ui_bridge(bridge.clone()),
        );
        let epoch = PluginContextEpoch::new(context);
        let ui = epoch.context().ui;
        let task = tokio::spawn(async move {
            if confirm {
                ui.confirm("Continue?", "Replace this session.")
                    .await
                    .map(|_| ())
            } else {
                ui.select("Environment", vec!["Test".into(), "Production".into()])
                    .await
                    .map(|_| ())
            }
        });
        let frame = tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(frame.owner.starts_with("native-ui:"));
        assert!(frame.validate().is_ok());
        if retire_pending {
            epoch.retire();
            assert!(matches!(
                tokio::time::timeout(std::time::Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(PluginContextError::Retired)
            ));
        } else {
            bridge
                .pending
                .lock()
                .unwrap()
                .remove(&frame.owner)
                .unwrap()
                .send(UiSurfaceEvent::Interaction {
                    region_id: "dialog".into(),
                    node_id: "value".into(),
                    event: pi_plugin::UiInteraction::Submit { value: 0.into() },
                    snapshot: pi_plugin::UiSurfaceSnapshot::default(),
                    revision: frame.revision,
                })
                .unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_ok()
            );
            epoch.retire();
        }
        assert_eq!(*bridge.closed.lock().unwrap(), [frame.owner]);
        assert!(bridge.pending.lock().unwrap().is_empty());
    }
}
