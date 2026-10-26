//! One ordered pointer gesture across native controls and asynchronous custom components.
use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use pi_plugin::{UiMouseButton, UiMouseEvent, UiMouseEventKind as Kind, UiMouseResult};

use super::super::InputDisposition;
use super::*;

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const MAX_DEFERRED_INPUTS: usize = 1024;

#[derive(Clone, PartialEq, Eq)]
struct Target {
    region: FocusTarget,
    area: runtime::CustomMouseArea,
}

impl Target {
    fn same_component(&self, other: &Self) -> bool {
        self.region == other.region
            && self.area.node == other.area.node
            && self.area.instance == other.area.instance
    }
}

struct Pending {
    id: u64,
    target: Target,
    event: UiMouseEvent,
    raw: MouseEvent,
    deadline: Instant,
    continuation: bool,
    focus_revision: u64,
}

struct Gesture {
    target: Target,
    nested: Option<u64>,
    press: Option<(u16, u16)>,
    button: UiMouseButton,
    moved: bool,
}

struct Click {
    target: Target,
    nested: Option<u64>,
    point: (u16, u16),
    button: UiMouseButton,
    at: Instant,
    count: u8,
}

#[derive(Default)]
pub(super) struct MouseState {
    serial: u64,
    pending: Option<Pending>,
    gesture: Option<Gesture>,
    last_click: Option<Click>,
    deferred: VecDeque<Event>,
    fallback: VecDeque<Event>,
    suppress_gesture: bool,
    clear_selection: bool,
}

fn normalized(raw: MouseEvent, area: &runtime::CustomMouseArea) -> Option<UiMouseEvent> {
    let button = |button| match button {
        MouseButton::Left => UiMouseButton::Left,
        MouseButton::Middle => UiMouseButton::Middle,
        MouseButton::Right => UiMouseButton::Right,
    };
    let (kind, button, wheel_delta) = match raw.kind {
        MouseEventKind::Down(value) => (Kind::Press, button(value), None),
        MouseEventKind::Up(value) => (Kind::Release, button(value), None),
        MouseEventKind::Drag(value) => (Kind::Drag, button(value), None),
        MouseEventKind::Moved => (Kind::Move, UiMouseButton::None, None),
        MouseEventKind::ScrollUp => (Kind::Wheel, UiMouseButton::None, Some(-3)),
        MouseEventKind::ScrollDown => (Kind::Wheel, UiMouseButton::None, Some(3)),
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return None,
    };
    Some(UiMouseEvent {
        kind,
        button,
        wheel_delta,
        click_count: None,
        x: i32::from(raw.column) - area.origin_x,
        y: i32::from(raw.row) - area.origin_y,
        screen_x: raw.column,
        screen_y: raw.row,
        width: area.width,
        height: area.height,
        shift: raw.modifiers.contains(KeyModifiers::SHIFT),
        alt: raw.modifiers.contains(KeyModifiers::ALT),
        ctrl: raw.modifiers.contains(KeyModifiers::CONTROL),
    })
}

impl SurfaceOwners {
    pub(in crate::tui::extensions::surface) fn input_packet(
        &mut self,
        event: &Event,
        snapshot: &UiSurfaceSnapshot,
    ) -> InputDisposition {
        self.validate_mouse();
        if matches!(event, Event::FocusLost | Event::Resize(..)) {
            self.cancel_mouse();
            return InputDisposition::Ignored;
        }
        if self.mouse.pending.is_some() {
            if matches!(event, Event::Key(key) if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, crossterm::event::KeyCode::Char('c' | 'd')))
            {
                self.cancel_mouse();
                return InputDisposition::Ignored;
            }
            if self.mouse.deferred.len() < MAX_DEFERRED_INPUTS {
                self.mouse.deferred.push_back(event.clone());
                return InputDisposition::Deferred;
            }
            self.cancel_mouse();
        }
        if let Event::Mouse(raw) = event {
            if self.mouse.suppress_gesture {
                match raw.kind {
                    MouseEventKind::Down(_) | MouseEventKind::Moved => {
                        self.mouse.suppress_gesture = false;
                    }
                    MouseEventKind::Up(_) => {
                        self.mouse.suppress_gesture = false;
                        return InputDisposition::Handled;
                    }
                    MouseEventKind::Drag(_) => return InputDisposition::Handled,
                    // Wheel events are independent from a cancelled button gesture.
                    _ => {}
                }
            }
            return self.mouse_input(*raw, snapshot);
        }
        if self.input(event, snapshot) {
            InputDisposition::Handled
        } else {
            InputDisposition::Ignored
        }
    }

    pub(in crate::tui::extensions::surface) fn pop_mouse_input(&mut self) -> Option<(Event, bool)> {
        self.mouse
            .fallback
            .pop_front()
            .map(|event| (event, true))
            .or_else(|| {
                if self.mouse.pending.is_none() {
                    self.mouse.deferred.pop_front().map(|event| (event, false))
                } else {
                    None
                }
            })
    }

    pub(in crate::tui::extensions::surface) fn mouse_deadline(&self) -> Option<Instant> {
        self.mouse.pending.as_ref().map(|pending| pending.deadline)
    }

    pub(in crate::tui::extensions::surface) fn expire_mouse(&mut self, now: Instant) {
        if self
            .mouse_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            self.cancel_mouse();
        }
    }

    pub(in crate::tui::extensions::surface) fn take_mouse_selection_reset(&mut self) -> bool {
        std::mem::take(&mut self.mouse.clear_selection)
    }

    pub(in crate::tui::extensions::surface) fn cancel_mouse(&mut self) {
        let pressed = self
            .mouse
            .gesture
            .as_ref()
            .is_some_and(|gesture| gesture.press.is_some())
            || self
                .mouse
                .pending
                .as_ref()
                .is_some_and(|pending| matches!(pending.event.kind, Kind::Press | Kind::Drag));
        if pressed {
            self.mouse.suppress_gesture = true;
        }
        for event in &self.mouse.deferred {
            if let Event::Mouse(raw) = event {
                match raw.kind {
                    MouseEventKind::Down(_) => self.mouse.suppress_gesture = true,
                    MouseEventKind::Up(_) => self.mouse.suppress_gesture = false,
                    _ => {}
                }
            }
        }
        self.mouse.pending = None;
        self.mouse.gesture = None;
        self.mouse.last_click = None;
        self.mouse
            .deferred
            .retain(|event| !matches!(event, Event::Mouse(_)));
        for owner in &mut self.owners {
            owner.events.retain(|event| {
                !matches!(
                    event,
                    UiSurfaceEvent::Interaction {
                        event: UiInteraction::CustomMouse { .. },
                        ..
                    }
                )
            });
        }
    }

    fn target_alive(&self, target: &Target) -> bool {
        let Some((index, region_index)) = self.locate(&target.region) else {
            return false;
        };
        let region = &self.owners[index].frame.regions[region_index];
        matches!(runtime::find_node(&region.root, &target.area.node), Some(pi_plugin::UiNode { kind: pi_plugin::UiNodeKind::Custom { mouse: true, instance_id, .. }, .. }) if *instance_id == target.area.instance)
            && self.owners[index]
                .regions
                .iter()
                .find(|state| state.id == region.id)
                .is_some_and(|state| {
                    // A press may change a list's description and reflow its region. Like Pi,
                    // retain its original coordinate transform until release/click completes.
                    state.bounds.get().is_some()
                        && state.tree.borrow().has_mouse_target(&target.area)
                })
    }

    pub(super) fn validate_mouse(&mut self) {
        let invalid = self.mouse.pending.as_ref().is_some_and(|pending| {
            !self.target_alive(&pending.target)
                || (pending.focus_revision != self.focus_revision
                    && self.focused.as_ref() != Some(&pending.target.region))
        }) || self
            .mouse
            .gesture
            .as_ref()
            .is_some_and(|gesture| !self.target_alive(&gesture.target));
        if invalid {
            self.cancel_mouse();
        }
    }

    fn mouse_region(&self, raw: MouseEvent) -> Option<FocusTarget> {
        let contains = |index: usize, region: &UiRegion| {
            self.owners[index]
                .regions
                .iter()
                .find(|state| state.id == region.id)
                .and_then(|state| state.bounds.get())
                .is_some_and(|area| area.contains((raw.column, raw.row).into()))
        };
        for (index, region_index) in self.overlay_order().into_iter().rev() {
            let region = &self.owners[index].frame.regions[region_index];
            let target = self.target(index, &region.id);
            if contains(index, region)
                || (self.focused.as_ref() == Some(&target)
                    && region.overlay.as_ref().is_none_or(|options| options.modal))
            {
                return Some(target);
            }
        }
        for (index, owner) in self.owners.iter().enumerate().rev() {
            for region in owner.frame.regions.iter().rev() {
                if region.placement != UiRegionPlacement::Overlay
                    && self.region_visible(index, region)
                    && contains(index, region)
                {
                    return Some(self.target(index, &region.id));
                }
            }
        }
        None
    }

    fn mouse_input(&mut self, raw: MouseEvent, snapshot: &UiSurfaceSnapshot) -> InputDisposition {
        // Pi treats wheel input independently from an in-progress drag.
        let wheel = matches!(
            raw.kind,
            MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        );
        if !wheel && let Some(gesture) = &mut self.mouse.gesture {
            if gesture
                .press
                .is_some_and(|point| point != (raw.column, raw.row))
            {
                gesture.moved = true;
                self.mouse.last_click = None;
            }
            let target = gesture.target.clone();
            let nested = gesture.nested;
            if let Some(event) = normalized(raw, &target.area) {
                self.send_mouse(target, nested, raw, event, true, snapshot);
                return InputDisposition::Deferred;
            }
        }
        let Some(region) = self.mouse_region(raw) else {
            return InputDisposition::Ignored;
        };
        let (index, region_index) = self.locate(&region).expect("visible mouse region");
        let state = self.owners[index]
            .regions
            .iter()
            .find(|state| state.id == self.owners[index].frame.regions[region_index].id)
            .expect("mounted region");
        let area = state.tree.borrow().custom_mouse_at(raw.column, raw.row);
        if let Some(area) = area
            && let Some(event) = normalized(raw, &area)
        {
            self.send_mouse(Target { region, area }, None, raw, event, false, snapshot);
            InputDisposition::Deferred
        } else {
            self.mouse_fallback(&region, raw, snapshot)
        }
    }

    fn mouse_fallback(
        &mut self,
        target: &FocusTarget,
        raw: MouseEvent,
        snapshot: &UiSurfaceSnapshot,
    ) -> InputDisposition {
        let Some((index, region_index)) = self.locate(target) else {
            return InputDisposition::Handled;
        };
        let region = &self.owners[index].frame.regions[region_index];
        let modal = region.placement == UiRegionPlacement::Overlay
            && region.overlay.as_ref().is_none_or(|options| options.modal);
        let state = self.owners[index]
            .regions
            .iter()
            .find(|state| state.id == region.id)
            .expect("mounted region");
        let (handled, interaction) = state
            .tree
            .borrow_mut()
            .input(&region.root, &Event::Mouse(raw));
        if handled {
            self.activate(target.clone());
        }
        if let Some((node, interaction)) = interaction {
            self.emit(index, target.region.clone(), node, interaction, snapshot);
        }
        if handled || modal {
            InputDisposition::Handled
        } else {
            InputDisposition::Ignored
        }
    }

    fn send_mouse(
        &mut self,
        target: Target,
        nested: Option<u64>,
        raw: MouseEvent,
        event: UiMouseEvent,
        continuation: bool,
        snapshot: &UiSurfaceSnapshot,
    ) {
        self.mouse.serial += 1;
        let id = self.mouse.serial;
        let index = self
            .position(&target.region.owner)
            .expect("live mouse owner");
        let interaction = UiInteraction::CustomMouse {
            id,
            instance_id: target.area.instance.clone(),
            target: nested,
            event: event.clone(),
        };
        let region = target.region.region.clone();
        let node = target.area.node.clone();
        self.mouse.pending = Some(Pending {
            id,
            target,
            event,
            raw,
            deadline: Instant::now() + REPLY_TIMEOUT,
            continuation,
            focus_revision: self.focus_revision,
        });
        self.emit(index, region, node, interaction, snapshot);
    }

    pub(in crate::tui::extensions::surface) fn mouse_result(
        &mut self,
        owner: &str,
        id: u64,
        result: UiMouseResult,
        snapshot: &UiSurfaceSnapshot,
    ) {
        if !self
            .mouse
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id && pending.target.region.owner == owner)
        {
            return;
        }
        self.validate_mouse();
        let Some(pending) = self.mouse.pending.take() else {
            return;
        };
        if result.cancelled {
            // Retain enough state for cancel_mouse to suppress the rest of an abandoned press.
            self.mouse.pending = Some(pending);
            self.cancel_mouse();
            return;
        }
        let handled = result.handled || result.capture || result.focus;
        if handled {
            if result.focus {
                self.focus(
                    owner,
                    &pending.target.region.region,
                    &pending.target.area.node,
                );
            }
            if pending.event.kind == Kind::Press || result.capture {
                self.mouse.suppress_gesture = false;
                if pending.continuation && pending.event.kind != Kind::Press {
                    if let Some(gesture) = &mut self.mouse.gesture {
                        gesture.nested = result.target.or(gesture.nested);
                    }
                } else {
                    self.mouse.gesture = Some(Gesture {
                        target: pending.target.clone(),
                        nested: result.target,
                        press: (pending.event.kind == Kind::Press)
                            .then_some((pending.raw.column, pending.raw.row)),
                        button: pending.event.button,
                        moved: false,
                    });
                }
            }
            if pending.event.kind == Kind::Press {
                self.mouse.clear_selection = true;
            }
        } else if !pending.continuation
            && self.mouse_fallback(&pending.target.region, pending.raw, snapshot)
                == InputDisposition::Ignored
        {
            self.mouse.fallback.push_back(Event::Mouse(pending.raw));
        }
        if pending.event.kind == Kind::Release {
            if let Some(gesture) = &self.mouse.gesture
                && !gesture.moved
                && gesture.button == pending.event.button
                && gesture.press == Some((pending.raw.column, pending.raw.row))
            {
                let target = gesture.target.clone();
                let nested = gesture.nested;
                let now = Instant::now();
                let count = self
                    .mouse
                    .last_click
                    .as_ref()
                    .filter(|click| {
                        click.target.same_component(&target)
                            && click.nested == nested
                            && click.point == (pending.raw.column, pending.raw.row)
                            && click.button == pending.event.button
                            && now.duration_since(click.at) <= DOUBLE_CLICK_INTERVAL
                    })
                    .map_or(1, |click| click.count % 3 + 1);
                self.mouse.last_click = Some(Click {
                    target: target.clone(),
                    nested,
                    point: (pending.raw.column, pending.raw.row),
                    button: pending.event.button,
                    at: now,
                    count,
                });
                let mut event = pending.event;
                event.kind = Kind::Click;
                event.click_count = Some(count);
                self.send_mouse(target, nested, pending.raw, event, true, snapshot);
            } else {
                self.mouse.gesture = None;
            }
        } else if pending.event.kind == Kind::Click {
            self.mouse.gesture = None;
        }
    }
}
