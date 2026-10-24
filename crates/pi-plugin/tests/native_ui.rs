use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_plugin::prelude::*;
use tokio::sync::{mpsc, oneshot};

struct Access {
    frames: mpsc::UnboundedSender<(UiSurfaceFrame, oneshot::Sender<UiSurfaceEvent>)>,
    commands: Mutex<Vec<(String, UiCommand)>>,
    pending: Mutex<Vec<oneshot::Sender<UiSurfaceEvent>>>,
    interactive: bool,
}
impl ModelsContextAccess for Access {}
#[async_trait]
impl SessionContextAccess for Access {}
#[async_trait]
impl UiContextAccess for Access {
    fn mode(&self) -> PluginContextResult<PresentationMode> {
        Ok(if self.interactive {
            PresentationMode::Tui
        } else {
            PresentationMode::Print
        })
    }
    fn has_ui(&self) -> PluginContextResult<bool> {
        Ok(self.interactive)
    }
    fn ui_surface_snapshot(&self) -> PluginContextResult<UiSurfaceSnapshot> {
        Ok(UiSurfaceSnapshot::default())
    }
    fn ui_command(&self, owner: String, command: UiCommand) -> PluginContextResult<()> {
        self.commands.lock().unwrap().push((owner, command));
        Ok(())
    }
    async fn ui_surface_exchange(
        &self,
        frame: UiSurfaceFrame,
    ) -> PluginContextResult<UiSurfaceEvent> {
        let (tx, rx) = oneshot::channel();
        self.frames.send((frame, tx)).unwrap();
        Ok(rx.await.unwrap_or(UiSurfaceEvent::Closed))
    }
    fn ui_retire(&self) {
        for sender in self.pending.lock().unwrap().drain(..) {
            let _ = sender.send(UiSurfaceEvent::Closed);
        }
    }
}

struct Component {
    drops: Arc<AtomicUsize>,
    waker: Arc<Mutex<Option<UiComponentWaker>>>,
    fail_render: bool,
}
impl Drop for Component {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}
impl UiComponent for Component {
    type Output = String;
    fn mount(&mut self, waker: UiComponentWaker) -> PluginContextResult<()> {
        *self.waker.lock().unwrap() = Some(waker);
        Ok(())
    }
    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
        if self.fail_render {
            return Err(PluginContextError::Failed("renderer failed".into()));
        }
        Ok(UiSurfaceFrame {
            owner: "component-cannot-impersonate-another-owner".into(),
            revision: u64::MAX,
            regions: vec![UiRegion {
                id: "dialog".into(),
                placement: UiRegionPlacement::Overlay,
                root: UiNode::new(
                    "name",
                    UiNodeKind::Input {
                        initial_value: "界".into(),
                        placeholder: "Name".into(),
                        completions: Vec::new(),
                    },
                ),
                overlay: None,
            }],
            ..Default::default()
        })
    }
    fn handle_event(
        &mut self,
        event: UiSurfaceEvent,
    ) -> PluginContextResult<UiComponentUpdate<String>> {
        Ok(match event {
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Change { value },
                ..
            } => UiComponentUpdate::Continue(vec![UiCommand::Product(UiProductCommand::Title {
                title: value.as_str().unwrap().into(),
            })]),
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Submit { value },
                ..
            } => UiComponentUpdate::Complete(value.as_str().unwrap().into()),
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Cancel,
                ..
            } => UiComponentUpdate::Cancel,
            _ => UiComponentUpdate::Continue(Vec::new()),
        })
    }
}

fn interaction(revision: u64, event: UiInteraction) -> UiSurfaceEvent {
    UiSurfaceEvent::Interaction {
        region_id: "dialog".into(),
        node_id: "name".into(),
        event,
        snapshot: UiSurfaceSnapshot::default(),
        revision,
    }
}

fn component() -> Component {
    Component {
        drops: Arc::new(AtomicUsize::new(0)),
        waker: Arc::new(Mutex::new(None)),
        fail_render: false,
    }
}
type FrameReceiver = mpsc::UnboundedReceiver<(UiSurfaceFrame, oneshot::Sender<UiSurfaceEvent>)>;
fn host(interactive: bool) -> (PluginContextEpoch, Arc<Access>, FrameReceiver) {
    let (frames, receiver) = mpsc::unbounded_channel();
    let access = Arc::new(Access {
        frames,
        commands: Mutex::new(Vec::new()),
        pending: Mutex::new(Vec::new()),
        interactive,
    });
    (PluginContextEpoch::new(access.clone()), access, receiver)
}
async fn next(receiver: &mut FrameReceiver) -> (UiSurfaceFrame, oneshot::Sender<UiSurfaceEvent>) {
    tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn native_component_uses_shared_frames_orders_effects_and_closes_once() {
    let (epoch, access, mut frames) = host(true);
    let panel = component();
    let drops = panel.drops.clone();
    let waker = panel.waker.clone();
    let (_, signal) = AbortHandle::new();
    let task = tokio::spawn(async move { epoch.context().ui.custom(panel, signal).await });
    let (first, response) = next(&mut frames).await;
    assert!(first.owner.starts_with("native-ui:"));
    assert_eq!(first.revision, 1);
    assert_eq!(first.regions[0].root.id, "name");
    response
        .send(UiSurfaceEvent::Refresh {
            snapshot: UiSurfaceSnapshot {
                columns: 41,
                ..Default::default()
            },
        })
        .unwrap();
    let (second, response) = next(&mut frames).await;
    assert_eq!(second.owner, first.owner);
    assert_eq!(second.revision, 1);
    assert_eq!(second.regions, first.regions);
    response
        .send(interaction(
            second.revision,
            UiInteraction::Change {
                value: "updated".into(),
            },
        ))
        .unwrap();
    let (third, response) = next(&mut frames).await;
    assert_eq!(third.revision, 1);
    assert_eq!(
        *access.commands.lock().unwrap(),
        [(
            first.owner.clone(),
            UiCommand::Product(UiProductCommand::Title {
                title: "updated".into()
            })
        )]
    );
    waker
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .request_render()
        .unwrap();
    response
        .send(interaction(
            third.revision,
            UiInteraction::Submit {
                value: "你好".into(),
            },
        ))
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap().as_deref(), Some("你好"));
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        access
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, command)| { *command == UiCommand::Surface(UiSurfaceCommand::Close) })
            .count(),
        1
    );
    assert!(
        waker
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .request_render()
            .is_err()
    );
}

#[tokio::test]
async fn abort_and_dropping_the_future_release_pending_surfaces() {
    for drop_future in [false, true] {
        let (epoch, access, mut frames) = host(true);
        let panel = component();
        let drops = panel.drops.clone();
        let (abort, signal) = AbortHandle::new();
        let task = tokio::spawn(async move { epoch.context().ui.custom(panel, signal).await });
        let (frame, response) = next(&mut frames).await;
        if drop_future {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            abort.abort();
            assert_eq!(task.await.unwrap().unwrap(), None);
        }
        assert!(response.is_closed());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            *access.commands.lock().unwrap(),
            [(frame.owner, UiCommand::Surface(UiSurfaceCommand::Close))]
        );
    }
}

#[tokio::test]
async fn retirement_prevents_late_callbacks_and_rejects_a_new_mount() {
    let (epoch, access, mut frames) = host(true);
    let panel = component();
    let drops = panel.drops.clone();
    let waker = panel.waker.clone();
    let (_, signal) = AbortHandle::new();
    let ui = epoch.context().ui;
    let task = tokio::spawn(async move { ui.custom(panel, signal).await });
    let (_, response) = next(&mut frames).await;
    access.pending.lock().unwrap().push(response);
    epoch.retire();
    assert!(matches!(
        task.await.unwrap(),
        Err(PluginContextError::Retired)
    ));
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(
        waker
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .request_render()
            .is_err()
    );
    let (_, signal) = AbortHandle::new();
    assert!(matches!(
        epoch.context().ui.custom(component(), signal).await,
        Err(PluginContextError::Retired)
    ));
}

#[tokio::test]
async fn noninteractive_and_preaborted_calls_never_mount_or_render() {
    for interactive in [false, true] {
        let (epoch, access, mut frames) = host(interactive);
        let panel = component();
        let drops = panel.drops.clone();
        let (abort, signal) = AbortHandle::new();
        if interactive {
            abort.abort();
        }
        assert_eq!(
            epoch.context().ui.custom(panel, signal).await.unwrap(),
            None
        );
        assert!(frames.try_recv().is_err());
        assert!(access.commands.lock().unwrap().is_empty());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn render_failure_closes_without_publishing_a_broken_frame() {
    let (epoch, access, mut frames) = host(true);
    let mut panel = component();
    panel.fail_render = true;
    let drops = panel.drops.clone();
    let (_, signal) = AbortHandle::new();
    assert!(
        matches!(epoch.context().ui.custom(panel, signal).await, Err(PluginContextError::Failed(message)) if message == "renderer failed")
    );
    assert!(frames.try_recv().is_err());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(matches!(
        access.commands.lock().unwrap().as_slice(),
        [(_, UiCommand::Surface(UiSurfaceCommand::Close))]
    ));
}

#[tokio::test]
async fn stale_document_events_do_not_invoke_current_component_callbacks() {
    let (epoch, _access, mut frames) = host(true);
    let (_, signal) = AbortHandle::new();
    let task = tokio::spawn(async move { epoch.context().ui.custom(component(), signal).await });
    let (_, response) = next(&mut frames).await;
    response
        .send(interaction(
            0,
            UiInteraction::Submit {
                value: "stale".into(),
            },
        ))
        .unwrap();
    let (current, response) = next(&mut frames).await;
    assert_eq!(current.revision, 1);
    response
        .send(interaction(
            current.revision,
            UiInteraction::Submit {
                value: "current".into(),
            },
        ))
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap().as_deref(), Some("current"));
}

#[test]
fn declarative_wire_uses_semantic_nodes_and_events() {
    let node: UiNode = serde_json::from_value(json!({
        "id": "name", "type": "input", "initialValue": "名字", "placeholder": "Name"
    }))
    .unwrap();
    assert!(
        matches!(&node.kind, UiNodeKind::Input { initial_value, .. } if initial_value == "名字")
    );
    let encoded = serde_json::to_value(interaction(
        7,
        UiInteraction::Submit {
            value: "名字".into(),
        },
    ))
    .unwrap();
    assert_eq!(encoded["type"], "interaction");
    assert_eq!(encoded["nodeId"], "name");
    assert_eq!(
        encoded["event"],
        json!({ "type": "submit", "value": "名字" })
    );
    assert_eq!(encoded["revision"], 7);
    assert!(
        serde_json::from_value::<UiSurfaceEvent>(
            json!({"type": "input", "data": "x", "sequence": 1})
        )
        .is_err()
    );
}

#[tokio::test]
async fn document_revision_changes_only_when_component_description_changes() {
    struct Counter(u32);
    impl UiComponent for Counter {
        type Output = u32;
        fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
            Ok(UiSurfaceFrame {
                regions: vec![UiRegion {
                    id: "dialog".into(),
                    placement: UiRegionPlacement::Overlay,
                    root: UiNode::new(
                        "name",
                        UiNodeKind::Button {
                            text: self.0.to_string(),
                        },
                    ),
                    overlay: None,
                }],
                ..Default::default()
            })
        }
        fn handle_event(
            &mut self,
            event: UiSurfaceEvent,
        ) -> PluginContextResult<UiComponentUpdate<u32>> {
            Ok(match event {
                UiSurfaceEvent::Interaction {
                    event: UiInteraction::Activate,
                    ..
                } => {
                    self.0 += 1;
                    UiComponentUpdate::Continue(Vec::new())
                }
                UiSurfaceEvent::Interaction {
                    event: UiInteraction::Submit { .. },
                    ..
                } => UiComponentUpdate::Complete(self.0),
                _ => UiComponentUpdate::Continue(Vec::new()),
            })
        }
    }
    let (epoch, _access, mut frames) = host(true);
    let (_, signal) = AbortHandle::new();
    let task = tokio::spawn(async move { epoch.context().ui.custom(Counter(0), signal).await });
    let (first, response) = next(&mut frames).await;
    response
        .send(interaction(first.revision, UiInteraction::Activate))
        .unwrap();
    let (updated, response) = next(&mut frames).await;
    assert_eq!(updated.revision, first.revision + 1);
    assert_eq!(
        updated.regions[0].root.kind,
        UiNodeKind::Button { text: "1".into() }
    );
    response
        .send(UiSurfaceEvent::Refresh {
            snapshot: UiSurfaceSnapshot::default(),
        })
        .unwrap();
    let (unchanged, response) = next(&mut frames).await;
    assert_eq!(unchanged.revision, updated.revision);
    response
        .send(interaction(
            unchanged.revision,
            UiInteraction::Submit { value: 1.into() },
        ))
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap(), Some(1));
}

#[test]
fn validates_complete_tree_identities_before_frontend_state_changes() {
    let region = UiRegion {
        id: "dialog".into(),
        placement: UiRegionPlacement::Overlay,
        root: UiNode::new(
            "content",
            UiNodeKind::Column {
                gap: 0,
                children: vec![
                    UiNode::text("label", "First"),
                    UiNode::new(
                        "nested",
                        UiNodeKind::Panel {
                            title: String::new(),
                            child: Box::new(UiNode::text("label", "Duplicate")),
                        },
                    ),
                ],
            },
        ),
        overlay: None,
    };
    let mut frame = UiSurfaceFrame {
        owner: "owner".into(),
        revision: 1,
        regions: vec![region],
        ..Default::default()
    };
    assert_eq!(
        frame.validate().unwrap_err(),
        "UI region dialog: duplicate UI node ID: label"
    );
    frame.regions[0].root = UiNode::text("label", "Valid");
    assert!(frame.validate().is_ok());
    frame.regions.push(frame.regions[0].clone());
    assert_eq!(
        frame.validate().unwrap_err(),
        "duplicate UI region ID: dialog"
    );
    frame.regions[1].id = "second".into();
    assert!(
        frame.validate().is_ok(),
        "node IDs are scoped to each region"
    );
    frame.regions[1].root.id.clear();
    assert_eq!(
        frame.validate().unwrap_err(),
        "UI region second: UI node ID must not be empty"
    );
}

#[tokio::test]
async fn standard_selector_uses_shared_nodes_and_returns_validated_index() {
    let (epoch, access, mut frames) = host(true);
    let ui = epoch.context().ui;
    let task = tokio::spawn(async move {
        ui.select("Environment", vec!["Test".into(), "Production".into()])
            .await
    });
    let (frame, response) = next(&mut frames).await;
    let UiNodeKind::Column { children, .. } = &frame.regions[0].root.kind else {
        panic!("selector is a shared column");
    };
    assert_eq!(
        children[0],
        UiNode::new(
            "title",
            UiNodeKind::Text {
                text: "Environment".into(),
                tone: UiTone::Accent
            }
        )
    );
    assert_eq!(
        children[1],
        UiNode::new(
            "value",
            UiNodeKind::Select {
                options: vec!["Test".into(), "Production".into()],
                initial_selected: 0
            }
        )
    );
    response
        .send(UiSurfaceEvent::Interaction {
            region_id: "dialog".into(),
            node_id: "value".into(),
            event: UiInteraction::Submit { value: 1.into() },
            snapshot: UiSurfaceSnapshot::default(),
            revision: frame.revision,
        })
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap(), Some(1));
    assert_eq!(
        *access.commands.lock().unwrap(),
        [(frame.owner, UiCommand::Surface(UiSurfaceCommand::Close))]
    );
    assert_eq!(
        epoch
            .context()
            .ui
            .select("Empty", Vec::new())
            .await
            .unwrap(),
        None
    );
    assert!(frames.try_recv().is_err());
    let (inactive, _, mut inactive_frames) = host(false);
    assert!(
        !inactive
            .context()
            .ui
            .confirm("Unavailable", "No terminal")
            .await
            .unwrap()
    );
    assert!(inactive_frames.try_recv().is_err());
}

#[tokio::test]
async fn native_text_and_multi_selection_helpers_use_shared_nodes() {
    let (epoch, _access, mut frames) = host(true);

    let ui = epoch.context().ui;
    let input = tokio::spawn(async move { ui.input("Name", "Type a name").await });
    let (frame, response) = next(&mut frames).await;
    let UiNodeKind::Column { children, .. } = &frame.regions[0].root.kind else {
        panic!("input is a shared column")
    };
    assert!(matches!(
        &children[1].kind,
        UiNodeKind::Input { placeholder, .. } if placeholder == "Type a name"
    ));
    response
        .send(UiSurfaceEvent::Interaction {
            region_id: "dialog".into(),
            node_id: "value".into(),
            event: UiInteraction::Submit {
                value: "Ada".into(),
            },
            snapshot: UiSurfaceSnapshot::default(),
            revision: frame.revision,
        })
        .unwrap();
    assert_eq!(input.await.unwrap().unwrap().as_deref(), Some("Ada"));

    let request = UiMultiSelectRequest {
        title: "Skills".into(),
        options: vec![UiMultiSelectOption {
            label: "Alpha".into(),
            search_text: "alpha".into(),
            category: None,
            detail_lines: Vec::new(),
            read_only: false,
            sort_values: Vec::new(),
        }],
        actions: vec![UiMultiSelectAction {
            id: "apply".into(),
            key: 'g',
            label: "apply".into(),
            enabled: true,
            confirmation: None,
        }],
        categories: Vec::new(),
        sort_modes: Vec::new(),
        initially_selected: Vec::new(),
        initial_query: String::new(),
        initial_active_categories: Vec::new(),
        initial_sort_mode: 0,
        summary_lines: Vec::new(),
    };
    let ui = epoch.context().ui;
    let selection = tokio::spawn(async move { ui.multi_select(request).await });
    let (frame, response) = next(&mut frames).await;
    assert!(matches!(
        &frame.regions[0].root.kind,
        UiNodeKind::MultiSelect { request } if request.title == "Skills"
    ));
    let expected = UiMultiSelectResponse {
        selected: vec![0],
        action_id: "apply".into(),
        query: String::new(),
        active_categories: Vec::new(),
        sort_mode: 0,
    };
    response
        .send(UiSurfaceEvent::Interaction {
            region_id: "multi-select-dialog".into(),
            node_id: "multi-select".into(),
            event: UiInteraction::Submit {
                value: serde_json::to_value(&expected).unwrap(),
            },
            snapshot: UiSurfaceSnapshot::default(),
            revision: frame.revision,
        })
        .unwrap();
    assert_eq!(selection.await.unwrap().unwrap(), Some(expected));
}
