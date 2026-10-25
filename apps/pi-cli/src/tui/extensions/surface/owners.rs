//! Generation-owned descriptions, semantic events, and Rust component state.
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet, VecDeque},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use pi_plugin::{
    UiFocusTarget, UiInteraction, UiRegion, UiRegionBounds, UiRegionPlacement, UiRegionSnapshot,
    UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot,
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
#[path = "mouse.rs"]
mod mouse;

struct RegionRuntime {
    id: String,
    tree: RefCell<TreeRuntime>,
    bounds: Cell<Option<Rect>>,
    order: u64,
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct FocusTarget {
    owner: String,
    region: String,
}
struct Owner {
    frame: UiSurfaceFrame,
    admitted: bool,
    regions: Vec<RegionRuntime>,
    requested_focus: Option<(String, String)>,
    pending: Option<oneshot::Sender<UiSurfaceEvent>>,
    events: VecDeque<UiSurfaceEvent>,
}

#[derive(Default)]
pub(super) struct SurfaceOwners {
    owners: Vec<Owner>,
    retired: HashSet<String>,
    focused: Option<FocusTarget>,
    focus_revision: u64,
    focus_history: Vec<FocusTarget>,
    released: HashSet<FocusTarget>,
    order: u64,
    mouse: mouse::MouseState,
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
    fn region_visible(&self, index: usize, region: &UiRegion) -> bool {
        self.visible(index, region.placement)
            && !region
                .overlay
                .as_ref()
                .is_some_and(|options| options.hidden)
    }
    fn target(&self, index: usize, region: &str) -> FocusTarget {
        FocusTarget {
            owner: self.owners[index].frame.owner.clone(),
            region: region.into(),
        }
    }
    fn locate(&self, target: &FocusTarget) -> Option<(usize, usize)> {
        let index = self.position(&target.owner)?;
        let region = self.owners[index]
            .frame
            .regions
            .iter()
            .position(|region| region.id == target.region)?;
        self.region_visible(index, &self.owners[index].frame.regions[region])
            .then_some((index, region))
    }
    fn activate(&mut self, target: FocusTarget) {
        let Some((index, region_index)) = self.locate(&target) else {
            return;
        };
        if self.focused.as_ref() != Some(&target) {
            self.focus_revision = self.focus_revision.wrapping_add(1);
            if let Some(previous) = self.focused.replace(target.clone()) {
                self.focus_history.retain(|entry| entry != &previous);
                self.focus_history.push(previous);
            }
        }
        self.released.remove(&target);
        if self.owners[index].frame.regions[region_index].placement == UiRegionPlacement::Overlay {
            self.order = self.order.wrapping_add(1);
            if let Some(region) = self.owners[index]
                .regions
                .iter_mut()
                .find(|region| region.id == target.region)
            {
                region.order = self.order;
            }
        }
    }
    fn restore_focus(&mut self) {
        if self.focused.is_some() {
            self.focus_revision = self.focus_revision.wrapping_add(1);
        }
        self.focused = None;
        while let Some(previous) = self.focus_history.pop() {
            if self.locate(&previous).is_some() && !self.released.contains(&previous) {
                self.activate(previous);
                return;
            }
        }
        for (index, region_index) in self.overlay_order().into_iter().rev() {
            let region = &self.owners[index].frame.regions[region_index];
            let target = self.target(index, &region.id);
            if !region
                .overlay
                .as_ref()
                .is_some_and(|options| options.non_capturing)
                && !self.released.contains(&target)
            {
                self.activate(target);
                return;
            }
        }
    }
    fn overlay_order(&self) -> Vec<(usize, usize)> {
        let mut regions = Vec::new();
        for (index, owner) in self.owners.iter().enumerate() {
            for (region_index, region) in owner.frame.regions.iter().enumerate() {
                if region.placement == UiRegionPlacement::Overlay
                    && self.region_visible(index, region)
                {
                    let order = owner
                        .regions
                        .iter()
                        .find(|state| state.id == region.id)
                        .map_or(0, |state| state.order);
                    regions.push((order, index, region_index));
                }
            }
        }
        regions.sort_by_key(|(order, ..)| *order);
        regions
            .into_iter()
            .map(|(_, index, region)| (index, region))
            .collect()
    }
    pub(super) fn begin_frame(&self) {
        for owner in &self.owners {
            for region in &owner.regions {
                region.bounds.set(None);
            }
        }
    }
    pub(super) fn snapshots(&self) -> Vec<UiRegionSnapshot> {
        self.owners
            .iter()
            .enumerate()
            .flat_map(|(index, owner)| {
                owner.frame.regions.iter().map(move |region| {
                    let visible = self.region_visible(index, region);
                    let bounds = visible
                        .then(|| {
                            owner
                                .regions
                                .iter()
                                .find(|state| state.id == region.id)
                                .and_then(|state| state.bounds.get())
                        })
                        .flatten();
                    UiRegionSnapshot {
                        owner: owner.frame.owner.clone(),
                        region_id: region.id.clone(),
                        focused: visible
                            && self.focused.as_ref() == Some(&self.target(index, &region.id)),
                        bounds: bounds.map(|area| UiRegionBounds {
                            row: area.y,
                            col: area.x,
                            width: area.width,
                            height: area.height,
                        }),
                    }
                })
            })
            .collect()
    }
    pub(super) fn compose(&self) -> UiSurfaceFrame {
        let mut frame = UiSurfaceFrame::default();
        let mut transcript = BTreeMap::new();
        for (index, owner) in self.owners.iter().enumerate() {
            for region in &owner.frame.regions {
                if self.region_visible(index, region) {
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
        let previous_visible: HashSet<_> = entry
            .frame
            .regions
            .iter()
            .filter(|region| {
                !region
                    .overlay
                    .as_ref()
                    .is_some_and(|options| options.hidden)
            })
            .map(|region| region.id.clone())
            .collect();
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
                        bounds: Cell::new(None),
                        order: 0,
                    });
                    entry.regions.last_mut().expect("region inserted")
                };
            state.tree.get_mut().reconcile(&region.root);
            if !previous_visible.contains(&region.id) {
                self.order = self.order.wrapping_add(1);
                state.order = self.order;
            }
            if region
                .overlay
                .as_ref()
                .is_some_and(|options| options.hidden)
            {
                state.bounds.set(None);
            }
        }
        entry.events.retain_mut(|event| {
            let UiSurfaceEvent::Interaction { region_id, node_id, revision, event, .. } = event else { return true; };
            if let UiInteraction::Shortcut { key } = event {
                if !frame.shortcuts.iter().any(|shortcut| &shortcut.key == key) { return false; }
            } else {
                let old = entry.frame.regions.iter().find(|region| region.id == *region_id).and_then(|region| runtime::find_node(&region.root, node_id));
                let next = frame.regions.iter().find(|region| region.id == *region_id && !region.overlay.as_ref().is_some_and(|options| options.hidden)).and_then(|region| runtime::find_node(&region.root, node_id));
                if !matches!((old, next), (Some(old), Some(next)) if std::mem::discriminant(&old.kind) == std::mem::discriminant(&next.kind)) { return false; }
            }
            *revision = frame.revision;
            true
        });
        entry.admitted = true;
        entry.frame = frame;
        let survives = |target: &FocusTarget| {
            target.owner != entry.frame.owner
                || entry
                    .frame
                    .regions
                    .iter()
                    .any(|region| region.id == target.region)
        };
        self.focus_history.retain(survives);
        self.released.retain(survives);
        let requested_focus = entry.requested_focus.take();
        if let Some(old) = entry.pending.replace(response) {
            let _ = old.send(UiSurfaceEvent::Closed);
        }
        if self
            .focused
            .as_ref()
            .is_some_and(|target| self.locate(target).is_none())
        {
            self.restore_focus();
        }
        let new_focus = self.owners[index]
            .frame
            .regions
            .iter()
            .rev()
            .find(|region| {
                !previous_visible.contains(&region.id)
                    && self.region_visible(index, region)
                    && if region.placement == UiRegionPlacement::Overlay {
                        !region
                            .overlay
                            .as_ref()
                            .is_some_and(|options| options.non_capturing)
                    } else {
                        self.focused.is_none()
                            && self.owners[index].regions.iter().any(|state| {
                                state.id == region.id && state.tree.borrow().interactive()
                            })
                    }
            })
            .map(|region| self.target(index, &region.id));
        if let Some(target) = new_focus {
            self.activate(target);
        }
        if let Some((region_id, node_id)) = requested_focus {
            let owner = self.owners[index].frame.owner.clone();
            self.focus(&owner, &region_id, &node_id);
        }
        self.validate_mouse();
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
        let targets = if matches!(event, Event::Mouse(_)) {
            let mut targets: Vec<_> = self.overlay_order().into_iter().rev().collect();
            for (index, owner) in self.owners.iter().enumerate().rev() {
                targets.extend(owner.frame.regions.iter().enumerate().rev().filter_map(
                    |(region_index, region)| {
                        (region.placement != UiRegionPlacement::Overlay
                            && self.region_visible(index, region))
                        .then_some((index, region_index))
                    },
                ));
            }
            targets
        } else {
            self.focused
                .as_ref()
                .and_then(|target| self.locate(target))
                .into_iter()
                .collect()
        };
        for (index, region_index) in targets {
            let region = &self.owners[index].frame.regions[region_index];
            let target = self.target(index, &region.id);
            let modal = self.focused.as_ref() == Some(&target)
                && region.placement == UiRegionPlacement::Overlay
                && region.overlay.as_ref().is_none_or(|options| options.modal);
            let tree = self.owners[index]
                .regions
                .iter()
                .find(|state| state.id == region.id)
                .expect("mounted region");
            let (handled, interaction) = tree.tree.borrow_mut().input(&region.root, event);
            if handled && matches!(event, Event::Mouse(_)) {
                self.activate(target.clone());
            }
            if let Some((node_id, interaction)) = interaction {
                self.emit(index, target.region, node_id, interaction, snapshot);
            }
            if handled || modal {
                return true;
            }
        }
        false
    }
    pub(super) fn focus(&mut self, owner: &str, region_id: &str, node_id: &str) -> bool {
        if !self.accepts(owner) {
            return false;
        }
        let index = self.ensure_owner(owner);
        let target = self.target(index, region_id);
        if self.owners[index]
            .frame
            .regions
            .iter()
            .any(|region| region.id == region_id)
            && self.locate(&target).is_none()
        {
            return false;
        }
        let entry = &mut self.owners[index];
        entry.requested_focus = Some((region_id.into(), node_id.into()));
        if let Some(state) = entry.regions.iter_mut().find(|state| state.id == region_id)
            && state.tree.get_mut().focus(node_id)
        {
            entry.requested_focus = None;
            self.activate(target);
            true
        } else {
            false
        }
    }

    pub(super) fn unfocus(
        &mut self,
        owner: &str,
        region_id: &str,
        destination: Option<&UiFocusTarget>,
    ) {
        let target = FocusTarget {
            owner: owner.into(),
            region: region_id.into(),
        };
        if self.focused.as_ref() != Some(&target) {
            return;
        }
        let next = if let Some(UiFocusTarget::Region { region_id }) = destination {
            let next = FocusTarget {
                owner: owner.into(),
                region: region_id.clone(),
            };
            if next == target || self.locate(&next).is_none() {
                return;
            }
            Some(next)
        } else {
            None
        };
        self.released.insert(target.clone());
        self.focus_history.retain(|previous| previous != &target);
        self.focus_revision = self.focus_revision.wrapping_add(1);
        self.focused = None;
        if let Some(next) = next {
            self.activate(next);
        } else if destination.is_none() {
            self.restore_focus();
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
        self.focus_history.retain(|target| target.owner != owner);
        self.released.retain(|target| target.owner != owner);
        if self
            .focused
            .as_ref()
            .is_some_and(|target| target.owner == owner)
        {
            self.restore_focus();
        }
        self.validate_mouse();
    }
    pub(super) fn close(&mut self) {
        self.cancel_mouse();
        for owner in self.owners.drain(..) {
            self.retired.insert(owner.frame.owner);
            if let Some(sender) = owner.pending {
                let _ = sender.send(UiSurfaceEvent::Closed);
            }
        }
        self.focused = None;
        self.focus_history.clear();
        self.released.clear();
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
            for region in owner.frame.regions.iter().filter(|region| {
                region.placement == placement && self.region_visible(index, region)
            }) {
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
        for (index, region_index) in self.overlay_order() {
            let region = &self.owners[index].frame.regions[region_index];
            if self.focused.as_ref() == Some(&self.target(index, &region.id))
                && region.overlay.as_ref().is_none_or(|options| options.modal)
            {
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
            let area = super::overlay::bounds(
                root,
                &region.root,
                &region.overlay.clone().unwrap_or_default(),
                palette,
            );
            self.draw_region(index, region, frame, area, palette, graphics);
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
        let active = self.focused.as_ref() == Some(&self.target(index, &region.id));
        runtime.bounds.set(Some(area));
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
