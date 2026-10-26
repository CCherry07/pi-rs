//! Stateful shared Rust controls. Descriptions never contain terminal coordinates or input bytes.
use std::collections::{HashMap, HashSet};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use pi_plugin::{
    UiCompletion, UiInteraction, UiMultiSelectAction, UiMultiSelectOption, UiMultiSelectRequest,
    UiMultiSelectResponse, UiNode, UiNodeKind, UiTone,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};
use serde_json::json;

use super::super::super::{
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultiSelectFocus {
    List,
    Search,
    Filters,
}

struct MultiSelectState {
    source: UiMultiSelectRequest,
    title: String,
    options: Vec<UiMultiSelectOption>,
    actions: Vec<UiMultiSelectAction>,
    categories: Vec<(String, String)>,
    active_categories: HashSet<String>,
    pending_categories: Option<HashSet<String>>,
    sort_modes: Vec<(String, bool)>,
    sort_mode: usize,
    selected: HashSet<usize>,
    cursor: usize,
    filter_cursor: usize,
    query: String,
    focus: MultiSelectFocus,
    summary_lines: Vec<String>,
    pending_action: Option<String>,
}

impl MultiSelectState {
    fn new(request: &UiMultiSelectRequest) -> Self {
        let mut active_categories = request
            .initial_active_categories
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        if active_categories.is_empty() {
            active_categories.extend(request.categories.iter().map(|(id, _)| id.clone()));
        }
        Self {
            source: request.clone(),
            title: request.title.clone(),
            options: request.options.clone(),
            actions: request.actions.clone(),
            categories: request.categories.clone(),
            active_categories,
            pending_categories: None,
            sort_modes: request.sort_modes.clone(),
            sort_mode: request
                .initial_sort_mode
                .min(request.sort_modes.len().saturating_sub(1)),
            selected: request.initially_selected.iter().copied().collect(),
            cursor: 0,
            filter_cursor: 0,
            query: request.initial_query.clone(),
            focus: MultiSelectFocus::List,
            summary_lines: request.summary_lines.clone(),
            pending_action: None,
        }
    }

    fn reconfigure(&mut self, request: &UiMultiSelectRequest) {
        if self.source == *request {
            return;
        }
        self.source = request.clone();
        self.title.clone_from(&request.title);
        self.options.clone_from(&request.options);
        self.actions.clone_from(&request.actions);
        self.categories.clone_from(&request.categories);
        self.sort_modes.clone_from(&request.sort_modes);
        self.selected.retain(|index| *index < self.options.len());
        let valid_categories = self
            .categories
            .iter()
            .map(|(id, _)| id)
            .collect::<HashSet<_>>();
        self.active_categories
            .retain(|category| valid_categories.contains(category));
        if self.active_categories.is_empty() {
            self.active_categories
                .extend(self.categories.iter().map(|(id, _)| id.clone()));
        }
        self.sort_mode = self.sort_mode.min(self.sort_modes.len().saturating_sub(1));
        self.clamp_cursor();
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        let mut indices = self
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| {
                option
                    .category
                    .as_ref()
                    .is_none_or(|category| self.active_categories.contains(category))
                    && (query.is_empty() || fuzzy_matches(&option.search_text, &query))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if let Some((_, descending)) = self.sort_modes.get(self.sort_mode) {
            indices.sort_by(|left, right| {
                let left_option = &self.options[*left];
                let right_option = &self.options[*right];
                let left_values = left_option
                    .sort_values
                    .get(self.sort_mode)
                    .map_or(&[][..], Vec::as_slice);
                let right_values = right_option
                    .sort_values
                    .get(self.sort_mode)
                    .map_or(&[][..], Vec::as_slice);
                let primary = (0..left_values.len().max(right_values.len()))
                    .find_map(|key_index| {
                        let left_value = left_values.get(key_index).map_or("", String::as_str);
                        let right_value = right_values.get(key_index).map_or("", String::as_str);
                        let ordering = match (left_value.is_empty(), right_value.is_empty()) {
                            (true, false) => std::cmp::Ordering::Greater,
                            (false, true) => std::cmp::Ordering::Less,
                            _ if *descending => right_value.cmp(left_value),
                            _ => left_value.cmp(right_value),
                        };
                        (ordering != std::cmp::Ordering::Equal).then_some(ordering)
                    })
                    .unwrap_or(std::cmp::Ordering::Equal);
                primary
                    .then_with(|| {
                        category_position(&self.categories, left_option.category.as_deref()).cmp(
                            &category_position(&self.categories, right_option.category.as_deref()),
                        )
                    })
                    .then_with(|| {
                        left_option
                            .label
                            .to_lowercase()
                            .cmp(&right_option.label.to_lowercase())
                    })
            });
        }
        indices
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self
            .cursor
            .min(self.visible_indices().len().saturating_sub(1));
    }

    fn response(&self, action_id: String) -> UiInteraction {
        let mut selected = self.selected.iter().copied().collect::<Vec<_>>();
        selected.sort_unstable();
        let mut active_categories = self.active_categories.iter().cloned().collect::<Vec<_>>();
        active_categories.sort();
        UiInteraction::Submit {
            value: serde_json::to_value(UiMultiSelectResponse {
                selected,
                action_id,
                query: self.query.clone(),
                active_categories,
                sort_mode: self.sort_mode,
            })
            .expect("multi-selection response serializes"),
        }
    }

    fn input(&mut self, event: &Event) -> (bool, Option<UiInteraction>) {
        let Event::Key(key) = event else {
            return (false, None);
        };
        if key.kind == KeyEventKind::Release {
            return (false, None);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return (true, Some(UiInteraction::Cancel));
        }
        if let Some(pending_action) = self.pending_action.clone() {
            return match key.code {
                KeyCode::Char('y' | 'Y') => (true, Some(self.response(pending_action))),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.pending_action = None;
                    self.summary_lines = vec!["Action cancelled.".into()];
                    (true, None)
                }
                _ => (true, None),
            };
        }
        if key.code == KeyCode::Esc && self.focus != MultiSelectFocus::Filters {
            return (true, Some(UiInteraction::Cancel));
        }
        if self.focus == MultiSelectFocus::Filters {
            let draft = self
                .pending_categories
                .get_or_insert_with(|| self.active_categories.clone());
            match key.code {
                KeyCode::Esc => {
                    self.pending_categories = None;
                    self.focus = MultiSelectFocus::List;
                    self.summary_lines = vec!["Filter changes cancelled.".into()];
                }
                KeyCode::Up => self.filter_cursor = self.filter_cursor.saturating_sub(1),
                KeyCode::Down => {
                    self.filter_cursor =
                        (self.filter_cursor + 1).min(self.categories.len().saturating_sub(1));
                }
                KeyCode::Char(' ') => {
                    if let Some((id, _)) = self.categories.get(self.filter_cursor)
                        && !draft.remove(id)
                    {
                        draft.insert(id.clone());
                    }
                }
                KeyCode::Enter => {
                    let mut applied = self.pending_categories.take().unwrap_or_default();
                    let restored = applied.is_empty();
                    if restored {
                        applied.extend(self.categories.iter().map(|(id, _)| id.clone()));
                    }
                    self.active_categories = applied;
                    self.focus = MultiSelectFocus::List;
                    self.summary_lines = vec![if restored {
                        "All categories were disabled; restored all category filters.".into()
                    } else {
                        "Category filters applied.".into()
                    }];
                    self.clamp_cursor();
                }
                _ => {}
            }
            return (true, None);
        }
        if self.focus == MultiSelectFocus::Search {
            match key.code {
                KeyCode::Tab | KeyCode::Down | KeyCode::Enter => {
                    self.focus = MultiSelectFocus::List;
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.clamp_cursor();
                }
                KeyCode::Char(character)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.query.push(character);
                    self.clamp_cursor();
                }
                _ => {}
            }
            return (true, None);
        }

        let visible = self.visible_indices();
        let mut action_id = None;
        match key.code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(visible.len().saturating_sub(1));
            }
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::PageDown => {
                self.cursor = (self.cursor + 10).min(visible.len().saturating_sub(1));
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = visible.len().saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(index) = visible.get(self.cursor)
                    && !self.selected.remove(index)
                {
                    self.selected.insert(*index);
                }
            }
            KeyCode::Char('a') => {
                let count = visible.len();
                self.selected.extend(visible);
                self.summary_lines = vec![format!(
                    "Selected {count} visible item{}.",
                    if count == 1 { "" } else { "s" }
                )];
            }
            KeyCode::Char('n') => {
                self.selected.clear();
                self.summary_lines = vec!["Cleared all selections.".into()];
            }
            KeyCode::Char('s') if !self.sort_modes.is_empty() => {
                self.sort_mode = (self.sort_mode + 1) % self.sort_modes.len();
                self.clamp_cursor();
                self.summary_lines =
                    vec![format!("Sort mode: {}.", self.sort_modes[self.sort_mode].0)];
            }
            KeyCode::Char('f') if !self.categories.is_empty() => {
                self.pending_categories = Some(self.active_categories.clone());
                self.filter_cursor = 0;
                self.focus = MultiSelectFocus::Filters;
                self.summary_lines =
                    vec!["Filter panel open: space toggle · enter apply · esc cancel.".into()];
            }
            KeyCode::Tab | KeyCode::Char('/') => self.focus = MultiSelectFocus::Search,
            KeyCode::Char(character) => {
                if let Some(action) = self.actions.iter().find(|action| action.key == character) {
                    if !action.enabled {
                        self.summary_lines = vec![format!("{} is unavailable.", action.label)];
                    } else if self.selected.is_empty() {
                        self.summary_lines = vec!["Select one or more items first.".into()];
                    } else if let Some(template) = &action.confirmation {
                        let read_only_count = self
                            .selected
                            .iter()
                            .filter(|index| {
                                self.options.get(**index).is_some_and(|row| row.read_only)
                            })
                            .count();
                        let count = self.selected.len().saturating_sub(read_only_count);
                        if count == 0 {
                            action_id = Some(action.id.clone());
                        } else {
                            let read_only_note = if read_only_count == 0 {
                                String::new()
                            } else {
                                format!(
                                    " ({read_only_count} read-only item{} will be skipped)",
                                    if read_only_count == 1 { "" } else { "s" }
                                )
                            };
                            self.pending_action = Some(action.id.clone());
                            self.summary_lines = vec![
                                template
                                    .replace("{count}", &count.to_string())
                                    .replace("{plural}", if count == 1 { "" } else { "s" })
                                    .replace("{read_only_note}", &read_only_note),
                            ];
                        }
                    } else {
                        action_id = Some(action.id.clone());
                    }
                } else if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    self.focus = MultiSelectFocus::Search;
                    self.query.push(character);
                    self.clamp_cursor();
                }
            }
            _ => return (false, None),
        }
        (true, action_id.map(|id| self.response(id)))
    }
}

fn category_position(categories: &[(String, String)], category: Option<&str>) -> usize {
    category
        .and_then(|category| categories.iter().position(|(id, _)| id == category))
        .unwrap_or(categories.len())
}

fn fuzzy_matches(haystack: &str, query: &str) -> bool {
    let haystack = haystack.to_lowercase();
    if haystack.contains(query) {
        return true;
    }
    let mut characters = haystack.chars();
    query
        .chars()
        .all(|needle| characters.by_ref().any(|candidate| candidate == needle))
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
    MultiSelect(Box<MultiSelectState>),
    Button,
    Custom,
    Scroll(ScrollState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CustomMouseArea {
    pub(super) node: String,
    pub(super) instance: String,
    pub(super) bounds: Rect,
    pub(super) origin_x: i32,
    pub(super) origin_y: i32,
    pub(super) width: u16,
    pub(super) height: u16,
}

#[derive(Default)]
pub(super) struct TreeRuntime {
    controls: HashMap<String, Control>,
    order: Vec<String>,
    focused: Option<String>,
    hit_areas: Vec<(String, Rect)>,
    scroll_areas: Vec<(String, Rect)>,
    paint_nodes: Vec<(String, Rect)>,
    custom_mouse_areas: Vec<CustomMouseArea>,
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
        self.custom_mouse_areas.retain(|area| matches!(find_node(root, &area.node), Some(UiNode { kind: UiNodeKind::Custom { mouse: true, instance_id, .. }, .. }) if *instance_id == area.instance));
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
                    list.set_highlight_marker("› ");
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
            N::MultiSelect { request } => {
                present.insert(node.id.clone());
                order.push(node.id.clone());
                let make = || Control::MultiSelect(Box::new(MultiSelectState::new(request)));
                let control = self.controls.entry(node.id.clone()).or_insert_with(make);
                if !matches!(control, Control::MultiSelect(_)) {
                    *control = make();
                }
                if let Control::MultiSelect(state) = control {
                    state.reconfigure(request);
                }
            }
            N::Button { .. } => {
                present.insert(node.id.clone());
                order.push(node.id.clone());
                self.controls.insert(node.id.clone(), Control::Button);
            }
            N::Custom {
                interactive, mouse, ..
            } if *interactive || *mouse => {
                present.insert(node.id.clone());
                if *interactive {
                    order.push(node.id.clone());
                }
                self.controls.insert(node.id.clone(), Control::Custom);
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

    pub(super) fn custom_mouse_at(&self, column: u16, row: u16) -> Option<CustomMouseArea> {
        self.custom_mouse_areas
            .iter()
            .rev()
            .find(|area| area.bounds.contains((column, row).into()))
            .cloned()
    }

    pub(super) fn has_mouse_target(&self, target: &CustomMouseArea) -> bool {
        self.custom_mouse_areas
            .iter()
            .any(|area| area.node == target.node && area.instance == target.instance)
    }

    pub(super) fn input(
        &mut self,
        root: &UiNode,
        event: &Event,
    ) -> (bool, Option<(String, UiInteraction)>) {
        if let Some(id) = &self.focused
            && let Some(UiNode {
                kind: UiNodeKind::Custom { key_release, .. },
                ..
            }) = find_node(root, id)
            && let Some(data) = super::custom::input(event, *key_release)
        {
            return (
                true,
                Some((id.clone(), UiInteraction::CustomInput { data })),
            );
        }
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
                    // A mouse-capable custom node decides focus in its correlated reply.
                    if matches!(
                        find_node(root, &id),
                        Some(UiNode {
                            kind: UiNodeKind::Custom { mouse: true, .. },
                            ..
                        })
                    ) {
                        return (false, None);
                    }
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
            if let Some(id) = self.focused.clone()
                && let Some(Control::MultiSelect(state)) = self.controls.get_mut(&id)
            {
                let (handled, interaction) = state.input(event);
                return (handled, interaction.map(|interaction| (id, interaction)));
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
            Control::MultiSelect(_) => {
                unreachable!("multi-select input is handled before generic keys")
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
            Control::Custom => (false, None),
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
        self.custom_mouse_areas.clear();
        self.draw(node, frame, area, palette, active);
    }

    pub(super) fn record_graphics(
        &self,
        root: &UiNode,
        clip: Rect,
        graphics: &mut super::super::graphics::Graphics,
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
            N::Custom {
                lines,
                interactive,
                mouse,
                instance_id,
                cursor,
                ..
            } => {
                if *interactive || *mouse {
                    self.hit_areas.push((node.id.clone(), area));
                }
                if *mouse {
                    self.custom_mouse_areas.push(CustomMouseArea {
                        node: node.id.clone(),
                        instance: instance_id.clone(),
                        bounds: area,
                        origin_x: i32::from(area.x),
                        origin_y: i32::from(area.y),
                        width: area.width,
                        height: area.height,
                    });
                }
                frame.render_widget(Paragraph::new(super::custom::lines(lines)), area);
                if active
                    && self.focused.as_ref() == Some(&node.id)
                    && let Some((row, column)) = cursor
                    && *row < area.height
                    && *column < area.width
                {
                    let cell = &mut frame.buffer_mut()[(area.x + column, area.y + row)];
                    cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                }
            }
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
                    .border_type(BorderType::Rounded)
                    .padding(Padding::uniform(1))
                    .title(if title.is_empty() {
                        Line::default()
                    } else {
                        Line::from(Span::styled(
                            format!(" {title} "),
                            Style::default()
                                .fg(palette.accent)
                                .add_modifier(Modifier::BOLD),
                        ))
                    })
                    .border_style(Style::default().fg(Color::DarkGray));
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
                    self.custom_mouse_areas.len(),
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
                let mouse_areas = self
                    .custom_mouse_areas
                    .drain(starts.3..)
                    .filter_map(|mut target| {
                        let bounds = scrolled_area(target.bounds, area, offset, false)?;
                        target.origin_x += i32::from(area.x);
                        target.origin_y += i32::from(area.y) - i32::from(offset);
                        target.bounds = bounds;
                        Some(target)
                    })
                    .collect::<Vec<_>>();
                self.custom_mouse_areas.extend(mouse_areas);
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
                            .bg(palette.composer_background.unwrap_or(Color::Reset))
                            .add_modifier(Modifier::BOLD),
                    );
                    state.list.view(frame, area);
                }
            }
            N::MultiSelect { .. } => {
                self.hit_areas.push((node.id.clone(), area));
                if let Some(Control::MultiSelect(state)) = self.controls.get(&node.id) {
                    draw_multi_select(frame, area, state, palette);
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

fn multi_select_height(request: &UiMultiSelectRequest) -> usize {
    10usize
        .saturating_add(request.options.len().clamp(1, 14))
        .saturating_add(
            request
                .options
                .iter()
                .map(|option| option.detail_lines.len())
                .max()
                .unwrap_or_default(),
        )
        .saturating_add(request.summary_lines.len())
        .saturating_add(request.categories.len().saturating_add(2))
}

fn draw_multi_select(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &MultiSelectState,
    palette: UiPalette,
) {
    let visible = state.visible_indices();
    let sort_label = state
        .sort_modes
        .get(state.sort_mode)
        .map_or("default", |(label, _)| label.as_str());
    let focus_label = match state.focus {
        MultiSelectFocus::Search => "search",
        MultiSelectFocus::List => "list",
        MultiSelectFocus::Filters => "filters",
    };
    let mut lines = vec![
        Line::from(Span::styled(
            state.title.clone(),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(format!(
            "{focus_label}: {}{}",
            state.query,
            if state.focus == MultiSelectFocus::Search {
                "▏"
            } else {
                ""
            }
        )),
        Line::from(Span::styled(
            format!(
                "{} visible · {} total · {} selected · sort: {sort_label}",
                visible.len(),
                state.options.len(),
                state.selected.len()
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    if !state.categories.is_empty() {
        let categories = state
            .categories
            .iter()
            .filter(|(id, _)| state.active_categories.contains(id))
            .map(|(id, _)| format!("[{id}]"))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(Line::from(Span::styled(
            format!("filters: {categories}"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::default());

    let reserved_rows = 10usize
        .saturating_add(state.summary_lines.len())
        .saturating_add(if state.focus == MultiSelectFocus::Filters {
            state.categories.len().saturating_add(2)
        } else {
            0
        });
    let max_visible = usize::from(area.height)
        .saturating_sub(reserved_rows)
        .clamp(1, 14);
    let start = state
        .cursor
        .saturating_sub(max_visible / 2)
        .min(visible.len().saturating_sub(max_visible));
    let end = (start + max_visible).min(visible.len());
    if visible.is_empty() {
        lines.push(Line::from("No items match the current filters/search."));
    } else {
        for (position, option_index) in visible[start..end].iter().enumerate() {
            let absolute_position = start + position;
            let cursor = if absolute_position == state.cursor {
                "›"
            } else {
                " "
            };
            let check = if state.selected.contains(option_index) {
                "[x]"
            } else {
                "[ ]"
            };
            let style = if absolute_position == state.cursor {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("{cursor} {check} {}", state.options[*option_index].label),
                style,
            )));
        }
        if start > 0 || end < visible.len() {
            lines.push(Line::from(Span::styled(
                format!("Showing {}-{end} of {}", start + 1, visible.len()),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if let Some(option_index) = visible.get(state.cursor) {
            lines.push(Line::default());
            lines.extend(
                state.options[*option_index]
                    .detail_lines
                    .iter()
                    .map(|line| Line::from(line.clone())),
            );
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Last action",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.extend(state.summary_lines.iter().cloned().map(Line::from));

    if let Some(action_id) = &state.pending_action {
        let label = state
            .actions
            .iter()
            .find(|action| &action.id == action_id)
            .map_or(action_id.as_str(), |action| action.label.as_str());
        lines.extend([
            Line::default(),
            Line::from(Span::styled(
                format!("Confirm {label}: y yes · n no · esc cancel"),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
    } else if state.focus == MultiSelectFocus::Filters {
        lines.push(Line::default());
        let draft = state
            .pending_categories
            .as_ref()
            .unwrap_or(&state.active_categories);
        lines.extend(
            state
                .categories
                .iter()
                .enumerate()
                .map(|(index, (id, label))| {
                    let cursor = if index == state.filter_cursor {
                        "›"
                    } else {
                        " "
                    };
                    let check = if draft.contains(id) { "[x]" } else { "[ ]" };
                    Line::from(format!("{cursor} {check} {label}"))
                }),
        );
        lines.push(Line::from(Span::styled(
            "space toggle · enter apply · esc cancel",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let actions = state
            .actions
            .iter()
            .map(|action| {
                format!(
                    "{} {}{}",
                    action.key,
                    action.label,
                    if action.enabled { "" } else { " (disabled)" }
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        lines.extend([
            Line::default(),
            Line::from(Span::styled(
                "↑↓ move · space select · / search · s sort · f filters · a all · n none · esc close",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                actions,
                Style::default().fg(Color::DarkGray),
            )),
        ]);
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

pub(super) fn measure(node: &UiNode, width: u16, palette: UiPalette) -> u16 {
    use UiNodeKind as N;
    let height = match &node.kind {
        N::Custom { lines, .. } => lines.len(),
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
            usize::from(measure(child, width.saturating_sub(4), palette)).saturating_add(4)
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
        N::MultiSelect { request } => multi_select_height(request),
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
        UiNodeKind::Custom { lines, .. } => super::custom::lines(lines),
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
    graphics: &mut super::super::graphics::Graphics,
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
    fn custom_wire_content_clips_without_reflow_and_receives_focused_input() {
        let root: UiNode = serde_json::from_value(serde_json::json!({
            "id": "plugin",
            "type": "custom",
            "lines": ["\u{1b}[31mabcdefgh\u{1b}[0m", "你好"],
            "interactive": true,
            "cursor": [1, 2]
        }))
        .unwrap();
        let palette = UiPalette::from_background(None);
        let mut tree = TreeRuntime::default();
        tree.reconcile(&root);
        let mut terminal = Terminal::new(TestBackend::new(12, 5)).unwrap();
        let area = Rect::new(2, 1, 6, 2);
        terminal
            .draw(|frame| tree.render(&root, frame, area, palette, true))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(measure(&root, area.width, palette), 2);
        assert_eq!(buffer[(2, 1)].symbol(), "a");
        assert_eq!(buffer[(7, 1)].symbol(), "f");
        assert_eq!(buffer[(8, 1)].symbol(), " ");
        assert_eq!(buffer[(2, 2)].symbol(), "你");
        assert_eq!(buffer[(2, 1)].fg, Color::Indexed(1));
        assert!(buffer[(4, 2)].modifier.contains(Modifier::REVERSED));
        let (consumed, event) = tree.input(
            &root,
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(consumed);
        let (id, input) = event.unwrap();
        assert_eq!(id, "plugin");
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            serde_json::json!({ "type": "customInput", "data": "\u{1b}" })
        );
        assert_eq!(
            tree.input(&root, &Event::Paste("你好".into())).1.unwrap().1,
            UiInteraction::CustomInput {
                data: "\u{1b}[200~你好\u{1b}[201~".into()
            }
        );
    }

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
