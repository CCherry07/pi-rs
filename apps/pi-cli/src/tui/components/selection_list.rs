use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use tui_realm_stdlib::components::List;
use tuirealm::command::{Cmd, Direction};
use tuirealm::component::Component;
use tuirealm::props::{AttrValue, Attribute, PropPayload, PropValue};
use tuirealm::state::{State, StateValue};

/// Selection and viewport state for a one-line-per-item selector.
///
/// The tui-realm component owns navigation and viewport behavior. Callers only
/// provide semantic rows and ask to move or render the selection.
pub(in crate::tui) struct SelectionList {
    component: List,
    rows: Vec<Line<'static>>,
}

impl Default for SelectionList {
    fn default() -> Self {
        Self {
            component: List::default()
                .scroll(true)
                .rewind(true)
                .always_active()
                .highlight_str(Line::default())
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            rows: Vec::new(),
        }
    }
}

impl SelectionList {
    pub(in crate::tui) fn set_highlight_style(&mut self, style: Style) {
        self.component
            .attr(Attribute::HighlightStyle, AttrValue::Style(style));
    }

    pub(in crate::tui) fn set_rows(&mut self, rows: Vec<Line<'static>>) {
        self.rows = rows;
        self.sync_component_rows();
    }

    fn sync_component_rows(&mut self) {
        let rows = self.rows.iter().cloned().map(PropValue::TextLine).collect();
        self.component
            .attr(Attribute::Text, AttrValue::Payload(PropPayload::Vec(rows)));
    }

    pub(in crate::tui) fn reconcile_len(&mut self, len: usize) {
        if self.rows.len() == len {
            return;
        }
        self.set_rows((0..len).map(|_| Line::default()).collect());
    }

    pub(in crate::tui) fn selected(&self) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        match self.component.state() {
            State::Single(StateValue::Usize(index)) => Some(index.min(self.rows.len() - 1)),
            _ => None,
        }
    }

    pub(in crate::tui) fn reset(&mut self) {
        self.select(0);
    }

    pub(in crate::tui) fn select(&mut self, index: usize) {
        self.component.attr(
            Attribute::Value,
            AttrValue::Payload(PropPayload::Single(PropValue::Usize(index))),
        );
    }

    pub(in crate::tui) fn previous(&mut self) {
        let _ = self.component.perform(Cmd::Move(Direction::Up));
    }

    pub(in crate::tui) fn next(&mut self) {
        let _ = self.component.perform(Cmd::Move(Direction::Down));
    }

    pub(in crate::tui) fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        const SCROLL_PADDING: usize = 2;

        if area.is_empty() {
            return;
        }
        let selected = self.selected().unwrap_or_default();
        let viewport_len = usize::from(area.height);
        let start = selected
            .saturating_sub(SCROLL_PADDING)
            .min(self.rows.len().saturating_sub(viewport_len));
        if start > 0 {
            let visible_rows = self
                .rows
                .iter()
                .skip(start)
                .take(viewport_len)
                .cloned()
                .map(PropValue::TextLine)
                .collect();
            self.component.attr(
                Attribute::Text,
                AttrValue::Payload(PropPayload::Vec(visible_rows)),
            );
            self.select(selected - start);
        }
        self.component.view(frame, area);
        if start > 0 {
            self.sync_component_rows();
            self.select(selected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SelectionList;

    #[test]
    fn selection_wraps_and_is_clamped_when_rows_change() {
        let mut list = SelectionList::default();
        list.reconcile_len(3);

        list.previous();
        assert_eq!(list.selected(), Some(2));
        list.next();
        assert_eq!(list.selected(), Some(0));

        list.select(2);
        list.reconcile_len(1);
        assert_eq!(list.selected(), Some(0));

        list.reconcile_len(0);
        assert_eq!(list.selected(), None);
    }
}
