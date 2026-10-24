//! One Rust UI runtime for native and JavaScript plugin descriptions.
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
};

use crossterm::event::Event;
use pi_plugin::{
    UiCommand, UiHostOperation, UiHostRequest, UiProductCommand, UiRegionPlacement,
    UiSurfaceCommand, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot,
};
use ratatui::{layout::Rect, text::Line};
use tokio::sync::oneshot;

use super::super::UiPalette;
use super::bridge::SurfaceRequest;
mod owners;
mod runtime;

pub(in crate::tui) struct ExtensionSurface {
    graphics: RefCell<super::graphics::Graphics>,
    frame: UiSurfaceFrame,
    owners: owners::SurfaceOwners,
    forwarded: VecDeque<Event>,
    expected_transcript: HashMap<usize, String>,
    transcript_revision: u64,
    snapshot: UiSurfaceSnapshot,
    palette: Cell<UiPalette>,
}

/// Product work left after the surface has consumed protocol-local state changes.
///
/// The TUI and startup adapters execute these effects; they never interpret `UiCommand`
/// directly. This keeps owner admission, retirement, focus and wake semantics inside the surface.
pub(in crate::tui) enum Effect {
    ClearOwner {
        owner: String,
    },
    HostOperation {
        owner: String,
        id: u64,
        operation: UiHostOperation,
    },
    Product {
        owner: String,
        command: UiProductCommand,
    },
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
    pub(in crate::tui) fn handle_request(
        &mut self,
        request: SurfaceRequest,
        snapshot: &UiSurfaceSnapshot,
    ) -> Option<Effect> {
        match request {
            SurfaceRequest::Exchange(frame, response) => {
                self.exchange(frame, response, snapshot);
                None
            }
            SurfaceRequest::Command(owner, UiCommand::Surface(UiSurfaceCommand::Close)) => {
                self.retire(owner.clone());
                self.refresh(snapshot);
                Some(Effect::ClearOwner { owner })
            }
            SurfaceRequest::Command(owner, _) if !self.accepts(&owner) => None,
            SurfaceRequest::Command(owner, UiCommand::Surface(command)) => match command {
                UiSurfaceCommand::Wake => {
                    self.wake(&owner, snapshot);
                    None
                }
                UiSurfaceCommand::Focus { region_id, node_id } => {
                    self.focus(&owner, &region_id, &node_id);
                    None
                }
                UiSurfaceCommand::Close => unreachable!("close handled before owner admission"),
            },
            SurfaceRequest::Command(owner, UiCommand::Host(UiHostRequest { id, operation })) => {
                Some(Effect::HostOperation {
                    owner,
                    id,
                    operation,
                })
            }
            SurfaceRequest::Command(owner, UiCommand::Product(command)) => {
                Some(Effect::Product { owner, command })
            }
        }
    }

    pub(in crate::tui) fn finish_host_operation(
        &mut self,
        owner: &str,
        id: u64,
        result: Result<Option<String>, String>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        self.reset_graphics();
        self.host_result(owner, id, result, snapshot);
    }
    pub(in crate::tui) fn hide_working(&self) -> bool {
        self.frame.hide_working
    }
    pub(in crate::tui) fn has_header(&self) -> bool {
        self.frame
            .regions
            .iter()
            .any(|region| region.placement == UiRegionPlacement::Header)
    }
    pub(in crate::tui) fn renders_transcript(&self) -> bool {
        self.frame.render_transcript
    }
    pub(in crate::tui) fn transcript_revision(&self) -> u64 {
        self.transcript_revision
    }
    pub(in crate::tui) fn pop_forwarded(&mut self) -> Option<Event> {
        self.forwarded.pop_front()
    }
    pub(in crate::tui) fn forward(&mut self, event: Event) {
        self.forwarded.push_back(event);
    }
    pub(in crate::tui) fn set_images_enabled(&self, enabled: bool) {
        self.graphics.borrow_mut().set_enabled(enabled);
    }
    pub(in crate::tui) fn begin_frame(&self) {
        self.graphics.borrow_mut().begin();
    }
    pub(in crate::tui) fn reset_graphics(&self) {
        self.graphics.borrow_mut().reset();
    }
    pub(in crate::tui) fn prepare_frame(
        &self,
        buffer: &ratatui::buffer::Buffer,
    ) -> super::graphics::Paint {
        self.graphics.borrow_mut().prepare(buffer)
    }
    #[cfg(test)]
    pub(in crate::tui) fn push_region(&mut self, region: pi_plugin::UiRegion) {
        self.frame.regions.push(region);
    }
    pub(in crate::tui) fn set_palette(&self, palette: UiPalette) {
        self.palette.set(palette);
    }
    pub(in crate::tui) fn exchange(
        &mut self,
        frame: UiSurfaceFrame,
        response: oneshot::Sender<UiSurfaceEvent>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        self.snapshot = snapshot.clone();
        self.owners.exchange(frame, response, snapshot);
        self.compose();
    }
    pub(in crate::tui) fn refresh(&mut self, snapshot: &UiSurfaceSnapshot) {
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
    pub(in crate::tui) fn wake(&mut self, owner: &str, snapshot: &UiSurfaceSnapshot) {
        self.owners.wake(owner, snapshot);
    }
    pub(in crate::tui) fn host_result(
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
    pub(in crate::tui) fn input(&mut self, event: Event, snapshot: &UiSurfaceSnapshot) -> bool {
        self.input_packet(Some(event), None, snapshot)
    }
    pub(in crate::tui) fn input_packet(
        &mut self,
        event: Option<Event>,
        _raw: Option<&str>,
        snapshot: &UiSurfaceSnapshot,
    ) -> bool {
        event.is_some_and(|event| self.owners.input(&event, snapshot))
    }
    // Local input and editing never wait for a JavaScript callback. Change events coalesce until
    // observed, while semantic submissions remain ordered in the owner's exchange queue.
    pub(in crate::tui) fn can_accept_input(&self) -> bool {
        true
    }
    pub(in crate::tui) fn focus(&mut self, owner: &str, region_id: &str, node_id: &str) {
        self.owners.focus(owner, region_id, node_id);
    }
    pub(in crate::tui) fn accepts(&self, owner: &str) -> bool {
        self.owners.accepts(owner)
    }
    pub(in crate::tui) fn retire(&mut self, owner: String) {
        self.owners.retire(owner);
        self.compose();
    }
    pub(in crate::tui) fn close(&mut self) {
        self.owners.close();
        self.frame = UiSurfaceFrame::default();
        self.expected_transcript.clear();
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
    }
    pub(in crate::tui) fn has_regions(&self) -> bool {
        !self.frame.regions.is_empty()
    }
    pub(in crate::tui) fn transcript_lines(&self, index: usize) -> Option<Vec<Line<'static>>> {
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
    pub(in crate::tui) fn record_transcript_graphics(
        &self,
        index: usize,
        area: Rect,
        scroll: usize,
    ) {
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
    pub(in crate::tui) fn height(&self, placement: UiRegionPlacement, max: u16) -> u16 {
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
    pub(in crate::tui) fn startup_areas(
        &self,
        columns: u16,
        rows: u16,
    ) -> Vec<pi_plugin::UiSurfaceArea> {
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
    pub(in crate::tui) fn draw_regions(
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
    pub(in crate::tui) fn draw_overlays(&self, frame: &mut ratatui::Frame<'_>) {
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
    use pi_plugin::{
        UiInteraction, UiMultiSelectAction, UiMultiSelectOption, UiMultiSelectRequest,
        UiMultiSelectResponse, UiNode, UiNodeKind, UiRegion,
    };
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

    fn visible_at(surface: &ExtensionSurface, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| surface.draw_overlays(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn multi_selection_uses_the_shared_surface_runtime() {
        let request = UiMultiSelectRequest {
            title: "Procedural Skills".into(),
            options: vec![
                UiMultiSelectOption {
                    label: "[G] Alpha".into(),
                    search_text: "Alpha global".into(),
                    category: Some("G".into()),
                    detail_lines: vec!["Global alpha procedure".into()],
                    read_only: false,
                    sort_values: vec![vec!["2026-01-01".into()], vec!["alpha".into()]],
                },
                UiMultiSelectOption {
                    label: "[P] Beta".into(),
                    search_text: "Beta project".into(),
                    category: Some("P".into()),
                    detail_lines: vec!["Project beta procedure".into()],
                    read_only: false,
                    sort_values: vec![vec!["2026-02-01".into()], vec!["beta".into()]],
                },
            ],
            actions: vec![UiMultiSelectAction {
                id: "delete".into(),
                key: 'd',
                label: "delete".into(),
                enabled: true,
                confirmation: Some("Delete {count} item{plural}?{read_only_note}".into()),
            }],
            categories: vec![
                ("G".into(), "Global [G]".into()),
                ("P".into(), "Project [P]".into()),
            ],
            sort_modes: vec![("Updated".into(), true), ("Name".into(), false)],
            initially_selected: Vec::new(),
            initial_query: String::new(),
            initial_active_categories: Vec::new(),
            initial_sort_mode: 0,
            summary_lines: vec!["Select items with space.".into()],
        };
        let mut surface = ExtensionSurface::default();
        let mut result = mount(
            &mut surface,
            description(
                "native-or-js",
                1,
                UiNodeKind::MultiSelect {
                    request: Box::new(request),
                },
            ),
        );
        let snapshot = UiSurfaceSnapshot::default();
        assert!(visible_at(&surface, 110, 24).contains("2 visible · 2 total · 0 selected"));
        for code in [
            KeyCode::Char('f'),
            KeyCode::Char(' '),
            KeyCode::Enter,
            KeyCode::Char('s'),
            KeyCode::Char(' '),
            KeyCode::Char('/'),
            KeyCode::Char('b'),
            KeyCode::Char('e'),
            KeyCode::Char('t'),
            KeyCode::Tab,
            KeyCode::Char('d'),
        ] {
            assert!(surface.input(key(code), &snapshot));
        }
        let rendered = visible_at(&surface, 110, 24);
        assert!(rendered.contains("1 visible · 2 total · 1 selected"));
        assert!(rendered.contains("Confirm delete: y yes"));
        assert!(surface.input(key(KeyCode::Char('y')), &snapshot));
        let UiSurfaceEvent::Interaction {
            event: UiInteraction::Submit { value },
            ..
        } = result.try_recv().unwrap()
        else {
            panic!("expected multi-selection submission")
        };
        assert_eq!(
            serde_json::from_value::<UiMultiSelectResponse>(value).unwrap(),
            UiMultiSelectResponse {
                selected: vec![1],
                action_id: "delete".into(),
                query: "bet".into(),
                active_categories: vec!["P".into()],
                sort_mode: 1,
            }
        );
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

    #[test]
    fn request_handling_keeps_protocol_state_inside_the_surface() {
        let snapshot = UiSurfaceSnapshot::default();
        let mut surface = ExtensionSurface::default();
        let frame = description("owner", 1, UiNodeKind::Button { text: "Run".into() });
        let (response, _events) = oneshot::channel();
        assert!(
            surface
                .handle_request(SurfaceRequest::Exchange(frame, response), &snapshot)
                .is_none()
        );
        assert!(surface.has_regions());

        assert!(matches!(
            surface.handle_request(
                SurfaceRequest::Command("owner".into(), UiCommand::Host(UiHostRequest {
                    id: 7,
                    operation: UiHostOperation::Clipboard { text: "copied".into() },
                })),
                &snapshot,
            ),
            Some(Effect::HostOperation { owner, id: 7, operation: UiHostOperation::Clipboard { text } })
                if owner == "owner" && text == "copied"
        ));

        assert!(matches!(
            surface.handle_request(
                SurfaceRequest::Command(
                    "owner".into(),
                    UiCommand::Product(UiProductCommand::Theme {
                        name: "dark".into(),
                    }),
                ),
                &snapshot,
            ),
            Some(Effect::Product { owner, command: UiProductCommand::Theme { name } })
                if owner == "owner" && name == "dark"
        ));
        assert!(
            surface
                .handle_request(
                    SurfaceRequest::Command(
                        "owner".into(),
                        UiCommand::Surface(UiSurfaceCommand::Wake),
                    ),
                    &snapshot,
                )
                .is_none(),
            "wake is consumed by the surface"
        );
        assert!(matches!(
            surface.handle_request(
                SurfaceRequest::Command(
                    "owner".into(),
                    UiCommand::Surface(UiSurfaceCommand::Close),
                ),
                &snapshot,
            ),
            Some(Effect::ClearOwner { owner }) if owner == "owner"
        ));
        assert!(!surface.has_regions());
        assert!(
            surface
                .handle_request(
                    SurfaceRequest::Command(
                        "owner".into(),
                        UiCommand::Product(UiProductCommand::Title {
                            title: "stale".into(),
                        }),
                    ),
                    &snapshot,
                )
                .is_none(),
            "retired owners cannot emit product effects"
        );
        assert!(
            surface
                .handle_request(
                    SurfaceRequest::Command(
                        "owner".into(),
                        UiCommand::Host(UiHostRequest {
                            id: 8,
                            operation: UiHostOperation::Suspend,
                        })
                    ),
                    &snapshot,
                )
                .is_none(),
            "retired owners cannot request physical host operations"
        );
    }
}
