//! [`BlockTree`](crate::parser::BlockTree) to Ratatui text.
//!
//! Geometry stays in the Ratatui caller. This adapter owns Markdown semantics,
//! block decoration, syntax paint, and light/dark theme choices, and returns
//! ordinary [`Line`] values that can be wrapped or virtualized by any widget.

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::highlight::{self, TokenClass};
use crate::parser::{self, AlertKind, Block, InlineRun, InlineStyle, ListItem, TableAlign};

const STATUS_DOT: &str = "•";
const CODE_BLOCK_HORIZONTAL_PADDING: &str = "  ";

/// Terminal color scheme used for Markdown paint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Appearance {
    Light,
    Dark,
}

/// Semantic Markdown theme shared by every rendered node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownTheme {
    appearance: Appearance,
}

impl MarkdownTheme {
    pub const fn new(appearance: Appearance) -> Self {
        Self { appearance }
    }

    pub const fn appearance(self) -> Appearance {
        self.appearance
    }

    /// Solid terminal equivalent of the original Pi `inset` surface.
    ///
    /// GPUI paints some palette entries with alpha over its canvas. Ratatui
    /// has no alpha channel, so those entries are pre-composited here; their
    /// semantic ownership and resulting color remain inside this crate.
    pub const fn code_background(self) -> Color {
        self.inset()
    }

    const fn text(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(29, 29, 31),
            Appearance::Dark => Color::Rgb(245, 245, 247),
        }
    }

    const fn secondary(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(81, 81, 84),
            Appearance::Dark => Color::Rgb(199, 199, 204),
        }
    }

    const fn tertiary(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(125, 125, 130),
            Appearance::Dark => Color::Rgb(142, 142, 147),
        }
    }

    const fn ghost(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(166, 166, 171),
            Appearance::Dark => Color::Rgb(99, 99, 102),
        }
    }

    const fn border(self) -> Color {
        match self.appearance {
            // Original: hsla(240°, 5%, 10%, .08) over #f7f7f9.
            Appearance::Light => Color::Rgb(229, 229, 231),
            // Original: white at .09 over #171719.
            Appearance::Dark => Color::Rgb(44, 44, 46),
        }
    }

    const fn inset(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(232, 232, 236),
            Appearance::Dark => Color::Rgb(16, 16, 17),
        }
    }

    const fn overlay(self) -> Color {
        match self.appearance {
            // Original: hsla(240°, 5%, 10%, .055) over #f7f7f9.
            Appearance::Light => Color::Rgb(235, 235, 237),
            // Original: white at .07 over #171719.
            Appearance::Dark => Color::Rgb(39, 39, 41),
        }
    }

    const fn inline_code_text(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(154, 74, 0),
            Appearance::Dark => Color::Rgb(255, 159, 10),
        }
    }

    const fn code_wash(self) -> Color {
        self.overlay()
    }

    const fn accent(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(0, 122, 255),
            Appearance::Dark => Color::Rgb(10, 132, 255),
        }
    }

    const fn added(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(36, 138, 61),
            Appearance::Dark => Color::Rgb(48, 209, 88),
        }
    }

    const fn removed(self) -> Color {
        match self.appearance {
            Appearance::Light => Color::Rgb(215, 0, 21),
            Appearance::Dark => Color::Rgb(255, 69, 58),
        }
    }

    fn alert(self, kind: AlertKind) -> Color {
        match kind {
            AlertKind::Note => self.accent(),
            AlertKind::Tip => self.added(),
            AlertKind::Important => self.token(TokenClass::Keyword),
            AlertKind::Warning => self.token(TokenClass::Literal),
            AlertKind::Caution => self.removed(),
        }
    }

    fn token(self, class: TokenClass) -> Color {
        match (self.appearance, class) {
            (Appearance::Light, TokenClass::Keyword) => Color::Rgb(154, 75, 146),
            (Appearance::Dark, TokenClass::Keyword) => Color::Rgb(201, 139, 192),
            (Appearance::Light, TokenClass::Literal | TokenClass::Number) => {
                Color::Rgb(154, 96, 25)
            }
            (Appearance::Dark, TokenClass::Literal | TokenClass::Number) => {
                Color::Rgb(217, 160, 91)
            }
            (Appearance::Light, TokenClass::String) => Color::Rgb(63, 122, 54),
            (Appearance::Dark, TokenClass::String) => Color::Rgb(148, 192, 138),
            (_, TokenClass::Comment) => self.ghost(),
            (Appearance::Light, TokenClass::Type | TokenClass::Function) => {
                Color::Rgb(47, 102, 144)
            }
            (Appearance::Dark, TokenClass::Type | TokenClass::Function) => {
                Color::Rgb(143, 184, 217)
            }
            (_, TokenClass::Meta) => self.tertiary(),
            (_, TokenClass::Added) => self.added(),
            (_, TokenClass::Removed) => self.removed(),
        }
    }

    fn body(self) -> Style {
        Style::default().fg(self.text())
    }

    fn heading(self, _level: u8) -> Style {
        // A terminal cannot reproduce the original size scale, so preserve its
        // weight hierarchy without inventing underline or italic semantics.
        self.body().add_modifier(Modifier::BOLD)
    }

    fn table_header(self) -> Style {
        self.body().bg(self.overlay()).add_modifier(Modifier::BOLD)
    }

    fn table_cell(self) -> Style {
        self.body()
    }

    fn table_border(self) -> Style {
        Style::default().fg(self.border())
    }
}

/// Result of rendering one Markdown document.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedMarkdown {
    pub lines: Vec<Line<'static>>,
}

/// Render Markdown through the copied Pi document model into Ratatui text.
///
/// When `streaming` is true, hanging inline delimiters are repaired only for
/// the display parse. The original source is never changed.
pub fn render(source: &str, streaming: bool, theme: MarkdownTheme) -> RenderedMarkdown {
    if source.is_empty() {
        return RenderedMarkdown::default();
    }
    let mended = streaming
        .then(|| crate::mend::close_hanging(source))
        .flatten();
    let tree = parser::parse(mended.as_deref().unwrap_or(source));
    let mut renderer = Renderer::new(theme);
    renderer.render_blocks(
        &tree
            .blocks
            .iter()
            .map(|block| &block.block)
            .collect::<Vec<_>>(),
    );
    RenderedMarkdown {
        lines: renderer.lines,
    }
}

struct Renderer {
    theme: MarkdownTheme,
    lines: Vec<Line<'static>>,
}

impl Renderer {
    fn new(theme: MarkdownTheme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
        }
    }

    fn render_blocks(&mut self, blocks: &[&Block]) {
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 && self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
                self.lines.push(Line::default());
            }
            self.lines.extend(self.render_block(block));
        }
    }

    fn render_block(&self, block: &Block) -> Vec<Line<'static>> {
        match block {
            Block::Paragraph { runs } => inline_lines(runs, self.theme.body(), self.theme),
            Block::Image { url, alt } => vec![image_line(url, alt, self.theme)],
            Block::Heading { level, runs } => {
                inline_lines(runs, self.theme.heading(*level), self.theme)
            }
            Block::CodeBlock { language, code } => self.render_code(language.as_deref(), code),
            Block::BlockQuote { kind, children } => self.render_quote(*kind, children),
            Block::List {
                ordered_start,
                items,
            } => self.render_list(*ordered_start, items),
            Block::Table {
                header,
                rows,
                align,
            } => self.render_table(header, rows, align),
            Block::Rule => vec![Line::from(Span::styled(
                "─".repeat(24),
                self.theme.table_border(),
            ))],
        }
    }

    fn render_quote(&self, kind: Option<AlertKind>, children: &[Block]) -> Vec<Line<'static>> {
        let mut nested = Renderer::new(self.theme);
        nested.render_blocks(&children.iter().collect::<Vec<_>>());
        let marker_style = if let Some(kind) = kind {
            let color = self.theme.alert(kind);
            let label = match kind {
                AlertKind::Note => "Note",
                AlertKind::Tip => "Tip",
                AlertKind::Important => "Important",
                AlertKind::Warning => "Warning",
                AlertKind::Caution => "Caution",
            };
            nested.lines.insert(
                0,
                Line::from(vec![
                    Span::styled(
                        format!("{STATUS_DOT} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        label,
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                ]),
            );
            Style::default().fg(color)
        } else {
            self.theme.table_border()
        };

        if nested.lines.is_empty() {
            nested.lines.push(Line::default());
        }
        for line in &mut nested.lines {
            line.style = self.theme.body().patch(line.style);
            line.spans.insert(0, Span::styled("│ ", marker_style));
        }
        nested.lines
    }

    fn render_list(&self, ordered_start: Option<u64>, items: &[ListItem]) -> Vec<Line<'static>> {
        let marker_width = ordered_start.map_or(2, |start| {
            let end = start.saturating_add(items.len().saturating_sub(1) as u64);
            end.to_string().len().saturating_add(2)
        });
        let mut lines = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let marker = match (ordered_start, item.task) {
                (_, Some(true)) => "[x] ".to_owned(),
                (_, Some(false)) => "[ ] ".to_owned(),
                (Some(start), None) => format!("{}. ", start.saturating_add(index as u64)),
                (None, None) => format!("{STATUS_DOT} "),
            };
            let prefix_width = marker_width.max(UnicodeWidthStr::width(marker.as_str()));
            let marker_color = match item.task {
                Some(true) => self.theme.accent(),
                Some(false) => self.theme.border(),
                None => self.theme.tertiary(),
            };
            let mut nested = Renderer::new(self.theme);
            nested.render_blocks(&item.blocks.iter().collect::<Vec<_>>());
            if nested.lines.is_empty() {
                nested.lines.push(Line::default());
            }
            for (line_index, mut line) in nested.lines.into_iter().enumerate() {
                let prefix = if line_index == 0 {
                    format!("{marker:<prefix_width$}")
                } else {
                    " ".repeat(prefix_width)
                };
                line.spans
                    .insert(0, Span::styled(prefix, Style::default().fg(marker_color)));
                lines.push(line);
            }
        }
        lines
    }

    fn render_code(&self, language: Option<&str>, code: &str) -> Vec<Line<'static>> {
        let background = self.theme.code_background();
        let mut lines = Vec::new();
        if let Some(language) = language.filter(|language| !language.is_empty()) {
            lines.push(
                Line::from(Span::styled(
                    format!(
                        "{CODE_BLOCK_HORIZONTAL_PADDING}{}{CODE_BLOCK_HORIZONTAL_PADDING}",
                        language.to_ascii_lowercase()
                    ),
                    Style::default().fg(self.theme.ghost()),
                ))
                .style(Style::default().bg(background)),
            );
        }
        let language = language.and_then(highlight::lang_for_tag);
        let highlighted = language.map(|language| highlight::tokenize(language, code));
        for (line_index, text) in code.split('\n').enumerate() {
            let tokens = highlighted
                .as_ref()
                .and_then(|lines| lines.get(line_index))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut spans = vec![Span::styled(
                CODE_BLOCK_HORIZONTAL_PADDING,
                Style::default().fg(self.theme.secondary()),
            )];
            let mut cursor = 0usize;
            for token in tokens {
                if cursor < token.range.start {
                    spans.push(Span::styled(
                        text[cursor..token.range.start].to_owned(),
                        Style::default().fg(self.theme.secondary()),
                    ));
                }
                spans.push(Span::styled(
                    text[token.range.clone()].to_owned(),
                    Style::default().fg(self.theme.token(token.class)),
                ));
                cursor = token.range.end;
            }
            if cursor < text.len() || spans.is_empty() {
                spans.push(Span::styled(
                    text[cursor..].to_owned(),
                    Style::default().fg(self.theme.secondary()),
                ));
            }
            spans.push(Span::styled(
                CODE_BLOCK_HORIZONTAL_PADDING,
                Style::default().fg(self.theme.secondary()),
            ));
            lines.push(Line::from(spans).style(Style::default().bg(background)));
        }
        lines
    }

    fn render_table(
        &self,
        header: &[Vec<InlineRun>],
        rows: &[Vec<Vec<InlineRun>>],
        align: &[TableAlign],
    ) -> Vec<Line<'static>> {
        let columns = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 {
            return Vec::new();
        }
        let mut widths = vec![1usize; columns];
        for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
            for (index, cell) in row.iter().enumerate() {
                let text = cell.iter().map(|run| run.text.as_str()).collect::<String>();
                widths[index] = widths[index].max(UnicodeWidthStr::width(text.as_str()));
            }
        }

        let mut lines = vec![table_border_line('┌', '┬', '┐', &widths, self.theme)];
        if !header.is_empty() {
            lines.push(table_row_line(
                header,
                &widths,
                align,
                self.theme.table_header(),
                self.theme,
            ));
        }
        if !header.is_empty() && !rows.is_empty() {
            lines.push(table_border_line('├', '┼', '┤', &widths, self.theme));
        }
        for (index, row) in rows.iter().enumerate() {
            lines.push(table_row_line(
                row,
                &widths,
                align,
                self.theme.table_cell(),
                self.theme,
            ));
            if index + 1 < rows.len() {
                lines.push(table_border_line('├', '┼', '┤', &widths, self.theme));
            }
        }
        lines.push(table_border_line('└', '┴', '┘', &widths, self.theme));
        lines
    }
}

fn inline_lines(runs: &[InlineRun], base: Style, theme: MarkdownTheme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    for run in runs {
        let style = inline_style(&run.style, base, theme);
        let mut parts = run.text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                spans.push(Span::styled(part.to_owned(), style));
            }
            if parts.peek().is_some() {
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
        }
    }
    lines.push(Line::from(spans));
    lines
}

fn inline_style(style: &InlineStyle, base: Style, theme: MarkdownTheme) -> Style {
    let mut result = base;
    if style.bold {
        result = result.add_modifier(Modifier::BOLD);
    }
    if style.italic {
        result = result.add_modifier(Modifier::ITALIC);
    }
    if style.strikethrough {
        result = result.add_modifier(Modifier::CROSSED_OUT);
    }
    if style.link.is_some() {
        result = result
            .underline_color(theme.tertiary())
            .add_modifier(Modifier::UNDERLINED);
    }
    if style.code {
        result = result.fg(theme.inline_code_text()).bg(theme.code_wash());
    }
    result
}

fn image_line(_url: &str, alt: &str, theme: MarkdownTheme) -> Line<'static> {
    let label = if alt.trim().is_empty() { "image" } else { alt };
    Line::from(Span::styled(
        format!("[image] {label}"),
        Style::default().fg(theme.ghost()),
    ))
}

fn table_border_line(
    left: char,
    junction: char,
    right: char,
    widths: &[usize],
    theme: MarkdownTheme,
) -> Line<'static> {
    let mut text = String::from(left);
    for (index, width) in widths.iter().enumerate() {
        text.push_str(&"─".repeat(width.saturating_add(2)));
        text.push(if index + 1 == widths.len() {
            right
        } else {
            junction
        });
    }
    Line::from(Span::styled(text, theme.table_border()))
}

fn table_row_line(
    cells: &[Vec<InlineRun>],
    widths: &[usize],
    align: &[TableAlign],
    base: Style,
    theme: MarkdownTheme,
) -> Line<'static> {
    let mut spans = vec![Span::styled("│", theme.table_border())];
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(Vec::as_slice).unwrap_or_default();
        let text = cell.iter().map(|run| run.text.as_str()).collect::<String>();
        let used = UnicodeWidthStr::width(text.as_str());
        let padding = width.saturating_sub(used);
        let (left, right) = match align.get(index).copied().unwrap_or_default() {
            TableAlign::Left => (0, padding),
            TableAlign::Center => (padding / 2, padding.saturating_sub(padding / 2)),
            TableAlign::Right => (padding, 0),
        };
        spans.push(Span::styled(" ".repeat(left.saturating_add(1)), base));
        for line in inline_lines(cell, base, theme) {
            spans.extend(line.spans);
        }
        spans.push(Span::styled(" ".repeat(right.saturating_add(1)), base));
        spans.push(Span::styled("│", theme.table_border()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_rich_blocks_without_source_markers() {
        let markdown = concat!(
            "# Heading\n\n",
            "Use **bold**, *italic*, `code`, and [docs](https://example.com).\n\n",
            "- first\n- second\n\n",
            "| Left | Right |\n| --- | ---: |\n| one | two |",
        );
        let rendered = render(markdown, false, MarkdownTheme::new(Appearance::Dark));
        let output = text(&rendered.lines);

        assert!(output.contains("Heading"));
        assert!(!output.contains("# Heading"));
        assert!(!output.contains("**bold**"));
        assert!(!output.contains("](https://"));
        assert!(output.contains("┌"));
        assert!(output.contains("┘"));
    }

    #[test]
    fn streaming_render_mends_hanging_inline_markers() {
        let rendered = render(
            "Writing **partial",
            true,
            MarkdownTheme::new(Appearance::Dark),
        );
        let partial = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "partial")
            .expect("partial span");

        assert!(partial.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn fenced_code_has_full_line_background_and_syntax_paint() {
        let theme = MarkdownTheme::new(Appearance::Light);
        let rendered = render("```rust\nfn main() {}\n```", false, theme);
        let code = rendered
            .lines
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .expect("code line");
        let keyword = code
            .spans
            .iter()
            .find(|span| span.content == "fn")
            .expect("keyword span");

        assert_eq!(code.style.bg, Some(theme.code_background()));
        assert_eq!(keyword.style.fg, Some(Color::Rgb(154, 75, 146)));
    }

    #[test]
    fn code_block_has_no_vertical_padding() {
        let theme = MarkdownTheme::new(Appearance::Dark);
        let rendered = render("```rust\nvalue\n```", false, theme);

        assert_eq!(rendered.lines.len(), 2);
        assert_eq!(rendered.lines[0].to_string().trim(), "rust");
        assert_eq!(rendered.lines[1].to_string().trim(), "value");
        assert!(
            rendered
                .lines
                .iter()
                .all(|line| !line.to_string().is_empty())
        );
    }

    #[test]
    fn code_language_label_and_code_have_two_column_horizontal_padding() {
        let rendered = render(
            "```rust\nvalue\n```",
            false,
            MarkdownTheme::new(Appearance::Dark),
        );
        let language = rendered
            .lines
            .iter()
            .find(|line| line.to_string().trim() == "rust")
            .expect("language label");
        let code = rendered
            .lines
            .iter()
            .find(|line| line.to_string().trim() == "value")
            .expect("code line");
        let horizontal_padding = |line: &Line<'_>| {
            let text = line.to_string();
            (
                text.chars()
                    .take_while(|character| character.is_whitespace())
                    .count(),
                text.chars()
                    .rev()
                    .take_while(|character| character.is_whitespace())
                    .count(),
            )
        };

        assert_eq!(horizontal_padding(language), (2, 2));
        assert_eq!(horizontal_padding(code), (2, 2));
    }

    #[test]
    fn gfm_alerts_use_a_colored_dot_instead_of_emoji() {
        let rendered = render(
            "> [!WARNING]\n> Check the migration.",
            false,
            MarkdownTheme::new(Appearance::Dark),
        );
        let output = text(&rendered.lines);

        assert!(output.contains("• Warning"));
        assert!(!output.contains("[!WARNING]"));
        assert!(!output.contains('⚠'));
    }

    #[test]
    fn ratatui_adapter_preserves_the_original_pi_markdown_palette() {
        let rendered = render(
            concat!(
                "# Heading\n\n",
                "Read [the docs](https://example.com) and use `code`.\n\n",
                "> quoted\n\n",
                "```rust\nfn main() {}\n```",
            ),
            false,
            MarkdownTheme::new(Appearance::Dark),
        );

        let heading = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "Heading")
            .expect("heading span");
        assert_eq!(heading.style.fg, Some(Color::Rgb(245, 245, 247)));
        assert!(heading.style.add_modifier.contains(Modifier::BOLD));
        assert!(!heading.style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!heading.style.add_modifier.contains(Modifier::ITALIC));

        let link = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "the docs")
            .expect("link span");
        assert_eq!(link.style.fg, Some(Color::Rgb(245, 245, 247)));
        assert_eq!(link.style.underline_color, Some(Color::Rgb(142, 142, 147)));
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));

        let inline_code = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "code")
            .expect("inline code span");
        assert_eq!(inline_code.style.fg, Some(Color::Rgb(255, 159, 10)));
        assert_eq!(inline_code.style.bg, Some(Color::Rgb(39, 39, 41)));

        let quote = rendered
            .lines
            .iter()
            .find(|line| line.to_string().contains("quoted"))
            .expect("quote line");
        assert_eq!(quote.spans[0].style.fg, Some(Color::Rgb(44, 44, 46)));
        assert_eq!(quote.spans[1].style.fg, Some(Color::Rgb(245, 245, 247)));
        assert!(!quote.spans[1].style.add_modifier.contains(Modifier::ITALIC));

        let code = rendered
            .lines
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .expect("code line");
        assert_eq!(code.style.bg, Some(Color::Rgb(16, 16, 17)));
        let keyword = code
            .spans
            .iter()
            .find(|span| span.content == "fn")
            .expect("keyword span");
        assert_eq!(keyword.style.fg, Some(Color::Rgb(201, 139, 192)));
    }

    #[test]
    fn light_adapter_uses_the_original_pi_code_surface_and_tokens() {
        let rendered = render(
            "```rust\nfn main() {}\n```",
            false,
            MarkdownTheme::new(Appearance::Light),
        );
        let code = rendered
            .lines
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .expect("code line");

        assert_eq!(code.style.bg, Some(Color::Rgb(232, 232, 236)));
        let keyword = code
            .spans
            .iter()
            .find(|span| span.content == "fn")
            .expect("keyword span");
        assert_eq!(keyword.style.fg, Some(Color::Rgb(154, 75, 146)));
    }
}
