//! Custom component rows are painted into Ratatui cells, never sent to the terminal as commands.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub(super) fn lines(rows: &[String]) -> Vec<Line<'static>> {
    let mut style = Style::default();
    rows.iter().map(|row| line(row, &mut style)).collect()
}

fn line(source: &str, style: &mut Style) -> Line<'static> {
    let mut spans = Vec::new();
    let mut text = String::new();
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\x1b' {
            if character == '\t' {
                text.push_str("    ");
            } else if !character.is_control() {
                text.push(character);
            }
            continue;
        }
        if !text.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut text), *style));
        }
        match chars.next() {
            Some('[') => {
                let mut parameters = String::new();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        if next == 'm' {
                            sgr(style, &parameters);
                        }
                        break;
                    }
                    parameters.push(next);
                }
            }
            Some(']' | '_' | 'P' | '^') => {
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, *style));
    }
    Line::from(spans)
}

fn sgr(style: &mut Style, parameters: &str) {
    let values: Vec<u16> = parameters
        .split(';')
        .map(|value| value.parse().unwrap_or(0))
        .collect();
    let mut index = 0;
    while index < values.len() {
        let value = values[index];
        match value {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            5 => *style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            8 => *style = style.add_modifier(Modifier::HIDDEN),
            9 => *style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            25 => *style = style.remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            28 => *style = style.remove_modifier(Modifier::HIDDEN),
            29 => *style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => *style = style.fg(Color::Indexed((value - 30) as u8)),
            40..=47 => *style = style.bg(Color::Indexed((value - 40) as u8)),
            90..=97 => *style = style.fg(Color::Indexed((value - 90 + 8) as u8)),
            100..=107 => *style = style.bg(Color::Indexed((value - 100 + 8) as u8)),
            39 => *style = style.fg(Color::Reset),
            49 => *style = style.bg(Color::Reset),
            38 | 48 => {
                let color = match values.get(index + 1..) {
                    Some([5, color, ..]) if *color <= 255 => {
                        index += 2;
                        Some(Color::Indexed(*color as u8))
                    }
                    Some([2, red, green, blue, ..])
                        if *red <= 255 && *green <= 255 && *blue <= 255 =>
                    {
                        index += 4;
                        Some(Color::Rgb(*red as u8, *green as u8, *blue as u8))
                    }
                    _ => None,
                };
                if let Some(color) = color {
                    *style = if value == 38 {
                        style.fg(color)
                    } else {
                        style.bg(color)
                    };
                }
            }
            _ => {}
        }
        index += 1;
    }
}

/// Encode already-decoded native keys for pi-tui's key parser. Paste remains one bracketed unit.
pub(super) fn input(event: &Event, wants_release: bool) -> Option<String> {
    let key = match event {
        Event::Paste(text) => return Some(format!("\x1b[200~{text}\x1b[201~")),
        Event::Key(key) if key.kind != KeyEventKind::Release || wants_release => key,
        _ => return None,
    };
    let modifiers = 1
        + u8::from(key.modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(key.modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(key.modifiers.contains(KeyModifiers::CONTROL))
        + 8 * u8::from(key.modifiers.contains(KeyModifiers::SUPER))
        + 16 * u8::from(key.modifiers.contains(KeyModifiers::HYPER))
        + 32 * u8::from(key.modifiers.contains(KeyModifiers::META));
    let event_type = match key.kind {
        KeyEventKind::Press => "",
        KeyEventKind::Repeat => ":2",
        KeyEventKind::Release => ":3",
    };
    let csi = |prefix: u16, suffix: char| {
        if modifiers == 1 && event_type.is_empty() {
            if prefix == 1 && suffix != '~' {
                format!("\x1b[{suffix}")
            } else {
                format!("\x1b[{prefix}{suffix}")
            }
        } else {
            format!("\x1b[{prefix};{modifiers}{event_type}{suffix}")
        }
    };
    let character = match key.code {
        KeyCode::Up => return Some(csi(1, 'A')),
        KeyCode::Down => return Some(csi(1, 'B')),
        KeyCode::Right => return Some(csi(1, 'C')),
        KeyCode::Left => return Some(csi(1, 'D')),
        KeyCode::Home => return Some(csi(1, 'H')),
        KeyCode::End => return Some(csi(1, 'F')),
        KeyCode::Insert => return Some(csi(2, '~')),
        KeyCode::Delete => return Some(csi(3, '~')),
        KeyCode::PageUp => return Some(csi(5, '~')),
        KeyCode::PageDown => return Some(csi(6, '~')),
        KeyCode::F(number @ 1..=4) => {
            let suffix = char::from(b'P' + number - 1);
            return Some(if modifiers == 1 && event_type.is_empty() {
                format!("\x1bO{suffix}")
            } else {
                csi(1, suffix)
            });
        }
        KeyCode::F(number @ 5..=12) => {
            return Some(csi(
                [15, 17, 18, 19, 20, 21, 23, 24][usize::from(number - 5)],
                '~',
            ));
        }
        KeyCode::BackTab => return Some("\x1b[Z".into()),
        KeyCode::Char(character) => character,
        KeyCode::Enter => '\r',
        KeyCode::Esc => '\x1b',
        KeyCode::Tab => '\t',
        KeyCode::Backspace => '\x7f',
        _ => return None,
    };
    if key.kind == KeyEventKind::Release
        || key
            .modifiers
            .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META)
        || (key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || !matches!(key.code, KeyCode::Char(_))))
    {
        return Some(format!(
            "\x1b[{};{modifiers}{event_type}u",
            u32::from(character)
        ));
    }
    let character = if key.modifiers.contains(KeyModifiers::CONTROL) && character.is_ascii() {
        match character {
            'a'..='z' | 'A'..='Z' | '['..='_' => char::from(character as u8 & 0x1f),
            ' ' | '@' => '\0',
            '?' => '\x7f',
            _ => {
                return Some(format!(
                    "\x1b[{};{modifiers}{event_type}u",
                    u32::from(character)
                ));
            }
        }
    } else {
        character
    };
    Some(if key.modifiers.contains(KeyModifiers::ALT) {
        format!("\x1b{character}")
    } else {
        character.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn custom_rows_preserve_styles_without_executing_control_sequences() {
        let rows =
            lines(&["\x1b[1;38;2;10;20;30m你好\x1b[0m plain\x1b]0;title\x07\x1b[2J!".into()]);
        assert_eq!(rows[0].to_string(), "你好 plain!");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Rgb(10, 20, 30)));
        assert!(rows[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!rows[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn custom_input_preserves_navigation_modifiers_unicode_and_atomic_paste() {
        let key =
            |code, modifiers| input(&Event::Key(KeyEvent::new(code, modifiers)), false).unwrap();
        assert_eq!(key(KeyCode::Esc, KeyModifiers::NONE), "\x1b");
        assert_eq!(key(KeyCode::Char('c'), KeyModifiers::CONTROL), "\x03");
        assert_eq!(key(KeyCode::Enter, KeyModifiers::SHIFT), "\x1b[13;2u");
        assert_eq!(
            key(
                KeyCode::Char('a'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ),
            "\x1b[97;6u"
        );
        assert_eq!(key(KeyCode::Down, KeyModifiers::SHIFT), "\x1b[1;2B");
        assert_eq!(key(KeyCode::Char('你'), KeyModifiers::NONE), "你");
        assert_eq!(
            input(&Event::Paste("你好\nworld".into()), false).unwrap(),
            "\x1b[200~你好\nworld\x1b[201~"
        );
        let released = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(input(&released, false), None);
        assert_eq!(input(&released, true).unwrap(), "\x1b[97;1:3u");
    }
}
