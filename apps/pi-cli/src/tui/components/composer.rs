use super::super::*;

#[derive(Clone, Debug)]
pub(in crate::tui) struct ComposerInput {
    pub(in crate::tui) editor: TextArea<'static>,
}

impl Default for ComposerInput {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl ComposerInput {
    pub(in crate::tui) fn from_text(text: &str) -> Self {
        let lines = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
        let mut editor = TextArea::new(lines);
        editor.set_cursor_line_style(Style::default());
        editor.set_placeholder_text("Ask pi to do anything");
        editor.set_placeholder_style(Style::default().fg(Color::DarkGray));
        editor.set_wrap_mode(WrapMode::WordOrGlyph);
        editor.move_cursor(CursorMove::Bottom);
        editor.move_cursor(CursorMove::End);
        Self { editor }
    }

    pub(in crate::tui) fn text(&self) -> String {
        self.editor.lines().join("\n")
    }

    pub(in crate::tui) fn set_text(&mut self, text: impl AsRef<str>) {
        *self = Self::from_text(text.as_ref());
    }

    pub(in crate::tui) fn take_text(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }

    pub(in crate::tui) fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    pub(in crate::tui) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(in crate::tui) fn insert_newline(&mut self) {
        self.editor.insert_newline();
    }

    pub(in crate::tui) fn insert_str(&mut self, text: impl AsRef<str>) {
        self.editor.insert_str(text);
    }

    pub(in crate::tui) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.editor.delete_line_by_head(),
            (KeyCode::Char('-' | '_'), KeyModifiers::CONTROL) => self.editor.undo(),
            _ => self.editor.input(textarea_input(key)),
        }
    }

    pub(in crate::tui) fn widget(&self) -> &TextArea<'static> {
        &self.editor
    }
}

fn textarea_input(event: KeyEvent) -> TextAreaInput {
    let key = match event.code {
        KeyCode::Backspace => TextAreaKey::Backspace,
        KeyCode::Enter => TextAreaKey::Enter,
        KeyCode::Left => TextAreaKey::Left,
        KeyCode::Right => TextAreaKey::Right,
        KeyCode::Up => TextAreaKey::Up,
        KeyCode::Down => TextAreaKey::Down,
        KeyCode::Home => TextAreaKey::Home,
        KeyCode::End => TextAreaKey::End,
        KeyCode::PageUp => TextAreaKey::PageUp,
        KeyCode::PageDown => TextAreaKey::PageDown,
        KeyCode::Tab | KeyCode::BackTab => TextAreaKey::Tab,
        KeyCode::Delete => TextAreaKey::Delete,
        KeyCode::Insert => TextAreaKey::Null,
        KeyCode::F(number) => TextAreaKey::F(number),
        KeyCode::Char(character) => TextAreaKey::Char(character),
        KeyCode::Null | KeyCode::CapsLock | KeyCode::ScrollLock | KeyCode::NumLock => {
            TextAreaKey::Null
        }
        KeyCode::Esc => TextAreaKey::Esc,
        KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => TextAreaKey::Null,
    };
    TextAreaInput {
        key,
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: event.modifiers.contains(KeyModifiers::ALT),
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
    }
}
