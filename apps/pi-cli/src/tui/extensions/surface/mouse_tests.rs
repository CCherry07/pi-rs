use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use pi_plugin::{UiInteraction, UiMouseEvent, UiMouseEventKind, UiMouseResult, UiNode, UiRegion};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

struct Harness {
    surface: ExtensionSurface,
    frame: UiSurfaceFrame,
    receiver: oneshot::Receiver<UiSurfaceEvent>,
    snapshot: UiSurfaceSnapshot,
}

impl Harness {
    fn new(placement: UiRegionPlacement) -> Self {
        let frame = UiSurfaceFrame {
            owner: "mouse-owner".into(), revision: 1,
            regions: vec![UiRegion {
                id: "region".into(), placement,
                root: serde_json::from_value(json!({ "id": "pointer", "type": "custom", "lines": ["one", "two", "three"], "mouse": true, "instanceId": "first" })).unwrap(),
                overlay: None,
            }], ..Default::default()
        };
        let snapshot = UiSurfaceSnapshot {
            columns: 80,
            rows: 24,
            ..Default::default()
        };
        let mut surface = ExtensionSurface::default();
        let (sender, receiver) = oneshot::channel();
        surface.exchange(frame.clone(), sender, &snapshot);
        let mut value = Self {
            surface,
            frame,
            receiver,
            snapshot,
        };
        value.paint();
        value
    }
    fn paint(&mut self) {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        self.surface.begin_frame();
        terminal
            .draw(|frame| {
                self.surface.draw_regions(
                    frame,
                    UiRegionPlacement::AboveEditor,
                    Rect::new(8, 6, 25, 8),
                );
                self.surface.draw_overlays(frame);
            })
            .unwrap();
    }
    fn exchange(&mut self) {
        let (sender, receiver) = oneshot::channel();
        self.surface
            .exchange(self.frame.clone(), sender, &self.snapshot);
        self.receiver = receiver;
    }
    fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) -> bool {
        self.surface.input(
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }),
            &self.snapshot,
        )
    }
    fn request(&mut self) -> (u64, Option<u64>, UiMouseEvent) {
        let UiSurfaceEvent::Interaction {
            event: UiInteraction::CustomMouse {
                id, target, event, ..
            },
            ..
        } = self.receiver.try_recv().unwrap()
        else {
            panic!("expected mouse request")
        };
        (id, target, event)
    }
    fn reply(&mut self, id: u64, result: UiMouseResult) {
        self.surface.handle_request(
            SurfaceRequest::Command(
                self.frame.owner.clone(),
                UiCommand::Surface(UiSurfaceCommand::MouseResult { id, result }),
            ),
            &self.snapshot,
        );
    }
}

#[test]
fn mouse_only_custom_uses_painted_coordinates_and_waits_for_focus_before_typing() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    assert!(!app.surface.region_snapshots()[0].focused);
    assert!(app.mouse(MouseEventKind::Down(MouseButton::Left), 11, 7));
    let (id, _, event) = app.request();
    assert_eq!((event.x, event.y, event.width, event.height), (3, 1, 25, 3));
    assert_eq!(event.kind, UiMouseEventKind::Press);
    let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(app.surface.input(key.clone(), &app.snapshot));
    assert!(app.surface.pop_input().is_none());
    app.reply(
        id,
        UiMouseResult {
            focus: true,
            target: Some(7),
            ..Default::default()
        },
    );
    assert!(app.surface.region_snapshots()[0].focused);
    assert_eq!(app.surface.pop_input(), Some((key.clone(), false)));
    app.exchange();
    assert!(app.surface.input(key, &app.snapshot));
    assert!(
        matches!(app.receiver.try_recv().unwrap(), UiSurfaceEvent::Interaction { event: UiInteraction::CustomInput { data }, .. } if data == "x")
    );
}

#[test]
fn capture_routes_outside_bounds_and_release_without_movement_emits_click_once() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    app.mouse(MouseEventKind::Down(MouseButton::Left), 9, 6);
    let (id, _, _) = app.request();
    app.reply(
        id,
        UiMouseResult {
            capture: true,
            target: Some(9),
            ..Default::default()
        },
    );
    app.exchange();
    app.mouse(MouseEventKind::Drag(MouseButton::Left), 2, 1);
    let (id, target, event) = app.request();
    assert_eq!(target, Some(9));
    assert_eq!((event.x, event.y), (-6, -5));
    app.reply(id, UiMouseResult::default());
    app.exchange();
    app.mouse(MouseEventKind::Up(MouseButton::Left), 2, 1);
    let (id, _, _) = app.request();
    app.reply(id, UiMouseResult::default());
    assert!(app.surface.mouse_deadline().is_none());
    assert!(app.surface.pop_input().is_none());

    for count in 1..=2 {
        app.exchange();
        app.mouse(MouseEventKind::Down(MouseButton::Left), 9, 6);
        let (id, _, _) = app.request();
        app.reply(
            id,
            UiMouseResult {
                handled: true,
                target: Some(9),
                ..Default::default()
            },
        );
        app.exchange();
        app.mouse(MouseEventKind::Up(MouseButton::Left), 9, 6);
        let (id, _, _) = app.request();
        app.reply(id, UiMouseResult::default());
        app.exchange();
        let (id, target, click) = app.request();
        assert_eq!(target, Some(9));
        assert_eq!(click.kind, UiMouseEventKind::Click);
        assert_eq!(click.click_count, Some(count));
        app.reply(
            id,
            UiMouseResult {
                handled: true,
                ..Default::default()
            },
        );
        assert!(app.surface.mouse_deadline().is_none());
    }
}

#[test]
fn declined_mouse_falls_back_once_and_foreign_or_late_replies_do_nothing() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    app.mouse(MouseEventKind::ScrollDown, 10, 6);
    let (id, _, _) = app.request();
    app.surface.handle_request(
        SurfaceRequest::Command(
            "other-owner".into(),
            UiCommand::Surface(UiSurfaceCommand::MouseResult {
                id,
                result: UiMouseResult {
                    focus: true,
                    ..Default::default()
                },
            }),
        ),
        &app.snapshot,
    );
    assert!(app.surface.mouse_deadline().is_some());
    app.reply(id, UiMouseResult::default());
    assert!(matches!(
        app.surface.pop_input(),
        Some((
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            }),
            true
        ))
    ));
    app.reply(
        id,
        UiMouseResult {
            focus: true,
            ..Default::default()
        },
    );
    assert!(app.surface.pop_input().is_none());
    assert!(!app.surface.region_snapshots()[0].focused);
}

#[test]
fn replacement_hiding_retirement_and_timeout_cancel_the_gesture_without_clicking_through() {
    for action in ["replace", "hide", "retire", "timeout", "resize"] {
        let mut app = Harness::new(UiRegionPlacement::AboveEditor);
        app.mouse(MouseEventKind::Down(MouseButton::Left), 10, 6);
        let (id, _, _) = app.request();
        match action {
            "replace" => {
                if let pi_plugin::UiNodeKind::Custom { instance_id, .. } =
                    &mut app.frame.regions[0].root.kind
                {
                    *instance_id = "replacement".into();
                }
                app.frame.revision += 1;
                app.exchange();
            }
            "hide" => {
                app.frame.regions[0].overlay = Some(pi_plugin::UiOverlayOptions {
                    hidden: true,
                    ..Default::default()
                });
                app.frame.revision += 1;
                app.exchange();
            }
            "retire" => app.surface.retire(app.frame.owner.clone()),
            "timeout" => app
                .surface
                .owners
                .expire_mouse(app.surface.mouse_deadline().unwrap()),
            "resize" => {
                app.surface.input(Event::Resize(100, 30), &app.snapshot);
            }
            _ => unreachable!(),
        }
        assert!(app.surface.mouse_deadline().is_none(), "{action}");
        app.reply(
            id,
            UiMouseResult {
                focus: true,
                capture: true,
                ..Default::default()
            },
        );
        assert!(app.surface.pop_input().is_none(), "{action}");
        assert!(
            app.mouse(MouseEventKind::Up(MouseButton::Left), 10, 6),
            "{action}"
        );
        assert!(app.surface.mouse_deadline().is_none(), "{action}");
    }
}

#[test]
fn opaque_overlay_prevents_mouse_dispatch_to_a_lower_component() {
    let mut app = Harness::new(UiRegionPlacement::Overlay);
    let lower = app.surface.region_snapshots()[0].bounds.unwrap();
    let mut overlay = app.frame.regions[0].clone();
    overlay.id = "top".into();
    overlay.root = UiNode::text("label", "plain overlay");
    app.frame.regions.push(overlay);
    app.frame.revision += 1;
    app.exchange();
    app.paint();
    let top = app
        .surface
        .region_snapshots()
        .into_iter()
        .find(|region| region.region_id == "top")
        .unwrap()
        .bounds
        .unwrap();
    assert!(lower.row <= top.row);
    assert!(app.mouse(MouseEventKind::Down(MouseButton::Left), top.col, top.row));
    assert!(app.surface.mouse_deadline().is_none());
}

#[test]
fn mouse_coordinates_include_scroll_offset_but_hit_only_visible_cells() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    app.frame.regions[0].root = serde_json::from_value(json!({
        "id": "panel", "type": "panel", "title": "scroll",
        "child": { "id": "scroll", "type": "scroll", "height": 2,
            "child": { "id": "body", "type": "column", "children": [
                { "id": "prefix", "type": "text", "text": "prefix" },
                { "id": "pointer", "type": "custom", "lines": ["a", "b", "c", "d", "e"], "mouse": true, "instanceId": "first" }
            ] }
        }
    })).unwrap();
    app.frame.revision += 1;
    app.exchange();
    app.paint();
    app.surface.focus(&app.frame.owner, "region", "scroll");
    for _ in 0..2 {
        assert!(app.surface.input(
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            &app.snapshot
        ));
    }
    app.paint();
    assert!(app.mouse(MouseEventKind::Down(MouseButton::Left), 10, 8));
    let (id, _, event) = app.request();
    assert_eq!((event.x, event.y, event.width, event.height), (0, 1, 21, 5));
    app.reply(id, UiMouseResult::default());
    assert!(app.surface.pop_input().is_some());
    // The panel border cannot address the custom content hidden above the viewport.
    assert!(!app.mouse(MouseEventKind::Down(MouseButton::Left), 10, 7));
    assert!(app.surface.mouse_deadline().is_none());
}

#[test]
fn late_mouse_focus_cannot_raise_a_component_over_a_new_modal() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    app.mouse(MouseEventKind::Down(MouseButton::Left), 10, 6);
    let (id, _, _) = app.request();
    app.frame.regions.push(UiRegion {
        id: "new-modal".into(),
        placement: UiRegionPlacement::Overlay,
        root: UiNode::text("modal", "new modal"),
        overlay: None,
    });
    app.frame.revision += 1;
    app.exchange();
    app.paint();
    app.reply(
        id,
        UiMouseResult {
            focus: true,
            capture: true,
            ..Default::default()
        },
    );
    assert!(
        app.surface
            .region_snapshots()
            .iter()
            .any(|region| region.region_id == "new-modal" && region.focused)
    );
    assert!(app.surface.mouse_deadline().is_none());
    assert!(app.surface.pop_input().is_none());
}

#[test]
fn pressing_a_setting_may_reflow_its_description_without_losing_release_and_click() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    app.mouse(MouseEventKind::Down(MouseButton::Left), 10, 6);
    let (id, _, _) = app.request();
    app.reply(
        id,
        UiMouseResult {
            handled: true,
            target: Some(3),
            ..Default::default()
        },
    );
    if let pi_plugin::UiNodeKind::Custom { lines, .. } = &mut app.frame.regions[0].root.kind {
        lines.push("the selected setting has a longer description".into());
    }
    app.frame.revision += 1;
    app.exchange();
    app.paint();
    app.mouse(MouseEventKind::Up(MouseButton::Left), 10, 6);
    let (id, target, event) = app.request();
    assert_eq!(event.kind, UiMouseEventKind::Release);
    assert_eq!(target, Some(3));
    app.reply(id, UiMouseResult::default());
    app.exchange();
    let (_, target, event) = app.request();
    assert_eq!(event.kind, UiMouseEventKind::Click);
    assert_eq!(target, Some(3));
}

#[test]
fn cancelling_a_press_does_not_disable_independent_wheel_input() {
    let mut app = Harness::new(UiRegionPlacement::AboveEditor);
    app.mouse(MouseEventKind::Down(MouseButton::Left), 10, 6);
    app.request();
    app.surface.input(Event::FocusLost, &app.snapshot);
    app.exchange();
    // The button may have been released outside the terminal, with no release report.
    app.mouse(MouseEventKind::ScrollDown, 10, 6);
    let (_, _, event) = app.request();
    assert_eq!(event.kind, UiMouseEventKind::Wheel);
}
