//! Generation-owned descriptions, semantic events, and Rust component state.
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet, VecDeque},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use pi_plugin::{
    UiInteraction, UiRegion, UiRegionPlacement, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Clear,
};
use tokio::sync::oneshot;

use super::super::super::UiPalette;
use super::super::graphics::Graphics;
use super::runtime::{self, TreeRuntime};

struct RegionRuntime {
    id: String,
    tree: RefCell<TreeRuntime>,
}
struct Owner {
    frame: UiSurfaceFrame,
    admitted: bool,
    regions: Vec<RegionRuntime>,
    focused_region: Option<String>,
    requested_focus: Option<(String, String)>,
    pending: Option<oneshot::Sender<UiSurfaceEvent>>,
    events: VecDeque<UiSurfaceEvent>,
}

#[derive(Default)]
pub(super) struct SurfaceOwners {
    owners: Vec<Owner>,
    retired: HashSet<String>,
}

fn exclusive(placement: UiRegionPlacement) -> bool {
    matches!(
        placement,
        UiRegionPlacement::Header | UiRegionPlacement::Editor | UiRegionPlacement::Footer
    )
}

impl SurfaceOwners {
    fn position(&self, owner: &str) -> Option<usize> {
        self.owners
            .iter()
            .position(|entry| entry.frame.owner == owner)
    }
    fn ensure_owner(&mut self, owner: &str) -> usize {
        self.position(owner).unwrap_or_else(|| {
            self.owners.push(Owner {
                frame: UiSurfaceFrame {
                    owner: owner.into(),
                    ..Default::default()
                },
                admitted: false,
                regions: Vec::new(),
                focused_region: None,
                requested_focus: None,
                pending: None,
                events: VecDeque::new(),
            });
            self.owners.len() - 1
        })
    }
    fn visible(&self, index: usize, placement: UiRegionPlacement) -> bool {
        !exclusive(placement)
            || !self.owners[index + 1..].iter().any(|owner| {
                owner
                    .frame
                    .regions
                    .iter()
                    .any(|region| region.placement == placement)
            })
    }
    pub(super) fn compose(&self) -> UiSurfaceFrame {
        let mut frame = UiSurfaceFrame::default();
        let mut transcript = BTreeMap::new();
        for (index, owner) in self.owners.iter().enumerate() {
            for region in &owner.frame.regions {
                if self.visible(index, region.placement) {
                    frame.regions.push(region.clone());
                }
            }
            frame.hide_working |= owner.frame.hide_working;
            frame.render_transcript |= owner.frame.render_transcript;
            for item in &owner.frame.transcript {
                transcript.insert(item.index, item.clone());
            }
        }
        frame.transcript = transcript.into_values().collect();
        frame
    }
    pub(super) fn exchange(
        &mut self,
        frame: UiSurfaceFrame,
        response: oneshot::Sender<UiSurfaceEvent>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        if !self.accepts(&frame.owner) {
            let _ = response.send(UiSurfaceEvent::Closed);
            return;
        }
        let index = self.ensure_owner(&frame.owner);
        if self.owners[index].admitted
            && (frame.revision < self.owners[index].frame.revision
                || (frame.revision == self.owners[index].frame.revision
                    && frame != self.owners[index].frame))
        {
            let _ = response.send(UiSurfaceEvent::Refresh {
                snapshot: snapshot.clone(),
            });
            return;
        }
        let entry = &mut self.owners[index];
        let previous_overlays = entry
            .frame
            .regions
            .iter()
            .filter(|region| region.placement == UiRegionPlacement::Overlay)
            .count();
        let overlays = frame
            .regions
            .iter()
            .filter(|region| region.placement == UiRegionPlacement::Overlay)
            .count();
        entry
            .regions
            .retain(|state| frame.regions.iter().any(|region| region.id == state.id));
        for region in &frame.regions {
            let state =
                if let Some(index) = entry.regions.iter().position(|state| state.id == region.id) {
                    &mut entry.regions[index]
                } else {
                    entry.regions.push(RegionRuntime {
                        id: region.id.clone(),
                        tree: RefCell::new(TreeRuntime::default()),
                    });
                    entry.regions.last_mut().expect("region inserted")
                };
            state.tree.get_mut().reconcile(&region.root);
        }
        entry.events.retain_mut(|event| {
            let UiSurfaceEvent::Interaction { region_id, node_id, revision, event, .. } = event else { return true; };
            if let UiInteraction::Shortcut { key } = event {
                if !frame.shortcuts.iter().any(|shortcut| &shortcut.key == key) { return false; }
            } else {
                let old = entry.frame.regions.iter().find(|region| region.id == *region_id).and_then(|region| runtime::find_node(&region.root, node_id));
                let next = frame.regions.iter().find(|region| region.id == *region_id).and_then(|region| runtime::find_node(&region.root, node_id));
                if !matches!((old, next), (Some(old), Some(next)) if std::mem::discriminant(&old.kind) == std::mem::discriminant(&next.kind)) { return false; }
            }
            *revision = frame.revision;
            true
        });
        entry.admitted = true;
        entry.frame = frame;
        if overlays > previous_overlays
            || entry
                .focused_region
                .as_ref()
                .is_none_or(|id| !entry.regions.iter().any(|region| &region.id == id))
        {
            entry.focused_region = entry
                .regions
                .iter()
                .rev()
                .find(|state| state.tree.borrow().interactive())
                .map(|state| state.id.clone());
        }
        if let Some((region_id, node_id)) = entry.requested_focus.clone()
            && let Some(state) = entry.regions.iter_mut().find(|state| state.id == region_id)
            && state.tree.get_mut().focus(&node_id)
        {
            entry.focused_region = Some(region_id);
            entry.requested_focus = None;
        }
        if let Some(old) = entry.pending.replace(response) {
            let _ = old.send(UiSurfaceEvent::Closed);
        }
        let index = if overlays > previous_overlays {
            let entry = self.owners.remove(index);
            self.owners.push(entry);
            self.owners.len() - 1
        } else {
            index
        };
        self.flush(index);
    }
    pub(super) fn refresh(&mut self, snapshot: &UiSurfaceSnapshot) {
        for index in 0..self.owners.len() {
            self.refresh_owner(index, snapshot);
        }
    }
    fn refresh_owner(&mut self, index: usize, snapshot: &UiSurfaceSnapshot) {
        let owner = &mut self.owners[index];
        owner
            .events
            .retain(|event| !matches!(event, UiSurfaceEvent::Refresh { .. }));
        owner.events.push_back(UiSurfaceEvent::Refresh {
            snapshot: snapshot.clone(),
        });
        self.flush(index);
    }
    pub(super) fn wake(&mut self, owner: &str, snapshot: &UiSurfaceSnapshot) {
        if self.accepts(owner) {
            let index = self.ensure_owner(owner);
            self.refresh_owner(index, snapshot);
        }
    }
    pub(super) fn host_result(
        &mut self,
        owner: &str,
        id: u64,
        result: Result<Option<String>, String>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        if !self.accepts(owner) {
            return;
        }
        let index = self.ensure_owner(owner);
        let (result, error) = match result {
            Ok(value) => (value, None),
            Err(error) => (None, Some(error)),
        };
        self.owners[index]
            .events
            .push_back(UiSurfaceEvent::HostResult {
                id,
                result,
                error,
                snapshot: snapshot.clone(),
            });
        self.flush(index);
    }
    fn flush(&mut self, index: usize) {
        let entry = &mut self.owners[index];
        if !entry.events.is_empty()
            && let Some(sender) = entry.pending.take()
        {
            let event = entry.events.pop_front().expect("event checked");
            if let Err(event) = sender.send(event) {
                entry.events.push_front(event);
            }
        }
    }
    fn emit(
        &mut self,
        index: usize,
        region_id: String,
        node_id: String,
        event: UiInteraction,
        snapshot: &UiSurfaceSnapshot,
    ) {
        let entry = &mut self.owners[index];
        // Local typing remains responsive while a plugin is busy. Only the latest unobserved value
        // is useful; submission/cancellation retain their exact position in the event queue.
        if matches!(event, UiInteraction::Change { .. })
            && let Some(UiSurfaceEvent::Interaction {
                region_id: old_region,
                node_id: old_node,
                event: old_event,
                ..
            }) = entry.events.back_mut()
            && *old_region == region_id
            && *old_node == node_id
            && matches!(old_event, UiInteraction::Change { .. })
        {
            *old_event = event;
        } else {
            entry.events.push_back(UiSurfaceEvent::Interaction {
                region_id,
                node_id,
                event,
                snapshot: snapshot.clone(),
                revision: entry.frame.revision,
            });
        }
        self.flush(index);
    }
    pub(super) fn input(&mut self, event: &Event, snapshot: &UiSurfaceSnapshot) -> bool {
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
        {
            for index in (0..self.owners.len()).rev() {
                if let Some(shortcut) = self.owners[index]
                    .frame
                    .shortcuts
                    .iter()
                    .find(|shortcut| shortcut_matches(&shortcut.key, *key))
                {
                    let key = shortcut.key.clone();
                    self.emit(
                        index,
                        String::new(),
                        String::new(),
                        UiInteraction::Shortcut { key },
                        snapshot,
                    );
                    return true;
                }
            }
        }
        for index in (0..self.owners.len()).rev() {
            for region_index in (0..self.owners[index].frame.regions.len()).rev() {
                let region = &self.owners[index].frame.regions[region_index];
                if !self.visible(index, region.placement) {
                    continue;
                }
                let modal = region.placement == UiRegionPlacement::Overlay
                    && region.overlay.as_ref().is_none_or(|options| options.modal);
                let entry = &self.owners[index];
                let tree = entry
                    .regions
                    .iter()
                    .find(|state| state.id == region.id)
                    .expect("mounted region");
                if !modal
                    && entry
                        .focused_region
                        .as_ref()
                        .is_some_and(|focused| focused != &region.id)
                    && !matches!(event, Event::Mouse(_))
                {
                    continue;
                }
                if !modal && !tree.tree.borrow().interactive() {
                    continue;
                }
                let (handled, interaction) = tree.tree.borrow_mut().input(&region.root, event);
                let region_id = region.id.clone();
                if handled && matches!(event, Event::Mouse(_)) {
                    self.owners[index].focused_region = Some(region_id.clone());
                }
                if let Some((node_id, interaction)) = interaction {
                    self.emit(index, region_id, node_id, interaction, snapshot);
                }
                if handled || modal {
                    return true;
                }
            }
        }
        false
    }
    pub(super) fn focus(&mut self, owner: &str, region_id: &str, node_id: &str) -> bool {
        if !self.accepts(owner) {
            return false;
        }
        let index = self.ensure_owner(owner);
        let entry = &mut self.owners[index];
        entry.requested_focus = Some((region_id.into(), node_id.into()));
        if let Some(state) = entry.regions.iter_mut().find(|state| state.id == region_id)
            && state.tree.get_mut().focus(node_id)
        {
            entry.focused_region = Some(region_id.into());
            entry.requested_focus = None;
            true
        } else {
            false
        }
    }

    pub(super) fn accepts(&self, owner: &str) -> bool {
        !self.retired.contains(owner)
    }
    pub(super) fn retire(&mut self, owner: String) {
        self.retired.insert(owner.clone());
        if let Some(index) = self.position(&owner)
            && let Some(sender) = self.owners.remove(index).pending
        {
            let _ = sender.send(UiSurfaceEvent::Closed);
        }
    }
    pub(super) fn close(&mut self) {
        for owner in self.owners.drain(..) {
            self.retired.insert(owner.frame.owner);
            if let Some(sender) = owner.pending {
                let _ = sender.send(UiSurfaceEvent::Closed);
            }
        }
    }
    pub(super) fn draw_regions(
        &self,
        frame: &mut ratatui::Frame<'_>,
        placement: UiRegionPlacement,
        mut area: Rect,
        palette: UiPalette,
        graphics: &mut Graphics,
    ) {
        for (index, owner) in self.owners.iter().enumerate() {
            if !self.visible(index, placement) {
                continue;
            }
            for region in owner
                .frame
                .regions
                .iter()
                .filter(|region| region.placement == placement)
            {
                let height = runtime::measure(&region.root, area.width, palette).min(area.height);
                self.draw_region(
                    index,
                    region,
                    frame,
                    Rect::new(area.x, area.y, area.width, height),
                    palette,
                    graphics,
                );
                area.y += height;
                area.height -= height;
            }
        }
    }
    pub(super) fn draw_overlays(
        &self,
        frame: &mut ratatui::Frame<'_>,
        palette: UiPalette,
        graphics: &mut Graphics,
    ) {
        let root = frame.area();
        for (index, owner) in self.owners.iter().enumerate() {
            for region in owner
                .frame
                .regions
                .iter()
                .filter(|region| region.placement == UiRegionPlacement::Overlay)
            {
                if region.overlay.as_ref().is_none_or(|options| options.modal) {
                    let backdrop = Style::default()
                        .fg(Color::DarkGray)
                        .bg(Color::Reset)
                        .remove_modifier(Modifier::all())
                        .add_modifier(Modifier::DIM);
                    for cell in &mut frame.buffer_mut().content {
                        cell.set_style(backdrop);
                    }
                    graphics.occlude(root);
                }
                let width = region
                    .overlay
                    .as_ref()
                    .and_then(|options| options.width)
                    .unwrap_or(root.width.saturating_sub(4).max(1))
                    .min(root.width);
                let height = runtime::measure(&region.root, width, palette)
                    .min(
                        region
                            .overlay
                            .as_ref()
                            .and_then(|options| options.max_height)
                            .unwrap_or(root.height),
                    )
                    .min(root.height);
                let area = Rect::new(
                    root.x + (root.width - width) / 2,
                    root.y + (root.height - height) / 2,
                    width,
                    height,
                );
                self.draw_region(index, region, frame, area, palette, graphics);
            }
        }
    }
    fn draw_region(
        &self,
        index: usize,
        region: &UiRegion,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        palette: UiPalette,
        graphics: &mut Graphics,
    ) {
        let runtime = self.owners[index]
            .regions
            .iter()
            .find(|state| state.id == region.id)
            .expect("mounted region");
        let active = self.owners[index].focused_region.as_deref() == Some(region.id.as_str())
            && !self.owners[index + 1..].iter().any(|owner| {
                owner
                    .frame
                    .regions
                    .iter()
                    .any(|region| region.placement == UiRegionPlacement::Overlay)
            });
        graphics.occlude(area);
        frame.render_widget(Clear, area);
        let mut tree = runtime.tree.borrow_mut();
        tree.render(&region.root, frame, area, palette, active);
        tree.record_graphics(&region.root, area, graphics);
    }
}

fn shortcut_matches(binding: &str, key: KeyEvent) -> bool {
    let mut modifiers = KeyModifiers::NONE;
    let mut key_name = None;
    for part in binding.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "option" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "super" | "meta" | "cmd" => modifiers |= KeyModifiers::SUPER,
            _ => key_name = Some(part.to_ascii_lowercase()),
        }
    }
    let Some(name) = key_name else {
        return false;
    };
    if key.modifiers != modifiers {
        return false;
    }
    match key.code {
        KeyCode::Char(value) => {
            name == value.to_ascii_lowercase().to_string() || (value == ' ' && name == "space")
        }
        KeyCode::Enter => name == "enter" || name == "return",
        KeyCode::Esc => name == "esc" || name == "escape",
        KeyCode::Tab => name == "tab",
        KeyCode::BackTab => name == "backtab",
        KeyCode::Up => name == "up",
        KeyCode::Down => name == "down",
        KeyCode::Left => name == "left",
        KeyCode::Right => name == "right",
        KeyCode::Home => name == "home",
        KeyCode::End => name == "end",
        KeyCode::PageUp => name == "pageup",
        KeyCode::PageDown => name == "pagedown",
        KeyCode::Backspace => name == "backspace",
        KeyCode::Delete => name == "delete",
        KeyCode::Insert => name == "insert",
        KeyCode::F(number) => name == format!("f{number}"),
        _ => false,
    }
}
