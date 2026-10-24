//! Own terminal input once. Preserve original bytes for JS while parsing native events with Termina.
use crossterm::event::Event;
#[cfg(any(unix, test))]
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
#[cfg(any(unix, test))]
use std::collections::VecDeque;
use std::io;

pub(super) struct InputPacket {
    pub(super) event: Option<Event>,
    pub(super) data: Option<String>,
}

#[derive(Default)]
#[cfg(any(unix, test))]
struct InputDecoder {
    parser: termina::Parser,
    pending: Vec<u8>,
    packets: VecDeque<InputPacket>,
}

#[cfg(any(unix, test))]
impl InputDecoder {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.pending.push(byte);
            // A paste can be megabytes long. Feed it to the parser once, not once per prefix.
            if self.pending.starts_with(b"\x1b[200~") && self.pending.len() > 6 {
                if !self.pending.ends_with(b"\x1b[201~") {
                    continue;
                }
                self.parser.parse(&self.pending[6..], true);
            } else {
                self.parser.parse(&[byte], true);
            }
            self.drain();
            // Termina drops some complete reports without emitting an event. Keep their
            // raw bytes in a separate packet so the next key cannot become part of a reply.
            if self.has_complete_control_sequence() {
                self.flush();
            }
        }
    }

    fn has_complete_control_sequence(&self) -> bool {
        let bytes = self.pending.as_slice();
        if bytes.starts_with(b"\x1b[") {
            bytes.len() > 2
                && matches!(bytes.last(), Some(b'@'..=b'~'))
                // These prefixes include a payload after the initial CSI final byte.
                && !bytes.starts_with(b"\x1b[200~")
                && !bytes.starts_with(b"\x1b[M")
                && !bytes.starts_with(b"\x1b[[")
        } else if bytes.starts_with(b"\x1b]") {
            bytes.ends_with(b"\x07") || bytes.ends_with(b"\x1b\\")
        } else {
            bytes.starts_with(b"\x1bP") && bytes.ends_with(b"\x1b\\")
        }
    }

    fn drain(&mut self) {
        while let Some(event) = self.parser.pop() {
            let bytes = std::mem::take(&mut self.pending);
            self.packets.push_back(InputPacket {
                event: native_event(event),
                data: Some(String::from_utf8_lossy(&bytes).into_owned()),
            });
        }
    }

    fn may_timeout(&self) -> bool {
        self.pending.starts_with(b"\x1b")
            && !self.pending.starts_with(b"\x1b[200~")
            && !self.pending.starts_with(b"\x1b]")
            && !self.pending.starts_with(b"\x1bP")
    }

    fn flush(&mut self) {
        self.parser.parse(&[], false);
        self.drain();
        if !self.pending.is_empty() {
            self.packets.push_back(InputPacket {
                event: None,
                data: Some(
                    String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned(),
                ),
            });
            self.parser = termina::Parser::default();
        }
    }
}

#[cfg(any(unix, test))]
fn native_modifiers(modifiers: termina::event::Modifiers) -> KeyModifiers {
    use termina::event::Modifiers as M;
    [
        (M::SHIFT, KeyModifiers::SHIFT),
        (M::ALT, KeyModifiers::ALT),
        (M::CONTROL, KeyModifiers::CONTROL),
        (M::SUPER, KeyModifiers::SUPER),
        (M::HYPER, KeyModifiers::HYPER),
        (M::META, KeyModifiers::META),
    ]
    .into_iter()
    .filter(|(source, _)| modifiers.contains(*source))
    .fold(KeyModifiers::NONE, |result, (_, target)| result | target)
}

#[cfg(any(unix, test))]
fn native_event(event: termina::Event) -> Option<Event> {
    use termina::event as source;
    Some(match event {
        termina::Event::Key(key) => {
            let code = match key.code {
                source::KeyCode::Char(c) => KeyCode::Char(c),
                source::KeyCode::Function(n) => KeyCode::F(n),
                source::KeyCode::Escape => KeyCode::Esc,
                source::KeyCode::Enter => KeyCode::Enter,
                source::KeyCode::Backspace => KeyCode::Backspace,
                source::KeyCode::Tab => KeyCode::Tab,
                source::KeyCode::BackTab => KeyCode::BackTab,
                source::KeyCode::Left => KeyCode::Left,
                source::KeyCode::Right => KeyCode::Right,
                source::KeyCode::Up => KeyCode::Up,
                source::KeyCode::Down => KeyCode::Down,
                source::KeyCode::Home => KeyCode::Home,
                source::KeyCode::End => KeyCode::End,
                source::KeyCode::PageUp => KeyCode::PageUp,
                source::KeyCode::PageDown => KeyCode::PageDown,
                source::KeyCode::Insert => KeyCode::Insert,
                source::KeyCode::Delete => KeyCode::Delete,
                source::KeyCode::KeypadBegin => KeyCode::KeypadBegin,
                source::KeyCode::CapsLock => KeyCode::CapsLock,
                source::KeyCode::ScrollLock => KeyCode::ScrollLock,
                source::KeyCode::NumLock => KeyCode::NumLock,
                source::KeyCode::PrintScreen => KeyCode::PrintScreen,
                source::KeyCode::Pause => KeyCode::Pause,
                source::KeyCode::Menu => KeyCode::Menu,
                source::KeyCode::Null => KeyCode::Null,
                // These have no native app binding; their original sequence still reaches JS.
                source::KeyCode::Modifier(_) | source::KeyCode::Media(_) => return None,
            };
            Event::Key(KeyEvent {
                code,
                modifiers: native_modifiers(key.modifiers),
                kind: match key.kind {
                    source::KeyEventKind::Press => KeyEventKind::Press,
                    source::KeyEventKind::Repeat => KeyEventKind::Repeat,
                    source::KeyEventKind::Release => KeyEventKind::Release,
                },
                state: [
                    (source::KeyEventState::KEYPAD, KeyEventState::KEYPAD),
                    (source::KeyEventState::CAPS_LOCK, KeyEventState::CAPS_LOCK),
                    (source::KeyEventState::NUM_LOCK, KeyEventState::NUM_LOCK),
                ]
                .into_iter()
                .filter(|(source, _)| key.state.contains(*source))
                .fold(KeyEventState::NONE, |result, (_, target)| result | target),
            })
        }
        termina::Event::Mouse(mouse) => {
            use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
            let button = |value| match value {
                source::MouseButton::Left => MouseButton::Left,
                source::MouseButton::Right => MouseButton::Right,
                source::MouseButton::Middle => MouseButton::Middle,
            };
            Event::Mouse(MouseEvent {
                column: mouse.column,
                row: mouse.row,
                modifiers: native_modifiers(mouse.modifiers),
                kind: match mouse.kind {
                    source::MouseEventKind::Down(b) => MouseEventKind::Down(button(b)),
                    source::MouseEventKind::Up(b) => MouseEventKind::Up(button(b)),
                    source::MouseEventKind::Drag(b) => MouseEventKind::Drag(button(b)),
                    source::MouseEventKind::Moved => MouseEventKind::Moved,
                    source::MouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
                    source::MouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
                    source::MouseEventKind::ScrollLeft => MouseEventKind::ScrollLeft,
                    source::MouseEventKind::ScrollRight => MouseEventKind::ScrollRight,
                },
            })
        }
        termina::Event::Paste(text) => Event::Paste(text),
        termina::Event::FocusIn => Event::FocusGained,
        termina::Event::FocusOut => Event::FocusLost,
        _ => return None,
    })
}

#[cfg(unix)]
struct UnixInput {
    fd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    resize: tokio::signal::unix::Signal,
}

#[cfg(unix)]
#[derive(Default)]
pub(super) struct InputStream {
    source: Option<UnixInput>,
    decoder: InputDecoder,
    escape_deadline: Option<tokio::time::Instant>,
}

#[cfg(unix)]
impl InputStream {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) async fn next(&mut self) -> Option<io::Result<InputPacket>> {
        match self.read().await {
            Ok(packet) => packet.map(Ok),
            Err(error) => Some(Err(error)),
        }
    }

    async fn read(&mut self) -> io::Result<Option<InputPacket>> {
        use rustix::fs::{Mode, OFlags};
        if self.source.is_none() {
            // macOS cannot register the /dev/tty proxy with kqueue. Open the actual tty device
            // with an independent file description so O_NONBLOCK does not alter process stdin.
            let path = rustix::termios::ttyname(std::io::stdin(), Vec::new())?;
            let fd = rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            self.source = Some(UnixInput {
                fd: tokio::io::unix::AsyncFd::with_interest(fd, tokio::io::Interest::READABLE)
                    .map_err(|error| {
                        io::Error::new(error.kind(), format!("register terminal input: {error}"))
                    })?,
                resize: tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::window_change(),
                )?,
            });
        }
        let source = self.source.as_mut().unwrap();
        loop {
            if let Some(packet) = self.decoder.packets.pop_front() {
                return Ok(Some(packet));
            }
            let mut buffer = [0_u8; 8192];
            tokio::select! {
                ready = source.fd.readable() => {
                    let mut ready = ready?;
                    match ready.try_io(|fd| rustix::io::read(fd.get_ref(), &mut buffer).map_err(io::Error::from)) {
                        Ok(Ok(0)) => { self.decoder.flush(); return Ok(self.decoder.packets.pop_front()); }
                        Ok(Ok(count)) => {
                            self.decoder.push(&buffer[..count]);
                            self.escape_deadline = self.decoder.may_timeout().then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(escape_timeout_ms()));
                        }
                        Ok(Err(error)) => return Err(error),
                        Err(_) => continue,
                    }
                }
                _ = source.resize.recv() => {
                    let (columns, rows) = crossterm::terminal::size()?;
                    return Ok(Some(InputPacket { event: Some(Event::Resize(columns, rows)), data: None }));
                }
                _ = tokio::time::sleep_until(self.escape_deadline.unwrap_or_else(tokio::time::Instant::now)), if self.escape_deadline.is_some() => {
                    self.decoder.flush();
                    self.escape_deadline = None;
                }
            }
        }
    }
}

impl InputStream {
    pub(super) async fn next_native(&mut self) -> Option<io::Result<Event>> {
        loop {
            match self.next().await {
                Some(Ok(InputPacket {
                    event: Some(event), ..
                })) => return Some(Ok(event)),
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Some(Err(error)),
                None => return None,
            }
        }
    }
}

#[cfg(unix)]
fn escape_timeout_ms() -> u64 {
    std::env::var("PI_TUI_ESC_TIMEOUT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            if std::env::var_os("SSH_CONNECTION").is_some() {
                100
            } else {
                10
            }
        })
}

#[cfg(not(unix))]
pub(super) struct InputStream(crossterm::event::EventStream);

#[cfg(not(unix))]
impl InputStream {
    pub(super) fn new() -> Self {
        Self(crossterm::event::EventStream::new())
    }
    pub(super) async fn next(&mut self) -> Option<io::Result<InputPacket>> {
        use futures::StreamExt;
        self.0.next().await.map(|result| {
            result.map(|event| InputPacket {
                event: Some(event),
                data: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_original_kitty_sequences_and_native_release_semantics() {
        let mut decoder = InputDecoder::default();
        let data = "\x1b[97;5:3u";
        for part in [b"\x1b[97".as_slice(), b";5:", b"3u"] {
            decoder.push(part);
        }
        let packet = decoder.packets.pop_front().unwrap();
        assert_eq!(packet.data.as_deref(), Some(data));
        assert!(
            matches!(
                packet.event,
                Some(Event::Key(KeyEvent {
                    kind: KeyEventKind::Release,
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }))
            ),
            "{:?}",
            packet.event
        );

        let mouse = "\x1b[<0;2;9M";
        decoder.push(mouse.as_bytes());
        let packet = decoder.packets.pop_front().unwrap();
        assert_eq!(packet.data.as_deref(), Some(mouse));
        assert!(
            matches!(packet.event, Some(Event::Mouse(_))),
            "{:?}",
            packet.event
        );
    }

    #[test]
    fn paste_and_utf8_can_span_reads_without_losing_bytes_or_replaying_text() {
        let mut decoder = InputDecoder::default();
        let data = "\x1b[200~你好\nworld\x1b[201~x";
        for part in data.as_bytes().chunks(2) {
            decoder.push(part);
        }
        let paste = decoder.packets.pop_front().unwrap();
        assert_eq!(paste.data.as_deref(), Some("\x1b[200~你好\nworld\x1b[201~"));
        assert_eq!(paste.event, Some(Event::Paste("你好\nworld".into())));
        assert_eq!(
            decoder.packets.pop_front().unwrap().data.as_deref(),
            Some("x")
        );
        decoder.push(&[0xe7]);
        decoder.push(&[0x95]);
        decoder.push(&[0x8c]);
        assert_eq!(
            decoder.packets.pop_front().unwrap().data.as_deref(),
            Some("界")
        );
        assert!(decoder.packets.is_empty());
    }

    #[test]
    fn terminal_reports_do_not_capture_the_following_key() {
        let reports = [
            "\x1b[6;20;10t",
            "\x1b[?997;2n",
            "\x1b]4;0;rgb:0000/1111/2222\x07",
            "\x1b]4;1;rgb:3333/4444/5555\x1b\\",
            "\x1bP>|Pi TUI 1.0\x1b\\",
        ];
        // Exercise each reply next to Enter and all replies in a single read, with
        // every possible read boundary, including boundaries inside ST terminators.
        for replies in reports.chunks(1).chain(std::iter::once(reports.as_slice())) {
            let input = format!("{}\r", replies.concat());
            for split in 0..=input.len() {
                let mut decoder = InputDecoder::default();
                decoder.push(&input.as_bytes()[..split]);
                decoder.push(&input.as_bytes()[split..]);
                for report in replies {
                    let packet = decoder
                        .packets
                        .pop_front()
                        .expect("complete terminal report");
                    assert_eq!(packet.data.as_deref(), Some(*report));
                    assert!(packet.event.is_none());
                }
                let key = decoder.packets.pop_front().expect("following Enter key");
                assert_eq!(key.data.as_deref(), Some("\r"));
                assert_eq!(
                    key.event,
                    Some(Event::Key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE
                    )))
                );
                assert!(decoder.packets.is_empty());
            }
        }
    }

    #[test]
    fn legacy_mouse_and_function_keys_keep_their_trailing_payload() {
        let mut decoder = InputDecoder::default();
        // X10 coordinates are allowed to be CSI final bytes themselves.
        decoder.push(b"\x1b[M AA\x1b[[A");
        let mouse = decoder.packets.pop_front().unwrap();
        assert_eq!(mouse.data.as_deref(), Some("\x1b[M AA"));
        assert!(matches!(mouse.event, Some(Event::Mouse(_))));
        let function = decoder.packets.pop_front().unwrap();
        assert_eq!(function.data.as_deref(), Some("\x1b[[A"));
        assert_eq!(
            function.event,
            Some(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)))
        );
        assert!(decoder.packets.is_empty());
    }

    #[test]
    fn forwards_terminal_replies_and_unambiguous_escape_without_inventing_keys() {
        let mut decoder = InputDecoder::default();
        decoder.push(b"\x1b]11;rgb:0000/1111/2222\x07");
        let reply = decoder.packets.pop_front().unwrap();
        assert_eq!(
            reply.data.as_deref(),
            Some("\x1b]11;rgb:0000/1111/2222\x07")
        );
        assert!(reply.event.is_none());
        decoder.push(b"\x1b");
        decoder.flush();
        assert_eq!(
            decoder.packets.pop_front().unwrap().event,
            Some(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        );
    }
}
