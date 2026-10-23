//! Stateful shared Rust components. Descriptions never contain terminal coordinates or input bytes.
use std::collections::{HashMap, HashSet};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use pi_plugin::{UiCompletion, UiInteraction, UiNode, UiNodeKind, UiTone};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::json;

use super::super::{
    UiPalette,
    components::{ComposerInput, SelectionList},
    markdown_theme,
};

struct EditorState {
    multiline: bool,
    input: ComposerInput,
    completions: Vec<UiCompletion>,
    matches: Vec<usize>,
    suggestions: SelectionList,
}

impl EditorState {
    fn refresh_completions(&mut self) {
        let query = self.input.text().to_lowercase();
        let matches: Vec<_> = self
            .completions
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.value.to_lowercase() != query
                    && (item.value.to_lowercase().starts_with(&query)
                        || item.label.to_lowercase().starts_with(&query))
            })
            .map(|(index, _)| index)
            .collect();
        if self.matches != matches {
            self.suggestions.set_rows(
                matches
                    .iter()
                    .map(|index| {
                        let item = &self.completions[*index];
                        let label = if item.label.is_empty() {
                            &item.value
                        } else {
                            &item.label
                        };
                        Line::from(match &item.description {
                            Some(description) => format!("{label}  {description}"),
                            None => label.clone(),
                        })
                    })
                    .collect(),
            );
            self.suggestions.reset();
            self.matches = matches;
        }
    }

    fn accept_completion(&mut self) -> Option<String> {
        let index = *self.matches.get(self.suggestions.selected()?)?;
        let value = self.completions[index].value.clone();
        self.input.set_text(&value);
        self.refresh_completions();
        Some(value)
    }
}

struct SelectState {
    options: Vec<String>,
    list: SelectionList,
}

#[derive(Default)]
struct ScrollState {
    offset: u16,
    viewport: u16,
    content_height: u16,
}

impl ScrollState {
    fn move_by(&mut self, amount: i32) {
        self.offset = (i32::from(self.offset) + amount).clamp(
            0,
            i32::from(self.content_height.saturating_sub(self.viewport)),
        ) as u16;
    }
}

enum Control {
    Editor(Box<EditorState>),
    Select(Box<SelectState>),
    Button,
    Scroll(ScrollState),
}

#[derive(Default)]
pub(super) struct TreeRuntime {
    controls: HashMap<String, Control>,
    order: Vec<String>,
    focused: Option<String>,
    hit_areas: Vec<(String, Rect)>,
    scroll_areas: Vec<(String, Rect)>,
    paint_nodes: Vec<(String, Rect)>,
}

impl TreeRuntime {
    pub(super) fn reconcile(&mut self, root: &UiNode) {
        let mut present = HashSet::new();
        let mut order = Vec::new();
        self.reconcile_node(root, &mut present, &mut order);
        self.controls.retain(|id, _| present.contains(id));
        if self.focused.as_ref().is_none_or(|id| !present.contains(id)) {
            self.focused = order.first().cloned();
        }
        self.order = order;
        self.hit_areas.retain(|(id, _)| present.contains(id));
    }

    fn reconcile_node(
        &mut self,
        node: &UiNode,
        present: &mut HashSet<String>,
        order: &mut Vec<String>,
    ) {
        use UiNodeKind as N;
        match &node.kind {
            N::Column { children, .. } | N::Row { children, .. } => {
                for child in children {
                    self.reconcile_node(child, present, order);
                }
            }
            N::Panel { child, .. } => self.reconcile_node(child, present, order),
            N::Scroll { child, .. } => {
                self.reconcile_node(child, present, order);
                present.insert(node.id.clone());
                order.push(node.id.clone());
                let state = self
                    .controls
                    .entry(node.id.clone())
                    .or_insert_with(|| Control::Scroll(ScrollState::default()));
                if !matches!(state, Control::Scroll(_)) {
                    *state = Control::Scroll(ScrollState::default());
                }
            }
            N::Input {
                initial_value,
                placeholder,
                completions,
            }
            | N::Editor {
                initial_value,
                placeholder,
                completions,
                ..
            } => {
                present.insert(node.id.clone());
                order.push(node.id.clone());
                let multiline = matches!(node.kind, N::Editor { .. });
                let state = self.controls.entry(node.id.clone()).or_insert_with(|| {
                    Control::Editor(Box::new(EditorState {
                        multiline,
                        input: ComposerInput::from_text(initial_value),
                        completions: Vec::new(),
                        matches: Vec::new(),
                        suggestions: SelectionList::default(),
                    }))
                });
                if !matches!(state, Control::Editor(editor) if editor.multiline == multiline) {
                    *state = Control::Editor(Box::new(EditorState {
                        multiline,
                        input: ComposerInput::from_text(initial_value),
                        completions: Vec::new(),
                        matches: Vec::new(),
                        suggestions: SelectionList::default(),
                    }));
                }
                if let Control::Editor(state) = state {
                    state.input.editor.set_placeholder_text(placeholder.clone());
                    if state.completions != *completions {
                        state.completions.clone_from(completions);
                        state.matches.clear();
                        state.refresh_completions();
                    }
                }
            }
            N::Select {
                options,
                initial_selected,
            } => {
                present.insert(node.id.clone());
                order.push(node.id.clone());
                let make = || {
                    let mut list = SelectionList::default();
                    list.set_rows(options.iter().cloned().map(Line::from).collect());
                    list.select(*initial_selected);
                    Control::Select(Box::new(SelectState {
                        options: options.clone(),
                        list,
                    }))
                };
                let control = self.controls.entry(node.id.clone()).or_insert_with(make);
                if !matches!(control, Control::Select(_)) {
                    *control = make();
                }
                if let Control::Select(state) = control
                    && state.options != *options
                {
                    state.options.clone_from(options);
                    state
                        .list
                        .set_rows(options.iter().cloned().map(Line::from).collect());
                }
            }
            N::Button { .. } => {
                present.insert(node.id.clone());
                order.push(node.id.clone());
                self.controls.insert(node.id.clone(), Control::Button);
            }
            _ => {}
        }
    }

    pub(super) fn focus(&mut self, id: &str) -> bool {
        if self.controls.contains_key(id) {
            self.focused = Some(id.into());
            true
        } else {
            false
        }
    }

    pub(super) fn interactive(&self) -> bool {
        !self.order.is_empty()
    }

    pub(super) fn input(
        &mut self,
        root: &UiNode,
        event: &Event,
    ) -> (bool, Option<(String, UiInteraction)>) {
        if let Event::Mouse(mouse) = event {
            use crossterm::event::{MouseButton, MouseEventKind};
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) && let Some((id, _)) = self
                .scroll_areas
                .iter()
                .rev()
                .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                && let Some(Control::Scroll(state)) = self.controls.get_mut(id)
            {
                state.move_by(if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                });
                return (true, None);
            }
            let hit = self
                .hit_areas
                .iter()
                .rev()
                .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                .map(|(id, area)| (id.clone(), *area));
            if let Some((id, area)) = hit {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.focused = Some(id.clone());
                    match self.controls.get_mut(&id) {
                        Some(Control::Button) => {
                            return (true, Some((id, UiInteraction::Activate)));
                        }
                        Some(Control::Select(state)) => {
                            let start = state
                                .list
                                .selected()
                                .unwrap_or_default()
                                .saturating_sub(2)
                                .min(state.options.len().saturating_sub(usize::from(area.height)));
                            state
                                .list
                                .select(start + usize::from(mouse.row.saturating_sub(area.y)));
                            return (
                                true,
                                Some((
                                    id,
                                    UiInteraction::Change {
                                        value: json!(state.list.selected()),
                                    },
                                )),
                            );
                        }
                        _ => return (true, None),
                    }
                }
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) && let Some(Control::Select(state)) = self.controls.get_mut(&id)
                {
                    if mouse.kind == MouseEventKind::ScrollUp {
                        state.list.previous();
                    } else {
                        state.list.next();
                    }
                    return (
                        true,
                        Some((
                            id,
                            UiInteraction::Change {
                                value: json!(state.list.selected()),
                            },
                        )),
                    );
                }
            }
            return (false, None);
        }
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                return (false, None);
            }
            if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
                && let Some(focused) = &self.focused
                && let Some(id) = scroll_ancestor(root, focused)
                && let Some(Control::Scroll(state)) = self.controls.get_mut(id)
            {
                let amount = i32::from(state.viewport.max(1));
                state.move_by(if key.code == KeyCode::PageUp {
                    -amount
                } else {
                    amount
                });
                return (true, None);
            }
            if key.code == KeyCode::Tab
                && key.modifiers.is_empty()
                && let Some(id) = self.focused.clone()
                && let Some(Control::Editor(state)) = self.controls.get_mut(&id)
                && let Some(value) = state.accept_completion()
            {
                return (
                    true,
                    Some((
                        id,
                        UiInteraction::Change {
                            value: json!(value),
                        },
                    )),
                );
            }
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) && !self.order.is_empty() {
                let index = self
                    .order
                    .iter()
                    .position(|id| Some(id) == self.focused.as_ref())
                    .unwrap_or(0);
                let backwards =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                let next = if backwards {
                    (index + self.order.len() - 1) % self.order.len()
                } else {
                    (index + 1) % self.order.len()
                };
                self.focused = Some(self.order[next].clone());
                return (true, None);
            }
            if key.code == KeyCode::Esc {
                return (
                    true,
                    Some((
                        self.focused.clone().unwrap_or_else(|| root.id.clone()),
                        UiInteraction::Cancel,
                    )),
                );
            }
        }
        let Some(id) = self.focused.clone() else {
            return (false, None);
        };
        let Some(node) = find_node(root, &id) else {
            return (false, None);
        };
        let Some(control) = self.controls.get_mut(&id) else {
            return (false, None);
        };
        match control {
            Control::Editor(state) => {
                let multiline = matches!(node.kind, UiNodeKind::Editor { .. });
                let before = state.input.text();
                if !state.matches.is_empty()
                    && let Event::Key(key) = event
                {
                    match key.code {
                        KeyCode::Up => {
                            state.suggestions.previous();
                            return (true, None);
                        }
                        KeyCode::Down => {
                            state.suggestions.next();
                            return (true, None);
                        }
                        _ => {}
                    }
                }
                match event {
                    Event::Paste(text) => {
                        state.input.insert_str(if multiline {
                            text.clone()
                        } else {
                            text.replace(['\r', '\n'], " ")
                        });
                    }
                    Event::Key(key)
                        if key.code == KeyCode::Enter
                            && (!multiline || key.modifiers.contains(KeyModifiers::CONTROL)) =>
                    {
                        return (
                            true,
                            Some((
                                id,
                                UiInteraction::Submit {
                                    value: json!(before),
                                },
                            )),
                        );
                    }
                    Event::Key(key) => {
                        state.input.handle_key(*key);
                    }
                    _ => return (false, None),
                }
                let after = state.input.text();
                state.refresh_completions();
                (
                    true,
                    (before != after).then(|| {
                        (
                            id,
                            UiInteraction::Change {
                                value: json!(after),
                            },
                        )
                    }),
                )
            }
            Control::Select(state) => {
                let Event::Key(key) = event else {
                    return (false, None);
                };
                let before = state.list.selected();
                match key.code {
                    KeyCode::Up => state.list.previous(),
                    KeyCode::Down => state.list.next(),
                    KeyCode::Home => state.list.select(0),
                    KeyCode::End => state.list.select(state.options.len().saturating_sub(1)),
                    KeyCode::Enter => {
                        return (
                            true,
                            Some((
                                id,
                                UiInteraction::Submit {
                                    value: json!(before),
                                },
                            )),
                        );
                    }
                    _ => return (false, None),
                }
                let selected = state.list.selected();
                (
                    true,
                    (before != selected).then(|| {
                        (
                            id,
                            UiInteraction::Change {
                                value: json!(selected),
                            },
                        )
                    }),
                )
            }
            Control::Scroll(state) => {
                if let Event::Key(key) = event {
                    match key.code {
                        KeyCode::Up => state.move_by(-1),
                        KeyCode::Down => state.move_by(1),
                        KeyCode::Home => state.offset = 0,
                        KeyCode::End => {
                            state.offset = state.content_height.saturating_sub(state.viewport)
                        }
                        _ => return (false, None),
                    }
                    (true, None)
                } else {
                    (false, None)
                }
            }
            Control::Button => {
                if matches!(event, Event::Key(key) if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')))
                {
                    (true, Some((id, UiInteraction::Activate)))
                } else {
                    (false, None)
                }
            }
        }
    }

    pub(super) fn render(
        &mut self,
        node: &UiNode,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        palette: UiPalette,
        active: bool,
    ) {
        self.hit_areas.clear();
        self.scroll_areas.clear();
        self.paint_nodes.clear();
        self.draw(node, frame, area, palette, active);
    }

    pub(super) fn record_graphics(
        &self,
        root: &UiNode,
        clip: Rect,
        graphics: &mut super::super::extension_graphics::Graphics,
    ) {
        for (id, area) in &self.paint_nodes {
            if let Some(node) = find_node(root, id) {
                let visible = area.intersection(clip);
                if !visible.is_empty()
                    && (matches!(node.kind, UiNodeKind::Link { .. }) || *area == visible)
                {
                    paint_graphics(node, visible, graphics);
                }
            }
        }
    }

    fn draw(
        &mut self,
        node: &UiNode,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        palette: UiPalette,
        active: bool,
    ) {
        if area.is_empty() {
            return;
        }
        use UiNodeKind as N;
        match &node.kind {
            N::Column { children, gap } => {
                let mut y = area.y;
                for child in children {
                    let height =
                        measure(child, area.width, palette).min(area.bottom().saturating_sub(y));
                    self.draw(
                        child,
                        frame,
                        Rect::new(area.x, y, area.width, height),
                        palette,
                        active,
                    );
                    y = y
                        .saturating_add(height)
                        .saturating_add(*gap)
                        .min(area.bottom());
                }
            }
            N::Row { children, gap } => {
                let widths = row_widths(area.width, children.len(), *gap);
                let mut x = area.x;
                for (child, width) in children.iter().zip(widths) {
                    self.draw(
                        child,
                        frame,
                        Rect::new(x, area.y, width, area.height),
                        palette,
                        active,
                    );
                    x = x
                        .saturating_add(width)
                        .saturating_add(*gap)
                        .min(area.right());
                }
            }
            N::Panel { title, child } => {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(title.clone())
                    .border_style(Style::default().fg(palette.accent));
                let inner = block.inner(area);
                frame.render_widget(block, area);
                self.draw(child, frame, inner, palette, active);
            }
            N::Scroll { child, .. } => {
                use ratatui::{Terminal, backend::TestBackend};
                let content_height = measure(child, area.width, palette).max(1);
                let Some(Control::Scroll(state)) = self.controls.get_mut(&node.id) else {
                    return;
                };
                state.viewport = area.height;
                state.content_height = content_height;
                state.offset = state.offset.min(content_height.saturating_sub(area.height));
                let offset = state.offset;
                self.hit_areas.push((node.id.clone(), area));
                self.scroll_areas.push((node.id.clone(), area));
                let starts = (
                    self.hit_areas.len(),
                    self.scroll_areas.len(),
                    self.paint_nodes.len(),
                );
                let mut terminal = Terminal::new(TestBackend::new(area.width, content_height))
                    .expect("memory scroll viewport");
                terminal
                    .draw(|inner| self.draw(child, inner, inner.area(), palette, active))
                    .expect("memory scroll render");
                for row in 0..area.height.min(content_height.saturating_sub(offset)) {
                    for column in 0..area.width {
                        frame.buffer_mut()[(area.x + column, area.y + row)] =
                            terminal.backend().buffer()[(column, offset + row)].clone();
                    }
                }
                transform_areas(&mut self.hit_areas, starts.0, area, offset);
                transform_areas(&mut self.scroll_areas, starts.1, area, offset);
                let painted: Vec<_> = self
                    .paint_nodes
                    .drain(starts.2..)
                    .filter_map(|(id, rect)| {
                        let whole = find_node(child, &id)
                            .is_none_or(|node| !matches!(node.kind, N::Link { .. }));
                        scrolled_area(rect, area, offset, whole).map(|rect| (id, rect))
                    })
                    .collect();
                self.paint_nodes.extend(painted);
            }
            N::Input { .. } | N::Editor { .. } => {
                self.hit_areas.push((node.id.clone(), area));
                let focused = active && self.focused.as_ref() == Some(&node.id);
                if let Some(Control::Editor(state)) = self.controls.get_mut(&node.id) {
                    state.input.editor.set_cursor_style(if focused {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    });
                    let input_height = match &node.kind {
                        N::Editor { height, .. } => (*height).max(1),
                        _ => 1,
                    }
                    .min(area.height);
                    frame.render_widget(
                        state.input.widget(),
                        Rect::new(area.x, area.y, area.width, input_height),
                    );
                    if focused && !state.matches.is_empty() {
                        state.suggestions.set_highlight_style(
                            Style::default()
                                .fg(palette.accent)
                                .add_modifier(Modifier::BOLD),
                        );
                        state.suggestions.view(
                            frame,
                            Rect::new(
                                area.x,
                                area.y.saturating_add(input_height),
                                area.width,
                                area.height.saturating_sub(input_height),
                            ),
                        );
                    }
                }
            }
            N::Select { .. } => {
                self.hit_areas.push((node.id.clone(), area));
                if let Some(Control::Select(state)) = self.controls.get_mut(&node.id) {
                    state.list.set_highlight_style(
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::BOLD),
                    );
                    state.list.view(frame, area);
                }
            }
            N::Button { text } => {
                self.hit_areas.push((node.id.clone(), area));
                let focused = active && self.focused.as_ref() == Some(&node.id);
                let style = if focused {
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default().fg(palette.accent)
                };
                frame.render_widget(Paragraph::new(format!("[ {text} ]")).style(style), area);
            }
            _ => {
                let paint_area = match &node.kind {
                    N::Image { width, height, .. } => Some(Rect::new(
                        area.x,
                        area.y,
                        (*width).min(area.width),
                        (*height).min(area.height),
                    )),
                    N::Link { .. } => Some(area),
                    _ => None,
                };
                if let Some(area) = paint_area {
                    self.paint_nodes.push((node.id.clone(), area));
                }
                frame.render_widget(
                    Paragraph::new(lines(node, area.width, palette)).wrap(Wrap { trim: false }),
                    area,
                );
            }
        }
    }
}

fn scroll_ancestor<'a>(node: &'a UiNode, target: &str) -> Option<&'a str> {
    match &node.kind {
        UiNodeKind::Scroll { child, .. } => scroll_ancestor(child, target)
            .or_else(|| find_node(node, target).map(|_| node.id.as_str())),
        UiNodeKind::Column { children, .. } | UiNodeKind::Row { children, .. } => children
            .iter()
            .find_map(|child| scroll_ancestor(child, target)),
        UiNodeKind::Panel { child, .. } => scroll_ancestor(child, target),
        _ => None,
    }
}

fn scrolled_area(rect: Rect, viewport: Rect, offset: u16, whole: bool) -> Option<Rect> {
    let top = i32::from(rect.y) - i32::from(offset);
    let bottom = top + i32::from(rect.height);
    if bottom <= 0 || top >= i32::from(viewport.height) || rect.x >= viewport.width {
        return None;
    }
    if whole && (top < 0 || bottom > i32::from(viewport.height) || rect.right() > viewport.width) {
        return None;
    }
    let start = top.max(0) as u16;
    let end = bottom.min(i32::from(viewport.height)) as u16;
    Some(Rect::new(
        viewport.x.saturating_add(rect.x),
        viewport.y.saturating_add(start),
        rect.width.min(viewport.width - rect.x),
        end - start,
    ))
}

fn transform_areas(areas: &mut Vec<(String, Rect)>, start: usize, viewport: Rect, offset: u16) {
    let transformed: Vec<_> = areas
        .drain(start..)
        .filter_map(|(id, area)| {
            scrolled_area(area, viewport, offset, false).map(|area| (id, area))
        })
        .collect();
    areas.extend(transformed);
}

pub(super) fn find_node<'a>(node: &'a UiNode, id: &str) -> Option<&'a UiNode> {
    if node.id == id {
        return Some(node);
    }
    match &node.kind {
        UiNodeKind::Column { children, .. } | UiNodeKind::Row { children, .. } => {
            children.iter().find_map(|child| find_node(child, id))
        }
        UiNodeKind::Panel { child, .. } | UiNodeKind::Scroll { child, .. } => find_node(child, id),
        _ => None,
    }
}

fn row_widths(width: u16, count: usize, gap: u16) -> Vec<u16> {
    if count == 0 {
        return Vec::new();
    }
    let gaps = u16::try_from(count.saturating_sub(1))
        .unwrap_or(u16::MAX)
        .saturating_mul(gap);
    let available = usize::from(width.saturating_sub(gaps));
    (0..count)
        .map(|index| (available / count + usize::from(index < available % count)) as u16)
        .collect()
}

pub(super) fn measure(node: &UiNode, width: u16, palette: UiPalette) -> u16 {
    use UiNodeKind as N;
    let height = match &node.kind {
        N::Column { children, gap } => children
            .iter()
            .map(|child| usize::from(measure(child, width, palette)))
            .sum::<usize>()
            .saturating_add(children.len().saturating_sub(1) * usize::from(*gap)),
        N::Row { children, gap } => children
            .iter()
            .zip(row_widths(width, children.len(), *gap))
            .map(|(child, width)| usize::from(measure(child, width, palette)))
            .max()
            .unwrap_or(0),
        N::Panel { child, .. } => {
            usize::from(measure(child, width.saturating_sub(2), palette)).saturating_add(2)
        }
        N::Spacer { height } | N::Image { height, .. } | N::Scroll { height, .. } => {
            usize::from(*height)
        }
        N::Editor {
            height,
            completions,
            ..
        } => usize::from((*height).max(1)) + completions.len().min(5),
        N::Input { completions, .. } => 1 + completions.len().min(5),
        N::Button { .. } | N::Progress { .. } => 1,
        N::Select { options, .. } => options.len().max(1),
        _ => Paragraph::new(lines(node, width, palette))
            .wrap(Wrap { trim: false })
            .line_count(width.max(1)),
    };
    height.min(usize::from(u16::MAX)) as u16
}

fn tone_style(tone: UiTone, palette: UiPalette) -> Style {
    match tone {
        UiTone::Default => Style::default(),
        UiTone::Accent => Style::default().fg(palette.accent),
        UiTone::Muted => Style::default().fg(Color::DarkGray),
        UiTone::Success => Style::default().fg(Color::Green),
        UiTone::Warning => Style::default().fg(Color::Yellow),
        UiTone::Error => Style::default().fg(Color::Red),
    }
}

fn lines(node: &UiNode, _width: u16, palette: UiPalette) -> Vec<Line<'static>> {
    match &node.kind {
        UiNodeKind::Text { text, tone } => text
            .split('\n')
            .map(|line| Line::from(Span::styled(line.to_owned(), tone_style(*tone, palette))))
            .collect(),
        UiNodeKind::Link { text, .. } => vec![Line::from(Span::styled(
            text.clone(),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::UNDERLINED),
        ))],
        UiNodeKind::Markdown { text } => {
            crate::markdown::render(text, false, markdown_theme(palette.terminal_appearance)).lines
        }
        UiNodeKind::Spacer { height } => (0..*height).map(|_| Line::default()).collect(),
        UiNodeKind::Progress { text } => vec![Line::from(Span::styled(
            format!("◌ {text}"),
            Style::default().fg(palette.accent),
        ))],
        UiNodeKind::Image { alt, .. } => vec![Line::from(alt.clone())],
        _ => Vec::new(),
    }
}

/// Transcript rendering uses exactly the same component layout and paint implementation as slots.
pub(super) fn render_lines(root: &UiNode, width: u16, palette: UiPalette) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthStr;
    let (_runtime, terminal) = rasterize(root, width, palette);
    let width = width.max(1);
    let height = measure(root, width, palette).max(1);
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut spans = Vec::new();
            let mut x = 0;
            while x < width {
                let cell = &buffer[(x, y)];
                spans.push(Span::styled(cell.symbol().to_owned(), cell.style()));
                x =
                    x.saturating_add(
                        cell.symbol().width().max(1).min(usize::from(width - x)) as u16
                    );
            }
            Line::from(spans)
        })
        .collect()
}

fn rasterize(
    root: &UiNode,
    width: u16,
    palette: UiPalette,
) -> (
    TreeRuntime,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    use ratatui::{Terminal, backend::TestBackend};
    let width = width.max(1);
    let height = measure(root, width, palette).max(1);
    let mut runtime = TreeRuntime::default();
    runtime.reconcile(root);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("memory terminal");
    terminal
        .draw(|frame| runtime.render(root, frame, frame.area(), palette, false))
        .expect("memory terminal draw");
    (runtime, terminal)
}

/// Visits image content at the coordinates computed by the shared Rust layout. Cropping decisions
/// remain with the owner, which knows the final viewport and transcript scroll position.
pub(super) fn visit_paint_nodes(
    node: &UiNode,
    width: u16,
    palette: UiPalette,
    visit: &mut impl FnMut(&UiNode, Rect),
) {
    let (runtime, _terminal) = rasterize(node, width, palette);
    for (id, area) in &runtime.paint_nodes {
        if let Some(node) = find_node(node, id) {
            visit(node, *area);
        }
    }
}

pub(super) fn paint_graphics(
    node: &UiNode,
    area: Rect,
    graphics: &mut super::super::extension_graphics::Graphics,
) {
    match &node.kind {
        UiNodeKind::Image {
            data,
            mime_type,
            alt,
            ..
        } => {
            graphics.record_image(data, mime_type, alt, area);
        }
        UiNodeKind::Link { url, .. } => graphics.record_link(url, area),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn scrolling_clips_images_and_translates_links_with_the_same_paint_layout() {
        let root = UiNode::new(
            "scroll",
            UiNodeKind::Scroll {
                height: 2,
                child: Box::new(UiNode::new(
                    "column",
                    UiNodeKind::Column {
                        gap: 0,
                        children: vec![
                            UiNode::text("before", "before"),
                            UiNode::new(
                                "image",
                                UiNodeKind::Image {
                                    data: "encoded".into(),
                                    mime_type: "image/png".into(),
                                    alt: "image".into(),
                                    width: 4,
                                    height: 2,
                                },
                            ),
                            UiNode::new(
                                "link",
                                UiNodeKind::Link {
                                    text: "link".into(),
                                    url: "https://example.com".into(),
                                },
                            ),
                            UiNode::text("after", "after"),
                        ],
                    },
                )),
            },
        );
        let mut tree = TreeRuntime::default();
        tree.reconcile(&root);
        let palette = UiPalette::from_background(None);
        let mut terminal = Terminal::new(TestBackend::new(12, 8)).unwrap();
        let area = Rect::new(2, 3, 8, 2);
        terminal
            .draw(|frame| tree.render(&root, frame, area, palette, true))
            .unwrap();
        assert!(
            tree.paint_nodes.is_empty(),
            "a partly visible image cannot escape the scroll viewport"
        );
        tree.input(
            &root,
            &Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        terminal
            .draw(|frame| tree.render(&root, frame, area, palette, true))
            .unwrap();
        assert_eq!(
            tree.paint_nodes,
            vec![("image".into(), Rect::new(2, 3, 4, 2))]
        );
        tree.input(
            &root,
            &Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        terminal
            .draw(|frame| tree.render(&root, frame, area, palette, true))
            .unwrap();
        assert_eq!(
            tree.paint_nodes,
            vec![("link".into(), Rect::new(2, 4, 8, 1))]
        );
        assert_eq!(terminal.backend().buffer()[(2, 4)].symbol(), "l");
    }
}
