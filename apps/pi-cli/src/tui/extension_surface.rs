//! One Rust UI runtime for native and JavaScript plugin descriptions.
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
};

use crossterm::event::Event;
use pi_plugin::{UiRegionPlacement, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot};
use ratatui::{layout::Rect, text::Line};
use tokio::sync::oneshot;

use super::UiPalette;
mod owners;
mod runtime;

pub(super) struct ExtensionSurface {
    pub(super) graphics: RefCell<super::extension_graphics::Graphics>,
    pub(super) frame: UiSurfaceFrame,
    owners: owners::SurfaceOwners,
    pub(super) forwarded: VecDeque<Event>,
    expected_transcript: HashMap<usize, String>,
    pub(super) transcript_revision: u64,
    snapshot: UiSurfaceSnapshot,
    palette: Cell<UiPalette>,
}

impl Default for ExtensionSurface {
    fn default() -> Self {
        Self {
            graphics: RefCell::default(),
            frame: UiSurfaceFrame::default(),
            owners: owners::SurfaceOwners::default(),
            forwarded: VecDeque::new(),
            expected_transcript: HashMap::new(),
            transcript_revision: 0,
            snapshot: UiSurfaceSnapshot::default(),
            palette: Cell::new(UiPalette::from_background(None)),
        }
    }
}

impl ExtensionSurface {
    pub(super) fn set_palette(&self, palette: UiPalette) {
        self.palette.set(palette);
    }
    pub(super) fn exchange(
        &mut self,
        frame: UiSurfaceFrame,
        response: oneshot::Sender<UiSurfaceEvent>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        self.snapshot = snapshot.clone();
        self.owners.exchange(frame, response, snapshot);
        self.compose();
    }
    pub(super) fn refresh(&mut self, snapshot: &UiSurfaceSnapshot) {
        let expected: HashMap<_, _> = snapshot
            .transcript
            .iter()
            .map(|item| (item.index, item.fingerprint.clone()))
            .collect();
        if expected != self.expected_transcript
            || snapshot.transcript_width != self.snapshot.transcript_width
        {
            self.expected_transcript = expected;
            self.transcript_revision = self.transcript_revision.wrapping_add(1);
        }
        self.snapshot = snapshot.clone();
        self.owners.refresh(snapshot);
    }
    pub(super) fn wake(&mut self, owner: &str, snapshot: &UiSurfaceSnapshot) {
        self.owners.wake(owner, snapshot);
    }
    pub(super) fn host_result(
        &mut self,
        owner: &str,
        id: u64,
        result: Result<Option<String>, String>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        self.owners.host_result(owner, id, result, snapshot);
    }
    fn compose(&mut self) {
        let frame = self.owners.compose();
        if self.frame.transcript != frame.transcript {
            self.transcript_revision = self.transcript_revision.wrapping_add(1);
        }
        self.frame = frame;
    }
    #[cfg(test)]
    pub(super) fn input(&mut self, event: Event, snapshot: &UiSurfaceSnapshot) -> bool {
        self.input_packet(Some(event), None, snapshot)
    }
    pub(super) fn input_packet(
        &mut self,
        event: Option<Event>,
        _raw: Option<&str>,
        snapshot: &UiSurfaceSnapshot,
    ) -> bool {
        event.is_some_and(|event| self.owners.input(&event, snapshot))
    }
    // Local input and editing never wait for a JavaScript callback. Change events coalesce until
    // observed, while semantic submissions remain ordered in the owner's exchange queue.
    pub(super) fn can_accept_input(&self) -> bool {
        true
    }
    pub(super) fn focus(&mut self, owner: &str, region_id: &str, node_id: &str) {
        self.owners.focus(owner, region_id, node_id);
    }
    pub(super) fn accepts(&self, owner: &str) -> bool {
        self.owners.accepts(owner)
    }
    pub(super) fn retire(&mut self, owner: String) {
        self.owners.retire(owner);
        self.compose();
    }
    pub(super) fn close(&mut self) {
        self.owners.close();
        self.frame = UiSurfaceFrame::default();
        self.expected_transcript.clear();
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
    }
    pub(super) fn has_regions(&self) -> bool {
        !self.frame.regions.is_empty()
    }
    pub(super) fn transcript_lines(&self, index: usize) -> Option<Vec<Line<'static>>> {
        self.frame
            .transcript
            .iter()
            .find(|item| {
                item.index == index
                    && self.expected_transcript.get(&index) == Some(&item.fingerprint)
            })
            .map(|item| {
                runtime::render_lines(
                    &item.root,
                    self.snapshot.transcript_width.max(1),
                    self.palette.get(),
                )
            })
    }
    pub(super) fn record_transcript_graphics(&self, index: usize, area: Rect, scroll: usize) {
        if let Some(item) = self.frame.transcript.iter().find(|item| {
            item.index == index && self.expected_transcript.get(&index) == Some(&item.fingerprint)
        }) {
            runtime::visit_paint_nodes(
                &item.root,
                area.width,
                self.palette.get(),
                &mut |node, image| {
                    let scroll = scroll.min(usize::from(u16::MAX)) as u16;
                    if image.y < scroll {
                        return;
                    }
                    let target = Rect::new(
                        area.x.saturating_add(image.x),
                        area.y.saturating_add(image.y - scroll),
                        image.width,
                        image.height,
                    );
                    if target.intersection(area) == target {
                        runtime::paint_graphics(node, target, &mut self.graphics.borrow_mut());
                    }
                },
            );
        }
    }
    pub(super) fn height(&self, placement: UiRegionPlacement, max: u16) -> u16 {
        self.frame
            .regions
            .iter()
            .filter(|region| region.placement == placement)
            .map(|region| {
                usize::from(runtime::measure(
                    &region.root,
                    self.snapshot.columns,
                    self.palette.get(),
                ))
            })
            .sum::<usize>()
            .min(usize::from(max)) as u16
    }
    pub(super) fn startup_areas(&self, columns: u16, rows: u16) -> Vec<pi_plugin::UiSurfaceArea> {
        use UiRegionPlacement as P;
        let mut row = 0;
        [
            P::Header,
            P::AboveEditor,
            P::Editor,
            P::BelowEditor,
            P::Footer,
        ]
        .into_iter()
        .map(|placement| {
            let height = self
                .frame
                .regions
                .iter()
                .filter(|region| region.placement == placement)
                .map(|region| {
                    usize::from(runtime::measure(&region.root, columns, self.palette.get()))
                })
                .sum::<usize>()
                .min(usize::from(rows.saturating_sub(row))) as u16;
            let area = pi_plugin::UiSurfaceArea {
                placement,
                row,
                column: 0,
                width: columns,
                height,
            };
            row += height;
            area
        })
        .collect()
    }
    pub(super) fn draw_regions(
        &self,
        frame: &mut ratatui::Frame<'_>,
        placement: UiRegionPlacement,
        area: Rect,
    ) {
        self.owners.draw_regions(
            frame,
            placement,
            area,
            self.palette.get(),
            &mut self.graphics.borrow_mut(),
        );
    }
    pub(super) fn draw_overlays(&self, frame: &mut ratatui::Frame<'_>) {
        self.owners
            .draw_overlays(frame, self.palette.get(), &mut self.graphics.borrow_mut());
    }
}

impl Drop for ExtensionSurface {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use pi_plugin::{UiInteraction, UiNode, UiNodeKind, UiRegion};
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn description(owner: &str, revision: u64, kind: UiNodeKind) -> UiSurfaceFrame {
        UiSurfaceFrame {
            owner: owner.into(),
            revision,
            regions: vec![UiRegion {
                id: "dialog".into(),
                placement: UiRegionPlacement::Overlay,
                root: UiNode {
                    id: "control".into(),
                    kind,
                },
                overlay: None,
            }],
            ..Default::default()
        }
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn mount(
        surface: &mut ExtensionSurface,
        frame: UiSurfaceFrame,
    ) -> oneshot::Receiver<UiSurfaceEvent> {
        let (sender, receiver) = oneshot::channel();
        surface.exchange(frame, sender, &UiSurfaceSnapshot::default());
        receiver
    }
    fn visible(surface: &ExtensionSurface) -> String {
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();
        terminal.draw(|frame| surface.draw_overlays(frame)).unwrap();
        use unicode_width::UnicodeWidthStr;
        let buffer = terminal.backend().buffer();
        let mut result = String::new();
        for row in 0..8 {
            let mut column = 0;
            while column < 20 {
                let symbol = buffer[(column, row)].symbol();
                result.push_str(symbol);
                column += symbol.width().max(1) as u16;
            }
        }
        result
    }

    #[test]
    fn native_and_serialized_descriptions_have_identical_rust_rendering() {
        let frame = description(
            "native",
            1,
            UiNodeKind::Column {
                gap: 1,
                children: vec![
                    UiNode {
                        id: "heading".into(),
                        kind: UiNodeKind::Text {
                            text: "选择环境".into(),
                            tone: pi_plugin::UiTone::Accent,
                        },
                    },
                    UiNode {
                        id: "choice".into(),
                        kind: UiNodeKind::Select {
                            options: vec!["测试".into(), "生产".into()],
                            initial_selected: 0,
                        },
                    },
                ],
            },
        );
        let serialized = serde_json::to_string(&frame).unwrap();
        let mut native = ExtensionSurface::default();
        let _native = mount(&mut native, frame);
        let mut js = ExtensionSurface::default();
        let _js = mount(&mut js, serde_json::from_str(&serialized).unwrap());
        assert_eq!(visible(&native), visible(&js));
        native.input(key(KeyCode::Down), &UiSurfaceSnapshot::default());
        js.input(key(KeyCode::Down), &UiSurfaceSnapshot::default());
        assert_eq!(visible(&native), visible(&js));
    }

    #[test]
    fn local_editing_continues_without_plugin_round_trips() {
        let mut surface = ExtensionSurface::default();
        let frame = description(
            "js",
            1,
            UiNodeKind::Input {
                initial_value: String::new(),
                placeholder: "type".into(),
                completions: Vec::new(),
            },
        );
        let mut first = mount(&mut surface, frame.clone());
        let snapshot = UiSurfaceSnapshot::default();
        surface.input(Event::Paste("中文".into()), &snapshot);
        assert!(
            matches!(first.try_recv().unwrap(), UiSurfaceEvent::Interaction { event: UiInteraction::Change { value }, .. } if value == json!("中文"))
        );
        surface.input(key(KeyCode::Char('a')), &snapshot);
        surface.input(key(KeyCode::Char('b')), &snapshot);
        surface.input(key(KeyCode::Left), &snapshot);
        surface.input(key(KeyCode::Backspace), &snapshot);
        assert!(visible(&surface).contains("中文b"));
        let mut next = mount(&mut surface, frame.clone());
        assert!(
            matches!(next.try_recv().unwrap(), UiSurfaceEvent::Interaction { event: UiInteraction::Change { value }, .. } if value == json!("中文b"))
        );
        surface.input(key(KeyCode::Enter), &snapshot);
        let mut next = mount(&mut surface, frame);
        assert!(
            matches!(next.try_recv().unwrap(), UiSurfaceEvent::Interaction { event: UiInteraction::Submit { value }, .. } if value == json!("中文b"))
        );
    }

    #[test]
    fn stale_updates_and_retired_generations_cannot_replace_local_ui() {
        let mut surface = ExtensionSurface::default();
        let frame = description(
            "old",
            2,
            UiNodeKind::Input {
                initial_value: "new".into(),
                placeholder: String::new(),
                completions: Vec::new(),
            },
        );
        let _pending = mount(&mut surface, frame);
        let mut stale = mount(
            &mut surface,
            description(
                "old",
                1,
                UiNodeKind::Input {
                    initial_value: "stale".into(),
                    placeholder: String::new(),
                    completions: Vec::new(),
                },
            ),
        );
        assert!(matches!(
            stale.try_recv().unwrap(),
            UiSurfaceEvent::Refresh { .. }
        ));
        assert!(visible(&surface).contains("new"));
        surface.retire("old".into());
        let mut closed = mount(
            &mut surface,
            description("old", 3, UiNodeKind::Button { text: "bad".into() }),
        );
        assert_eq!(closed.try_recv().unwrap(), UiSurfaceEvent::Closed);
        assert!(!surface.has_regions());
    }

    #[test]
    fn tab_focus_and_selection_emit_semantic_values() {
        let mut surface = ExtensionSurface::default();
        let frame = description(
            "test",
            1,
            UiNodeKind::Column {
                gap: 1,
                children: vec![
                    UiNode {
                        id: "choice".into(),
                        kind: UiNodeKind::Select {
                            options: vec!["A".into(), "B".into()],
                            initial_selected: 0,
                        },
                    },
                    UiNode {
                        id: "button".into(),
                        kind: UiNodeKind::Button { text: "OK".into() },
                    },
                ],
            },
        );
        let mut response = mount(&mut surface, frame.clone());
        let snapshot = UiSurfaceSnapshot::default();
        surface.input(key(KeyCode::Down), &snapshot);
        assert!(
            matches!(response.try_recv().unwrap(), UiSurfaceEvent::Interaction { node_id, event: UiInteraction::Change { value }, .. } if node_id == "choice" && value == json!(1))
        );
        surface.input(key(KeyCode::Tab), &snapshot);
        surface.input(key(KeyCode::Enter), &snapshot);
        let mut response = mount(&mut surface, frame);
        assert!(
            matches!(response.try_recv().unwrap(), UiSurfaceEvent::Interaction { node_id, event: UiInteraction::Activate, .. } if node_id == "button")
        );
    }

    #[test]
    fn queued_submission_survives_a_new_revision_of_the_same_component() {
        let mut surface = ExtensionSurface::default();
        let mut frame = description(
            "busy",
            1,
            UiNodeKind::Input {
                initial_value: String::new(),
                placeholder: "before".into(),
                completions: Vec::new(),
            },
        );
        let mut first = mount(&mut surface, frame.clone());
        surface.input(key(KeyCode::Char('a')), &UiSurfaceSnapshot::default());
        assert!(matches!(
            first.try_recv().unwrap(),
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Change { .. },
                ..
            }
        ));
        surface.input(key(KeyCode::Enter), &UiSurfaceSnapshot::default());
        frame.revision = 2;
        frame.regions[0].root.kind = UiNodeKind::Input {
            initial_value: "reset is ignored".into(),
            placeholder: "after".into(),
            completions: Vec::new(),
        };
        let mut response = mount(&mut surface, frame);
        assert!(
            matches!(response.try_recv().unwrap(), UiSurfaceEvent::Interaction { revision: 2, event: UiInteraction::Submit { value }, .. } if value == json!("a"))
        );
        assert!(visible(&surface).contains('a'));
    }

    #[test]
    fn completions_and_explicit_focus_are_owned_by_rust() {
        let mut surface = ExtensionSurface::default();
        let frame = description(
            "complete",
            1,
            UiNodeKind::Column {
                gap: 1,
                children: vec![
                    UiNode::new(
                        "name",
                        UiNodeKind::Input {
                            initial_value: "pr".into(),
                            placeholder: String::new(),
                            completions: vec![
                                pi_plugin::UiCompletion {
                                    value: "production".into(),
                                    label: "Production".into(),
                                    description: None,
                                },
                                pi_plugin::UiCompletion {
                                    value: "preview".into(),
                                    label: "Preview".into(),
                                    description: None,
                                },
                            ],
                        },
                    ),
                    UiNode::new(
                        "submit",
                        UiNodeKind::Button {
                            text: "Apply".into(),
                        },
                    ),
                ],
            },
        );
        let mut response = mount(&mut surface, frame.clone());
        surface.input(key(KeyCode::Down), &UiSurfaceSnapshot::default());
        surface.input(key(KeyCode::Tab), &UiSurfaceSnapshot::default());
        assert!(
            matches!(response.try_recv().unwrap(), UiSurfaceEvent::Interaction { node_id, event: UiInteraction::Change { value }, .. } if node_id == "name" && value == json!("preview"))
        );
        surface.focus("complete", "dialog", "submit");
        surface.input(key(KeyCode::Enter), &UiSurfaceSnapshot::default());
        let mut response = mount(&mut surface, frame);
        assert!(
            matches!(response.try_recv().unwrap(), UiSurfaceEvent::Interaction { node_id, event: UiInteraction::Activate, .. } if node_id == "submit")
        );
    }

    #[test]
    fn scroll_containers_keep_offsets_in_rust_without_plugin_events() {
        let mut surface = ExtensionSurface::default();
        let mut frame = description(
            "scroll",
            1,
            UiNodeKind::Scroll {
                height: 3,
                child: Box::new(UiNode::text(
                    "content",
                    (0..10)
                        .map(|index| format!("line{index}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )),
            },
        );
        let mut response = mount(&mut surface, frame.clone());
        assert!(visible(&surface).contains("line0"));
        assert!(!visible(&surface).contains("line3"));
        surface.input(key(KeyCode::PageDown), &UiSurfaceSnapshot::default());
        assert!(!visible(&surface).contains("line0"));
        assert!(visible(&surface).contains("line3"));
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        frame.revision = 2;
        let _response = mount(&mut surface, frame);
        assert!(visible(&surface).contains("line3"));
        surface.input(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollDown,
                column: 3,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }),
            &UiSurfaceSnapshot::default(),
        );
        assert!(visible(&surface).contains("line6"));
        surface.input(key(KeyCode::Home), &UiSurfaceSnapshot::default());
        assert!(visible(&surface).contains("line0"));
    }

    #[test]
    fn focus_requested_before_mount_targets_the_new_component() {
        let mut surface = ExtensionSurface::default();
        surface.focus("early", "dialog", "second");
        let frame = description(
            "early",
            1,
            UiNodeKind::Column {
                gap: 0,
                children: vec![
                    UiNode::new(
                        "first",
                        UiNodeKind::Button {
                            text: "First".into(),
                        },
                    ),
                    UiNode::new(
                        "second",
                        UiNodeKind::Button {
                            text: "Second".into(),
                        },
                    ),
                ],
            },
        );
        let mut response = mount(&mut surface, frame);
        surface.input(key(KeyCode::Enter), &UiSurfaceSnapshot::default());
        assert!(
            matches!(response.try_recv().unwrap(), UiSurfaceEvent::Interaction { node_id, event: UiInteraction::Activate, .. } if node_id == "second")
        );
    }
}
