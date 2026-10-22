//! Frontend-owned presentation of extension components. JavaScript never owns terminal modes.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use pi_plugin::{UiRegion, UiRegionPlacement, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use std::collections::{HashMap, VecDeque};
use tokio::sync::oneshot;

#[derive(Default)]
pub(super) struct ExtensionSurface {
    pub(super) frame: UiSurfaceFrame,
    pending: Option<oneshot::Sender<UiSurfaceEvent>>,
    events: VecDeque<UiSurfaceEvent>,
    inputs: HashMap<u64, Event>,
    sequence: u64,
    retired: std::collections::HashSet<String>,
    wake: bool,
    pub(super) forwarded: VecDeque<Event>,
}
impl ExtensionSurface {
    pub(super) fn exchange(
        &mut self,
        frame: UiSurfaceFrame,
        response: oneshot::Sender<UiSurfaceEvent>,
        snapshot: &UiSurfaceSnapshot,
    ) {
        if self.retired.contains(&frame.owner) {
            let _ = response.send(UiSurfaceEvent::Closed);
            return;
        }
        if self.frame.owner != frame.owner {
            if !self.frame.owner.is_empty() {
                self.retired.insert(self.frame.owner.clone());
            }
            self.close();
        }
        self.frame = frame;
        if let Some(old) = self.pending.replace(response) {
            let _ = old.send(UiSurfaceEvent::Closed);
        }
        if self.wake {
            self.wake = false;
            self.refresh(snapshot);
        }
        self.flush();
    }
    pub(super) fn refresh(&mut self, snapshot: &UiSurfaceSnapshot) {
        // Coalesce invalidations, preserving all input events.
        self.events
            .retain(|e| !matches!(e, UiSurfaceEvent::Refresh { .. }));
        self.events.push_back(UiSurfaceEvent::Refresh {
            snapshot: snapshot.clone(),
        });
        self.flush();
    }
    pub(super) fn wake(&mut self, snapshot: &UiSurfaceSnapshot) {
        self.wake = true;
        if self.pending.is_some() {
            self.wake = false;
            self.refresh(snapshot);
        }
    }
    fn flush(&mut self) {
        if self.pending.is_some() && !self.events.is_empty() {
            let _ = self
                .pending
                .take()
                .unwrap()
                .send(self.events.pop_front().unwrap());
        }
    }
    pub(super) fn input(&mut self, event: Event, snapshot: &UiSurfaceSnapshot) -> bool {
        if !self.frame.capture_input {
            return false;
        }
        let data = match &event {
            Event::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                match encode_key(*key) {
                    Some(data) => data,
                    None => return false,
                }
            }
            Event::Paste(text) => format!("\x1b[200~{text}\x1b[201~"),
            _ => return false,
        };
        self.sequence = self.sequence.wrapping_add(1);
        // Keep only recent events for app-action forwarding; acknowledged input is bounded.
        let oldest = self.sequence.saturating_sub(256);
        self.inputs.retain(|seq, _| *seq >= oldest);
        self.inputs.insert(self.sequence, event);
        self.events.push_back(UiSurfaceEvent::Input {
            data,
            sequence: self.sequence,
            snapshot: snapshot.clone(),
        });
        self.flush();
        true
    }
    pub(super) fn forward(&mut self, sequence: u64) {
        if let Some(event) = self.inputs.remove(&sequence) {
            self.forwarded.push_back(event);
        }
    }
    pub(super) fn is_retired(&self, owner: &str) -> bool {
        self.retired.contains(owner)
    }
    pub(super) fn retire(&mut self, owner: String) {
        if self.frame.owner == owner {
            self.close();
        }
        self.retired.insert(owner);
    }
    pub(super) fn close(&mut self) {
        if let Some(sender) = self.pending.take() {
            let _ = sender.send(UiSurfaceEvent::Closed);
        }
        self.frame = UiSurfaceFrame::default();
        self.events.clear();
        self.inputs.clear();
        self.wake = false;
    }
    pub(super) fn has_regions(&self) -> bool {
        !self.frame.regions.is_empty()
    }
    pub(super) fn height(&self, placement: UiRegionPlacement, max: u16) -> u16 {
        self.frame
            .regions
            .iter()
            .filter(|r| r.placement == placement)
            .map(|r| r.lines.len())
            .sum::<usize>()
            .min(usize::from(max)) as u16
    }
    pub(super) fn draw_regions(
        &self,
        frame: &mut ratatui::Frame<'_>,
        placement: UiRegionPlacement,
        mut area: Rect,
    ) {
        for region in self
            .frame
            .regions
            .iter()
            .filter(|r| r.placement == placement)
        {
            let height = region.lines.len().min(usize::from(area.height)) as u16;
            draw_region(frame, region, Rect::new(area.x, area.y, area.width, height));
            area.y += height;
            area.height -= height;
        }
    }
    pub(super) fn draw_overlays(&self, frame: &mut ratatui::Frame<'_>) {
        let root = frame.area();
        for region in self
            .frame
            .regions
            .iter()
            .filter(|r| r.placement == UiRegionPlacement::Overlay)
        {
            let width = region.width.min(root.width);
            let height = (region.lines.len().min(usize::from(root.height))) as u16;
            let x = region
                .column
                .unwrap_or((root.width - width) / 2)
                .min(root.width.saturating_sub(width));
            let y = region
                .row
                .unwrap_or((root.height - height) / 2)
                .min(root.height.saturating_sub(height));
            draw_region(
                frame,
                region,
                Rect::new(root.x + x, root.y + y, width, height),
            );
        }
    }
}
impl Drop for ExtensionSurface {
    fn drop(&mut self) {
        self.close();
    }
}
fn draw_region(frame: &mut ratatui::Frame<'_>, region: &UiRegion, area: Rect) {
    frame.render_widget(Clear, area);
    let lines: Vec<Line<'_>> = region
        .lines
        .iter()
        .map(|line| {
            Line::from(
                line.iter()
                    .map(|span| {
                        let mut style = Style::default();
                        if let Some([r, g, b]) = span.foreground {
                            style = style.fg(Color::Rgb(r, g, b));
                        }
                        if let Some([r, g, b]) = span.background {
                            style = style.bg(Color::Rgb(r, g, b));
                        }
                        for (enabled, modifier) in [
                            (span.bold, Modifier::BOLD),
                            (span.italic, Modifier::ITALIC),
                            (span.underline, Modifier::UNDERLINED),
                            (span.inverse, Modifier::REVERSED),
                        ] {
                            if enabled {
                                style = style.add_modifier(modifier);
                            }
                        }
                        Span::styled(&span.text, style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    if let Some([row, col]) = region.cursor
        && row < area.height
        && col < area.width
    {
        frame.set_cursor_position((area.x + col, area.y + row));
    }
}
pub(super) fn encode_key(key: KeyEvent) -> Option<String> {
    let modifiers = 1
        + u8::from(key.modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(key.modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(key.modifiers.contains(KeyModifiers::CONTROL));
    let plain = modifiers == 1;
    let value = match key.code {
        KeyCode::Char(c)
            if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() =>
        {
            ((c.to_ascii_lowercase() as u8 - b'a') + 1) as char
        }
        KeyCode::Char(c) => {
            return Some(if key.modifiers.contains(KeyModifiers::ALT) {
                format!("\x1b{c}")
            } else {
                c.to_string()
            });
        }
        KeyCode::Enter if plain => '\r',
        KeyCode::Esc => '\x1b',
        KeyCode::Backspace if plain => '\x7f',
        KeyCode::Tab if plain => '\t',
        KeyCode::BackTab => return Some("\x1b[Z".into()),
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Right
        | KeyCode::Left
        | KeyCode::Home
        | KeyCode::End => {
            let suffix = match key.code {
                KeyCode::Up => 'A',
                KeyCode::Down => 'B',
                KeyCode::Right => 'C',
                KeyCode::Left => 'D',
                KeyCode::Home => 'H',
                _ => 'F',
            };
            return Some(if plain {
                format!("\x1b[{suffix}")
            } else {
                format!("\x1b[1;{modifiers}{suffix}")
            });
        }
        KeyCode::Delete | KeyCode::Insert | KeyCode::PageUp | KeyCode::PageDown => {
            let code = match key.code {
                KeyCode::Delete => 3,
                KeyCode::Insert => 2,
                KeyCode::PageUp => 5,
                _ => 6,
            };
            return Some(if plain {
                format!("\x1b[{code}~")
            } else {
                format!("\x1b[{code};{modifiers}~")
            });
        }
        KeyCode::Enter => return Some(format!("\x1b[13;{modifiers}u")),
        KeyCode::Backspace => return Some(format!("\x1b[127;{modifiers}u")),
        _ => return None,
    };
    Some(value.to_string())
}

/// Session-start hooks can await UI before a PiSession exists. Keep the same
/// component channel live during activation, then hand its pending exchange to run_loop.
pub(crate) async fn during_startup<T>(
    startup: impl std::future::Future<Output = Result<T, String>>,
    receiver: &mut crate::plugin_ui::SurfaceReceiver,
    fullscreen: bool,
) -> Result<T, String> {
    use crate::plugin_ui::SurfaceRequest;
    use crossterm::event::EventStream;
    use futures::StreamExt;
    use pi_plugin::UiSurfaceAction;
    tokio::pin!(startup);
    let mut terminal: Option<super::TerminalSession> = None;
    let mut events = EventStream::new();
    let mut surface = ExtensionSurface::default();
    let result = loop {
        tokio::select! {
            result = &mut startup => break result,
            request = receiver.requests.recv() => {
                let Some(request) = request else { break Err("extension UI closed during startup".into()); };
                if terminal.is_none() {
                    terminal = Some(super::TerminalSession::new(fullscreen).map_err(|e| e.to_string())?);
                }
                let size = terminal.as_mut().unwrap().terminal_mut().size().map_err(|e| e.to_string())?;
                let snapshot = UiSurfaceSnapshot { columns: size.width, rows: size.height, ..receiver.snapshot.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone() };
                *receiver.snapshot.write().unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.clone();
                match request {
                    SurfaceRequest::Exchange(frame, response) => surface.exchange(frame, response, &snapshot),
                    SurfaceRequest::Action(owner, action) => {
                        if matches!(action, UiSurfaceAction::Close) { surface.retire(owner); continue; }
                        if surface.is_retired(&owner) { continue; }
                        if !surface.frame.owner.is_empty() && surface.frame.owner != owner { continue; }
                        match action {
                            UiSurfaceAction::Close => surface.retire(owner),
                            UiSurfaceAction::Wake => surface.wake(&snapshot),
                            // Preserve editor/status operations for the full frontend.
                            other => receiver.initial.push_back(SurfaceRequest::Action(owner, other)),
                        }
                    }
                }
            }
            event = events.next(), if terminal.is_some() => {
                match event {
                    Some(Ok(event)) => {
                        let snapshot = receiver.snapshot.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                        surface.input(event, &snapshot);
                    }
                    Some(Err(e)) => break Err(e.to_string()),
                    None => break Err("terminal closed during startup".into()),
                }
            }
        }
        if let Some(terminal) = terminal.as_mut() {
            terminal
                .terminal_mut()
                .draw(|frame| {
                    let area = frame.area();
                    surface.draw_regions(frame, UiRegionPlacement::Header, area);
                    surface.draw_regions(frame, UiRegionPlacement::Editor, area);
                    surface.draw_overlays(frame);
                })
                .map_err(|e| e.to_string())?;
        }
    };
    if let Some(pending) = surface.pending.take() {
        receiver.initial.push_front(SurfaceRequest::Exchange(
            std::mem::take(&mut surface.frame),
            pending,
        ));
    }
    if let Some(terminal) = terminal {
        terminal.finish().map_err(|e| e.to_string())?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::Backend;
    #[test]
    fn input_is_queued_between_frames_and_retirement_closes_the_waiter() {
        let mut surface = ExtensionSurface::default();
        let snapshot = UiSurfaceSnapshot::default();
        let (sender, mut receiver) = oneshot::channel();
        surface.exchange(
            UiSurfaceFrame {
                owner: "one".into(),
                capture_input: true,
                ..Default::default()
            },
            sender,
            &snapshot,
        );
        let key = Event::Key(KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE));
        assert!(surface.input(key, &snapshot));
        assert!(
            matches!(receiver.try_recv().unwrap(), UiSurfaceEvent::Input { data, .. } if data == "界")
        );
        assert!(surface.input(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &snapshot
        ));
        let (sender, mut receiver) = oneshot::channel();
        surface.exchange(surface.frame.clone(), sender, &snapshot);
        assert!(
            matches!(receiver.try_recv().unwrap(), UiSurfaceEvent::Input { data, .. } if data == "\r")
        );
        let (sender, mut receiver) = oneshot::channel();
        surface.exchange(surface.frame.clone(), sender, &snapshot);
        surface.close();
        assert_eq!(receiver.try_recv().unwrap(), UiSurfaceEvent::Closed);
        assert!(!surface.frame.capture_input);
    }
    #[test]
    fn styles_and_cursor_are_drawn_without_executing_terminal_sequences() {
        use ratatui::{Terminal, backend::TestBackend};
        let region = UiRegion {
            placement: UiRegionPlacement::Overlay,
            lines: vec![vec![pi_plugin::UiTextSpan {
                text: "中文".into(),
                foreground: Some([12, 34, 56]),
                bold: true,
                ..Default::default()
            }]],
            width: 8,
            cursor: Some([0, 4]),
            row: Some(2),
            column: Some(3),
        };
        let mut surface = ExtensionSurface::default();
        surface.frame.regions.push(region);
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
        terminal.draw(|frame| surface.draw_overlays(frame)).unwrap();
        assert_eq!(terminal.backend().buffer()[(3, 2)].symbol(), "中");
        assert_eq!(
            terminal.backend().buffer()[(3, 2)].fg,
            Color::Rgb(12, 34, 56)
        );
        assert_eq!(
            terminal.backend_mut().get_cursor_position().unwrap(),
            (7, 2).into()
        );
    }
    #[test]
    fn special_keys_preserve_modifiers() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)).as_deref(),
            Some("\x1b[13;2u")
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)).as_deref(),
            Some("\x1b[1;3D")
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)).as_deref(),
            Some("\x04")
        );
    }
    #[test]
    fn retired_surface_cannot_remount_after_reload() {
        let mut surface = ExtensionSurface::default();
        let snapshot = UiSurfaceSnapshot::default();
        surface.retire("old".into());
        let (sender, mut receiver) = oneshot::channel();
        surface.exchange(
            UiSurfaceFrame {
                owner: "old".into(),
                capture_input: true,
                ..Default::default()
            },
            sender,
            &snapshot,
        );
        assert_eq!(receiver.try_recv().unwrap(), UiSurfaceEvent::Closed);
        assert!(!surface.frame.capture_input);
    }
}
