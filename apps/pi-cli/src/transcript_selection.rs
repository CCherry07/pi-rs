use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptSelection {
    anchor: Position,
    focus: Position,
    dragging: bool,
    dragged: bool,
}

impl TranscriptSelection {
    #[cfg(test)]
    pub(crate) const fn new(anchor: Position, focus: Position) -> Self {
        Self {
            anchor,
            focus,
            dragging: false,
            dragged: true,
        }
    }

    pub(crate) fn begin(surface: &TranscriptSurface, position: Position) -> Option<Self> {
        let anchor = surface.point(position)?;
        Some(Self {
            anchor,
            focus: anchor,
            dragging: true,
            dragged: false,
        })
    }

    pub(crate) fn drag_to(&mut self, surface: &TranscriptSurface, position: Position) {
        if !self.dragging {
            return;
        }
        if let Some(focus) = surface.clamped_point(position) {
            self.focus = focus;
            self.dragged = true;
        }
    }

    pub(crate) fn finish(&mut self, surface: &TranscriptSurface, position: Position) -> bool {
        if self.dragging
            && let Some(focus) = surface.clamped_point(position)
        {
            self.focus = focus;
        }
        self.dragging = false;
        self.dragged
    }

    fn bounds(self) -> (Position, Position) {
        if (self.anchor.y, self.anchor.x) <= (self.focus.y, self.focus.x) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfaceCell {
    symbol: String,
    width: u16,
}

impl Default for SurfaceCell {
    fn default() -> Self {
        Self {
            symbol: " ".to_string(),
            width: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptSurface {
    area: Rect,
    rows: Vec<Vec<SurfaceCell>>,
}

impl TranscriptSurface {
    pub(crate) fn capture(buffer: &Buffer, requested: Rect) -> Self {
        let buffer_area = *buffer.area();
        let left = requested.x.max(buffer_area.x);
        let top = requested.y.max(buffer_area.y);
        let right = requested.right().min(buffer_area.right());
        let bottom = requested.bottom().min(buffer_area.bottom());
        let area = Rect::new(
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        );
        let mut rows = Vec::with_capacity(usize::from(area.height));
        for y in area.y..area.bottom() {
            let mut row = vec![SurfaceCell::default(); usize::from(area.width)];
            let mut offset = 0u16;
            while offset < area.width {
                let x = area.x.saturating_add(offset);
                let symbol = buffer
                    .cell((x, y))
                    .map_or(" ", ratatui::buffer::Cell::symbol)
                    .to_string();
                let width = u16::try_from(UnicodeWidthStr::width(symbol.as_str()).max(1))
                    .unwrap_or(1)
                    .min(area.width.saturating_sub(offset));
                row[usize::from(offset)] = SurfaceCell { symbol, width };
                for continuation in 1..width {
                    row[usize::from(offset.saturating_add(continuation))] = SurfaceCell {
                        symbol: String::new(),
                        width: 0,
                    };
                }
                offset = offset.saturating_add(width);
            }
            rows.push(row);
        }
        Self { area, rows }
    }

    pub(crate) fn point(&self, position: Position) -> Option<Position> {
        (position.x >= self.area.x
            && position.x < self.area.right()
            && position.y >= self.area.y
            && position.y < self.area.bottom())
        .then(|| self.snap_to_lead(position))
    }

    pub(crate) fn clamped_point(&self, position: Position) -> Option<Position> {
        if self.area.is_empty() {
            return None;
        }
        let clamped = Position::new(
            position
                .x
                .clamp(self.area.x, self.area.right().saturating_sub(1)),
            position
                .y
                .clamp(self.area.y, self.area.bottom().saturating_sub(1)),
        );
        Some(self.snap_to_lead(clamped))
    }

    fn snap_to_lead(&self, position: Position) -> Position {
        let row = &self.rows[usize::from(position.y.saturating_sub(self.area.y))];
        let mut offset = position.x.saturating_sub(self.area.x);
        while offset > 0 && row[usize::from(offset)].width == 0 {
            offset = offset.saturating_sub(1);
        }
        Position::new(self.area.x.saturating_add(offset), position.y)
    }

    pub(crate) fn selected_text(&self, selection: TranscriptSelection) -> Option<String> {
        if self.area.is_empty() {
            return None;
        }
        let (start, end) = selection.bounds();
        if start.y < self.area.y
            || end.y >= self.area.bottom()
            || start.x < self.area.x
            || end.x >= self.area.right()
        {
            return None;
        }

        let mut selected = Vec::with_capacity(usize::from(end.y.saturating_sub(start.y)) + 1);
        for y in start.y..=end.y {
            let from = if y == start.y { start.x } else { self.area.x };
            let through = if y == end.y {
                end.x
            } else {
                self.area.right().saturating_sub(1)
            };
            let row = &self.rows[usize::from(y.saturating_sub(self.area.y))];
            let mut text = String::new();
            for x in from..=through {
                let cell = &row[usize::from(x.saturating_sub(self.area.x))];
                if cell.width > 0 {
                    text.push_str(&cell.symbol);
                }
            }
            selected.push(text.trim_end_matches(' ').to_string());
        }
        let text = selected.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    pub(crate) fn paint(
        &self,
        buffer: &mut Buffer,
        selection: TranscriptSelection,
        background: Color,
    ) {
        if self.area.is_empty() {
            return;
        }
        let (start, end) = selection.bounds();
        for y in start.y..=end.y {
            if y < self.area.y || y >= self.area.bottom() {
                continue;
            }
            let from = if y == start.y { start.x } else { self.area.x }.max(self.area.x);
            let mut through = if y == end.y {
                end.x
            } else {
                self.area.right().saturating_sub(1)
            }
            .min(self.area.right().saturating_sub(1));
            let row = &self.rows[usize::from(y.saturating_sub(self.area.y))];
            let end_cell = &row[usize::from(through.saturating_sub(self.area.x))];
            through = through
                .saturating_add(end_cell.width.saturating_sub(1))
                .min(self.area.right().saturating_sub(1));
            for x in from..=through {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_bg(background);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    #[test]
    fn visible_selection_copies_cross_line_unicode_without_phantom_cells() {
        let area = Rect::new(0, 0, 8, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "alpha", Style::default());
        buffer.set_string(0, 1, "中 beta", Style::default());
        let surface = TranscriptSurface::capture(&buffer, area);
        let selection = TranscriptSelection::new(Position::new(1, 0), Position::new(4, 1));

        assert_eq!(
            surface.selected_text(selection).as_deref(),
            Some("lpha\n中 be")
        );
    }

    #[test]
    fn drag_selection_supports_reverse_motion_and_ignores_plain_clicks() {
        let area = Rect::new(0, 0, 5, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "abcde", Style::default());
        buffer.set_string(0, 1, "中 xy", Style::default());
        let surface = TranscriptSurface::capture(&buffer, area);

        let mut selection =
            TranscriptSelection::begin(&surface, Position::new(4, 1)).expect("selection start");
        selection.drag_to(&surface, Position::new(1, 0));
        assert!(selection.finish(&surface, Position::new(1, 0)));
        assert_eq!(
            surface.selected_text(selection).as_deref(),
            Some("bcde\n中 xy")
        );

        let mut click =
            TranscriptSelection::begin(&surface, Position::new(2, 0)).expect("click start");
        assert!(!click.finish(&surface, Position::new(2, 0)));
    }

    #[test]
    fn selection_paint_marks_only_the_selected_terminal_cells() {
        let area = Rect::new(0, 0, 5, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "hello", Style::default());
        let surface = TranscriptSurface::capture(&buffer, area);
        let selection = TranscriptSelection::new(Position::new(1, 0), Position::new(3, 0));

        surface.paint(&mut buffer, selection, Color::Blue);

        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        for x in 1..=3 {
            assert_eq!(buffer[(x, 0)].bg, Color::Blue);
        }
        assert_eq!(buffer[(4, 0)].bg, Color::Reset);
    }
}
