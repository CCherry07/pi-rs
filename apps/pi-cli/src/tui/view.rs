use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct UiAreas {
    pub(super) transcript: Rect,
    pub(super) context: Rect,
    pub(super) composer: Rect,
    pub(super) footer: Rect,
    pub(super) gutter: u16,
}

pub(super) fn startup_header_height(width: u16, gutter: u16) -> u16 {
    let content_width = width.saturating_sub(gutter.saturating_mul(2));
    if content_width < 36 {
        2
    } else {
        STARTUP_HEADER_HEIGHT
    }
}

pub(super) fn cached_transcript_layout(
    app: &App,
    width: u16,
    gutter: u16,
    appearance: TerminalAppearance,
) -> Arc<CachedTranscriptLayout> {
    let show_working_placeholder =
        app.streaming_assistant.is_none() && (app.awaiting_assistant || app.status == "Working…");
    let key = TranscriptLayoutKey {
        width,
        gutter,
        appearance,
        working_elapsed_width: format_elapsed_compact(app.working_elapsed_seconds()).len(),
        tools_expanded: app.tools_expanded,
    };
    {
        let mut cache = app.transcript_layout_cache.borrow_mut();
        if cache.transcript != app.transcript
            || cache.show_startup_header != app.show_startup_header
            || cache.show_working_placeholder != show_working_placeholder
        {
            cache.transcript.clone_from(&app.transcript);
            cache.show_startup_header = app.show_startup_header;
            cache.show_working_placeholder = show_working_placeholder;
            cache.entries.clear();
        }
        if let Some(entry) = cache.entries.iter().find(|entry| entry.key == key) {
            return Arc::clone(&entry.layout);
        }
    }

    let working_placeholder = show_working_placeholder.then(|| TranscriptItem::Assistant {
        text: String::new(),
        streaming: true,
        error: None,
    });
    let startup_height = if app.show_startup_header {
        usize::from(startup_header_height(width, gutter))
    } else {
        0
    };
    let mut next_start = startup_height;
    let user_text_width = width.saturating_sub(3).max(1);
    let mut blocks = Vec::with_capacity(
        app.transcript
            .len()
            .saturating_add(usize::from(show_working_placeholder)),
    );
    for item in app.transcript.iter().chain(working_placeholder.iter()) {
        if next_start > 0 {
            next_start = next_start.saturating_add(1);
        }
        let is_user = matches!(item, TranscriptItem::User(_));
        let streaming = matches!(
            item,
            TranscriptItem::Assistant {
                streaming: true,
                ..
            }
        );
        let working = matches!(
            item,
            TranscriptItem::Assistant {
                text,
                streaming: true,
                ..
            } if text.is_empty()
        );
        let (user_text, lines, content_height, code_background_rows) =
            if let TranscriptItem::User(text) = item {
                let content_height = Paragraph::new(text.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(user_text_width)
                    .max(1);
                (Some(text.clone()), Vec::new(), content_height, Vec::new())
            } else {
                let lines = transcript_item_lines(
                    item,
                    appearance,
                    0,
                    app.tools_expanded,
                    app.working_elapsed_seconds(),
                );
                let code_background_rows = wrapped_line_background_ranges(
                    &lines,
                    width.max(1),
                    code_block_background(appearance),
                );
                let content_height = Paragraph::new(lines.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width.max(1))
                    .max(1);
                (None, lines, content_height, code_background_rows)
            };
        let height = content_height.saturating_add(usize::from(is_user) * 2);
        blocks.push(CachedTranscriptBlock {
            user_text,
            lines,
            streaming,
            working,
            start: next_start,
            content_height,
            height,
            code_background_rows,
        });
        next_start = next_start.saturating_add(height);
    }
    let line_count = next_start.saturating_add(usize::from(next_start > 0));
    let layout = Arc::new(CachedTranscriptLayout {
        blocks,
        startup_height,
        line_count,
    });
    let mut cache = app.transcript_layout_cache.borrow_mut();
    if cache.entries.len() >= 8 {
        cache.entries.remove(0);
    }
    cache.entries.push(TranscriptLayoutEntry {
        key,
        layout: Arc::clone(&layout),
    });
    layout
}

pub(super) fn desired_transcript_height(app: &App, width: u16, gutter: u16) -> u16 {
    let height = cached_transcript_layout(app, width, gutter, TerminalAppearance::Dark).line_count;
    u16::try_from(height).unwrap_or(u16::MAX)
}

pub(super) fn ui_areas(root: Rect, app: &App) -> UiAreas {
    if root.is_empty() {
        return UiAreas::default();
    }

    let gutter = horizontal_gutter(root.width);
    if let Some(view) = app.active_bottom_view() {
        let item_count = match view {
            BottomPaneView::Model => app.model_specs.len(),
            BottomPaneView::Thinking => app.thinking_choices().len(),
            BottomPaneView::Resume => app.session_choices.len(),
            BottomPaneView::Tree => app.tree_choices.len(),
            BottomPaneView::Fork => app.fork_choices.len(),
            BottomPaneView::Login => app.login_providers.len(),
            BottomPaneView::Logout => app.logout_providers.len(),
        };
        let desired_view_height = u16::try_from(item_count.clamp(1, 10))
            .unwrap_or(10)
            .saturating_add(5)
            .min(root.height);
        let transcript_height = desired_transcript_height(app, root.width, gutter)
            .min(root.height.saturating_sub(desired_view_height));
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_height),
                Constraint::Length(desired_view_height),
                Constraint::Min(0),
            ])
            .split(root);
        return UiAreas {
            transcript: areas[0],
            composer: areas[1],
            gutter,
            ..UiAreas::default()
        };
    }
    let content_width = root
        .width
        .saturating_sub(gutter.saturating_mul(2))
        .saturating_sub(4)
        .max(1);
    let input = app.input.text();
    let (cursor_row, _) = input_cursor_position(&input, content_width);
    let desired_composer_height = u16::try_from(cursor_row.saturating_add(1).clamp(1, 5))
        .unwrap_or(5)
        .saturating_add(2);
    let suggestions = suggestions_for_app(app);
    let queue_count = app.queue.steering.len() + app.queue.follow_up.len();
    let has_completion_panel = completion_panel_visible(app);
    let footer_height = u16::from(!has_completion_panel && root.height >= 5);
    let available_after_chrome = root.height.saturating_sub(footer_height);
    let composer_height = desired_composer_height
        .min(available_after_chrome.saturating_sub(1))
        .max(available_after_chrome.min(3));
    let context_count = if suggestions.is_empty() && has_completion_panel {
        1
    } else if suggestions.is_empty() {
        queue_count.min(5)
    } else {
        suggestions.len().min(8)
    };
    let desired_context_height = u16::try_from(context_count).unwrap_or(8);
    let context_budget = root
        .height
        .saturating_sub(footer_height)
        .saturating_sub(composer_height)
        .saturating_sub(1);
    let context_height = desired_context_height.min(context_budget);
    let transcript_budget = root
        .height
        .saturating_sub(footer_height)
        .saturating_sub(composer_height)
        .saturating_sub(context_height);
    let transcript_height =
        desired_transcript_height(app, root.width, gutter).min(transcript_budget);
    let areas = if has_completion_panel {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_height),
                Constraint::Length(composer_height),
                Constraint::Length(context_height),
                Constraint::Length(0),
                Constraint::Min(0),
            ])
            .split(root)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_height),
                Constraint::Length(context_height),
                Constraint::Length(composer_height),
                Constraint::Length(footer_height),
                Constraint::Min(0),
            ])
            .split(root)
    };

    if has_completion_panel {
        UiAreas {
            transcript: areas[0],
            composer: areas[1],
            context: areas[2],
            footer: areas[3],
            gutter,
        }
    } else {
        UiAreas {
            transcript: areas[0],
            context: areas[1],
            composer: areas[2],
            footer: areas[3],
            gutter,
        }
    }
}

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, app: &App, palette: UiPalette) {
    let root = frame.area();
    if root.is_empty() {
        return;
    }
    if let Some(prompt) = &app.trust_prompt {
        draw_project_trust_prompt(frame, &prompt.cwd, &prompt.options, prompt.selected);
        return;
    }
    let areas = ui_areas(root, app);
    let suggestions = suggestions_for_app(app);

    draw_transcript(frame, areas.transcript, app, areas.gutter, palette);
    if let Some(selection) = app.transcript_selection {
        let surface = TranscriptSurface::capture(frame.buffer_mut(), areas.transcript);
        surface.paint(
            frame.buffer_mut(),
            selection,
            selection_background(palette.terminal_appearance),
        );
    }
    if let Some(view) = app.active_bottom_view() {
        draw_bottom_pane_view(frame, areas.composer, app, view, palette);
        return;
    }
    draw_composer(frame, areas.composer, app, palette);
    if !areas.context.is_empty() {
        draw_context_panel(frame, areas.context, app, &suggestions, palette);
    }
    if !areas.footer.is_empty() {
        draw_footer(frame, areas.footer, app, palette);
    }
}

pub(super) fn draw_bottom_pane_view(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    view: BottomPaneView,
    palette: UiPalette,
) {
    if area.is_empty() {
        return;
    }
    let (title, subtitle, item_count) = match view {
        BottomPaneView::Model => (
            "Select Model and Effort",
            "Choose the model used for this session",
            app.model_specs.len(),
        ),
        BottomPaneView::Thinking => (
            "Select Thinking Level",
            "Choose reasoning depth for this session",
            app.thinking_choices().len(),
        ),
        BottomPaneView::Resume => (
            "Resume a session",
            "Choose a previous session to continue",
            app.session_choices.len(),
        ),
        BottomPaneView::Tree => (
            "Navigate session tree",
            "Choose a point in the current session",
            app.tree_choices.len(),
        ),
        BottomPaneView::Fork => (
            "Fork from a message",
            "Choose a user message to edit and continue from",
            app.fork_choices.len(),
        ),
        BottomPaneView::Login => (
            "Select provider to configure",
            "API keys use a hidden prompt; OAuth opens the browser",
            app.login_providers.len(),
        ),
        BottomPaneView::Logout => (
            "Select provider to log out",
            "Only credentials stored in auth.json are removed",
            app.logout_providers.len(),
        ),
    };
    let body_height = u16::try_from(item_count.clamp(1, 10))
        .unwrap_or(10)
        .min(area.height.saturating_sub(4));
    let [header, body, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(body_height),
            Constraint::Min(1),
        ])
        .areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("  {title}"),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  {subtitle}"),
                Style::default().fg(Color::DarkGray),
            )),
        ]),
        header,
    );

    let mut selection = app.view_selection.borrow_mut();
    selection.reconcile_len(item_count);
    selection.set_highlight_style(
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    let selected = selection.selected().unwrap_or_default();
    let rows: Vec<Line<'static>> = match view {
        BottomPaneView::Model => app
            .model_specs
            .iter()
            .enumerate()
            .map(|(index, model)| {
                let current =
                    model.provider.as_str() == app.provider && model.id.as_str() == app.model;
                picker_row(
                    index,
                    selected,
                    &format!(
                        "{}/{}{}",
                        model.provider,
                        model.id,
                        if current { " (current)" } else { "" }
                    ),
                    &model.name,
                    body.width,
                )
            })
            .collect(),
        BottomPaneView::Thinking => app
            .thinking_choices()
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                picker_row(
                    index,
                    selected,
                    &format!(
                        "{}{}",
                        choice.level.as_str(),
                        if choice.level.as_str() == app.thinking {
                            " (current)"
                        } else {
                            ""
                        }
                    ),
                    choice.description,
                    body.width,
                )
            })
            .collect(),
        BottomPaneView::Resume => app
            .session_choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let name = choice.name.as_deref().unwrap_or(&choice.first_message);
                let label = format!("{name}{}", if choice.current { " (current)" } else { "" });
                let description = format!(
                    "{} · {} · {}",
                    choice.id,
                    format_session_age(choice.modified_at_ms, unix_time_ms()),
                    compact_path(&choice.cwd.to_string_lossy())
                );
                picker_row(index, selected, &label, &description, body.width)
            })
            .collect(),
        BottomPaneView::Tree => app
            .tree_choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let description = if choice.current {
                    format!("current · {}", choice.description)
                } else {
                    choice.description.clone()
                };
                picker_row(index, selected, &choice.label, &description, body.width)
            })
            .collect(),
        BottomPaneView::Fork => app
            .fork_choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                picker_row(
                    index,
                    selected,
                    &choice.label,
                    &choice.description,
                    body.width,
                )
            })
            .collect(),
        BottomPaneView::Login => app
            .login_providers
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                let methods = if provider.supports_oauth {
                    "OAuth or API key"
                } else {
                    "API key"
                };
                let description = provider
                    .stored_kind
                    .map(|kind| format!("{methods} · {kind} configured"))
                    .unwrap_or_else(|| format!("{methods} · unconfigured"));
                picker_row(index, selected, &provider.id, &description, body.width)
            })
            .collect(),
        BottomPaneView::Logout => app
            .logout_providers
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                picker_row(
                    index,
                    selected,
                    &provider.id,
                    &format!("stored {}", provider.stored_kind.unwrap_or("credential")),
                    body.width,
                )
            })
            .collect(),
    };
    if rows.is_empty() {
        let message = match view {
            BottomPaneView::Model => "  No registered models",
            BottomPaneView::Thinking => "  No thinking levels available",
            BottomPaneView::Resume => "  No resumable sessions",
            BottomPaneView::Tree => "  No entries in session",
            BottomPaneView::Fork => "  No user messages to fork from",
            BottomPaneView::Login => "  No login providers available",
            BottomPaneView::Logout => "  No stored credentials to remove",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                message,
                Style::default().fg(Color::DarkGray),
            ))),
            body,
        );
    } else {
        selection.set_rows(rows);
        selection.view(frame, body);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  Press enter to confirm or esc to go back",
            Style::default().fg(Color::DarkGray),
        ))),
        Rect::new(footer.x, footer.bottom().saturating_sub(1), footer.width, 1),
    );
}

pub(super) fn picker_row(
    index: usize,
    selected: usize,
    label: &str,
    description: &str,
    width: u16,
) -> Line<'static> {
    let marker = if index == selected { "› " } else { "  " };
    let ordinal = format!("{}. ", index.saturating_add(1));
    let label_width = usize::from(width).saturating_sub(30).clamp(18, 44);
    let label = truncate_end(label, label_width);
    let padding = label_width.saturating_sub(UnicodeWidthStr::width(label.as_str()));
    Line::from(vec![
        Span::raw(marker),
        Span::raw(ordinal),
        Span::raw(label),
        Span::raw(" ".repeat(padding.saturating_add(2))),
        Span::styled(
            truncate_end(
                description,
                usize::from(width).saturating_sub(label_width.saturating_add(6)),
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

pub(super) fn draw_project_trust_prompt(
    frame: &mut ratatui::Frame<'_>,
    cwd: &Path,
    options: &[ProjectTrustOption],
    selected: usize,
) {
    let root = frame.area();
    frame.render_widget(Clear, root);
    let area = inset(root, horizontal_gutter(root.width).saturating_add(1));
    let mut lines = vec![
        Line::from(Span::styled(
            "Trust project folder?",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            cwd.display().to_string(),
            Style::default().fg(Color::DarkGray),
        )),
        Line::default(),
        Line::from(
            "This allows pi to load .pi settings and resources, install missing project packages, and execute project extensions.",
        ),
        Line::default(),
    ];
    for (index, option) in options.iter().enumerate() {
        let is_selected = index == selected.min(options.len().saturating_sub(1));
        lines.push(Line::from(vec![
            Span::styled(
                if is_selected { "› " } else { "  " },
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                option.label.clone(),
                if is_selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "↑↓ select · enter confirm · esc do not trust",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

pub(super) fn horizontal_gutter(width: u16) -> u16 {
    match width {
        0..=39 => 0,
        40..=79 => 1,
        _ => 2,
    }
}

pub(super) fn inset(area: Rect, horizontal: u16) -> Rect {
    area.inner(Margin::new(horizontal.min(area.width / 3), 0))
}

pub(super) fn draw_transcript(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    gutter: u16,
    palette: UiPalette,
) {
    if area.is_empty() {
        return;
    }
    let layout = cached_transcript_layout(app, area.width, gutter, palette.terminal_appearance);
    let startup_height = layout.startup_height;
    let line_count = layout.line_count;
    let content_area = area;
    let viewport = usize::from(area.height);
    let max_scroll = line_count.saturating_sub(viewport);
    let from_bottom = app.scroll_from_bottom.min(max_scroll);
    let scroll = max_scroll.saturating_sub(from_bottom);
    let viewport_end = scroll.saturating_add(viewport);

    if startup_height > 0 {
        draw_startup_header(
            frame,
            area,
            app,
            gutter,
            palette,
            startup_height,
            scroll,
            viewport_end,
        );
    }

    for block in &layout.blocks {
        let block_end = block.start.saturating_add(block.height);
        let visible_start = block.start.max(scroll);
        let visible_end = block_end.min(viewport_end);
        if visible_start >= visible_end {
            continue;
        }

        let visible_y = visible_start.saturating_sub(scroll);
        let screen_y = area
            .y
            .saturating_add(u16::try_from(visible_y).unwrap_or(u16::MAX));
        let visible_height = u16::try_from(visible_end.saturating_sub(visible_start))
            .unwrap_or(u16::MAX)
            .min(area.bottom().saturating_sub(screen_y));

        if let Some(text) = block.user_text.as_deref() {
            if let Some(background) = palette.composer_background {
                frame.render_widget(
                    Block::default().style(Style::default().bg(background)),
                    Rect::new(area.x, screen_y, area.width, visible_height),
                );
            }

            let content_start = block.start.saturating_add(1);
            let content_end = content_start.saturating_add(block.content_height);
            let content_visible_start = content_start.max(scroll);
            let content_visible_end = content_end.min(viewport_end);
            if content_visible_start < content_visible_end {
                let content_y = content_visible_start.saturating_sub(scroll);
                let content_screen_y = area
                    .y
                    .saturating_add(u16::try_from(content_y).unwrap_or(u16::MAX));
                let content_visible_height =
                    u16::try_from(content_visible_end.saturating_sub(content_visible_start))
                        .unwrap_or(u16::MAX)
                        .min(area.bottom().saturating_sub(content_screen_y));
                let content_scroll = content_visible_start.saturating_sub(content_start);
                draw_submitted_prompt(
                    frame,
                    Rect::new(area.x, content_screen_y, area.width, content_visible_height),
                    text,
                    content_scroll,
                );
            }
        } else {
            let content_scroll = visible_start.saturating_sub(block.start);
            let content_visible_end = content_scroll.saturating_add(usize::from(visible_height));
            for &(background_start, background_end) in &block.code_background_rows {
                let visible_background_start = background_start.max(content_scroll);
                let visible_background_end = background_end.min(content_visible_end);
                if visible_background_start >= visible_background_end {
                    continue;
                }
                let background_y = screen_y.saturating_add(
                    u16::try_from(visible_background_start.saturating_sub(content_scroll))
                        .unwrap_or(u16::MAX),
                );
                let background_height =
                    u16::try_from(visible_background_end.saturating_sub(visible_background_start))
                        .unwrap_or(u16::MAX)
                        .min(content_area.bottom().saturating_sub(background_y));
                frame.render_widget(
                    Block::default().style(
                        Style::default().bg(code_block_background(palette.terminal_appearance)),
                    ),
                    Rect::new(
                        content_area.x,
                        background_y,
                        content_area.width,
                        background_height,
                    ),
                );
            }
            let mut lines = if block.working {
                vec![working_status_line(
                    palette.terminal_appearance,
                    app.animation_frame,
                    app.working_elapsed_seconds(),
                )]
            } else {
                block.lines.clone()
            };
            if block.streaming
                && !block.working
                && let Some(indicator) = lines.first_mut().and_then(|line| line.spans.first_mut())
            {
                indicator.style = indicator.style.fg(activity_indicator_color(
                    palette.terminal_appearance,
                    app.animation_frame,
                ));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((u16::try_from(content_scroll).unwrap_or(u16::MAX), 0)),
                Rect::new(content_area.x, screen_y, content_area.width, visible_height),
            );
        }
    }

    if line_count > viewport && area.width > 2 {
        let mut state = ScrollbarState::new(max_scroll.saturating_add(1))
            .position(scroll)
            .viewport_content_length(viewport);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("▐")
                .thumb_style(Style::default().fg(Color::DarkGray))
                .track_symbol(None)
                .begin_symbol(None)
                .end_symbol(None),
            area,
            &mut state,
        );
    }
}

pub(super) fn wrapped_line_background_ranges(
    lines: &[Line<'_>],
    width: u16,
    background: Color,
) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut row = 0usize;
    for line in lines {
        let height = Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(width.max(1))
            .max(1);
        let end = row.saturating_add(height);
        if line.style.bg == Some(background) {
            if let Some((_, previous_end)) = ranges.last_mut()
                && *previous_end == row
            {
                *previous_end = end;
            } else {
                ranges.push((row, end));
            }
        }
        row = end;
    }
    ranges
}

pub(super) fn draw_submitted_prompt(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    text: &str,
    scroll: usize,
) {
    if area.is_empty() {
        return;
    }
    if scroll == 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "› ",
                Style::default().add_modifier(Modifier::BOLD),
            ))),
            Rect::new(area.x, area.y, area.width.min(2), 1),
        );
    }
    frame.render_widget(
        Paragraph::new(text.to_string())
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        Rect::new(
            area.x.saturating_add(2),
            area.y,
            area.width.saturating_sub(3),
            area.height,
        ),
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_startup_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    gutter: u16,
    palette: UiPalette,
    height: usize,
    scroll: usize,
    viewport_end: usize,
) {
    let visible_start = scroll.min(height);
    let visible_end = viewport_end.min(height);
    if visible_start >= visible_end {
        return;
    }

    let height = u16::try_from(height).unwrap_or(u16::MAX);
    let scratch_area = Rect::new(0, 0, area.width, height);
    let mut scratch = Buffer::empty(scratch_area);
    render_startup_header(&mut scratch, inset(scratch_area, gutter), app, palette);

    for source_y in visible_start..visible_end {
        let target_y = area
            .y
            .saturating_add(u16::try_from(source_y.saturating_sub(scroll)).unwrap_or(u16::MAX));
        let source_y = u16::try_from(source_y).unwrap_or(u16::MAX);
        for x in 0..area.width {
            frame.buffer_mut()[(area.x.saturating_add(x), target_y)] =
                scratch[(x, source_y)].clone();
        }
    }
}

pub(super) fn render_startup_header(
    buffer: &mut Buffer,
    area: Rect,
    app: &App,
    palette: UiPalette,
) {
    let width = area.width;
    let top_margin = u16::from(area.height >= 7);
    let height = area.height.saturating_sub(top_margin).min(6);
    if width < 36 || height < 6 {
        Widget::render(
            Paragraph::new(Line::from(vec![
                Span::styled(">_ ", Style::default().fg(Color::DarkGray)),
                Span::styled("pi", Style::default().add_modifier(Modifier::BOLD)),
            ])),
            area,
            buffer,
        );
        return;
    }
    let card_width = width.min(54);
    let card = Rect::new(
        area.x,
        area.y.saturating_add(top_margin),
        card_width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_width = usize::from(card_width.saturating_sub(2));
    let directory = truncate_start(&compact_path(&app.cwd), inner_width.saturating_sub(12));
    let mut model = app.model.clone();
    if app.thinking != "off" {
        model.push(' ');
        model.push_str(&app.thinking);
    }
    let model = truncate_end(&model, inner_width.saturating_sub(30));
    let text = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(">_ ", Style::default().fg(Color::DarkGray)),
            Span::styled("pi", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" (v{})", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::default(),
        Line::from(vec![
            Span::styled(" model:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(model),
            Span::raw("  "),
            Span::styled("/model", Style::default().fg(palette.accent)),
            Span::styled(" to change", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled(" directory:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(directory),
        ]),
    ];
    Widget::render(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        card,
        buffer,
    );
    let tip_y = card.bottom().saturating_add(1);
    if tip_y < area.bottom() {
        Widget::render(
            Paragraph::new(Line::from(vec![
                Span::styled("Tip: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("type "),
                Span::styled("/", Style::default().fg(palette.accent)),
                Span::raw(" for commands or "),
                Span::styled("!", Style::default().fg(Color::Magenta)),
                Span::raw(" for shell"),
            ])),
            Rect::new(
                area.x.saturating_add(1),
                tip_y,
                area.width.saturating_sub(1),
                1,
            ),
            buffer,
        );
    }
}

pub(super) fn draw_context_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    suggestions: &[CommandSuggestion],
    palette: UiPalette,
) {
    let command_query = command_query(&app.input.text()).is_some();
    let model_query = model_query(&app.input.text()).is_some();
    let resume_query = resume_query(&app.input.text()).is_some();
    let lines = if suggestions.is_empty() && model_query {
        vec![Line::from(Span::styled(
            if app.model_specs.is_empty() {
                "  No registered models"
            } else {
                "  No matching models"
            },
            Style::default().fg(Color::DarkGray),
        ))]
    } else if suggestions.is_empty() && resume_query {
        vec![Line::from(Span::styled(
            if app.session_choices.is_empty() {
                "  No resumable sessions"
            } else {
                "  No matching sessions"
            },
            Style::default().fg(Color::DarkGray),
        ))]
    } else if suggestions.is_empty() && command_query {
        vec![Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(Color::DarkGray),
        ))]
    } else if suggestions.is_empty() {
        app.queue
            .steering
            .iter()
            .map(|text| {
                Line::from(vec![
                    Span::styled("  ↗  ", Style::default().fg(Color::DarkGray)),
                    Span::raw(text.clone()),
                    Span::styled("  steering", Style::default().fg(Color::DarkGray)),
                ])
            })
            .chain(app.queue.follow_up.iter().map(|text| {
                Line::from(vec![
                    Span::styled("  ↳  ", Style::default().fg(Color::DarkGray)),
                    Span::raw(text.clone()),
                    Span::styled("  queued", Style::default().fg(Color::DarkGray)),
                ])
            }))
            .take(5)
            .collect::<Vec<_>>()
    } else {
        draw_command_palette(
            frame,
            area,
            &mut app.command_palette.borrow_mut(),
            suggestions,
            palette,
        );
        return;
    };
    frame.render_widget(Paragraph::new(lines), area);
}

pub(super) fn draw_command_palette(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    selection: &mut SelectionList,
    suggestions: &[CommandSuggestion],
    palette: UiPalette,
) {
    let resume_selector = suggestions
        .first()
        .is_some_and(|suggestion| suggestion.invocation.starts_with("/resume "));
    let skill_selector = suggestions
        .iter()
        .any(|suggestion| suggestion.invocation.starts_with("/skill:"));
    let area_width = usize::from(area.width);
    let label_width = if resume_selector {
        area_width.saturating_sub(34).clamp(20, 56)
    } else if skill_selector {
        suggestions
            .iter()
            .map(|suggestion| {
                UnicodeWidthStr::width(
                    suggestion
                        .label
                        .as_deref()
                        .unwrap_or(&suggestion.invocation),
                )
            })
            .max()
            .unwrap_or(20)
            .max(20)
            .min(area_width.saturating_sub(2))
    } else {
        20
    };
    let description_width = area_width.saturating_sub(label_width.saturating_add(2));
    selection.reconcile_len(suggestions.len());
    selection.set_highlight_style(
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    let selected_index = selection.selected().unwrap_or_default();
    let rows = suggestions
        .iter()
        .enumerate()
        .map(|(index, suggestion)| {
            let hint = suggestion
                .argument_hint
                .as_deref()
                .map_or_else(String::new, |hint| format!(" {hint}"));
            let selected = index == selected_index;
            let label_style = if selected {
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let description_style = if selected {
                label_style
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let label = suggestion
                .label
                .as_deref()
                .unwrap_or(&suggestion.invocation);
            let label = if suggestion.invocation.starts_with("/skill:") {
                label.to_string()
            } else {
                truncate_end(&format!("{label}{hint}"), label_width)
            };
            let label_padding = label_width.saturating_sub(UnicodeWidthStr::width(label.as_str()));
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{label}{}", " ".repeat(label_padding)), label_style),
                Span::styled(
                    truncate_end(&suggestion.description, description_width),
                    description_style,
                ),
            ])
        })
        .collect();
    selection.set_rows(rows);
    selection.view(frame, area);
}

pub(super) fn draw_composer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    palette: UiPalette,
) {
    if area.is_empty() {
        return;
    }
    if let Some(background) = palette.composer_background {
        frame.render_widget(
            Block::default().style(Style::default().bg(background)),
            area,
        );
    }
    let input = app.input.text();
    let trimmed = input.trim_start();
    let marker_style = if trimmed.starts_with('!') {
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    } else if trimmed.starts_with('/') {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let content_y = area.y.saturating_add(u16::from(area.height > 1));
    let marker_area = Rect::new(area.x, content_y, area.width.min(COMPOSER_TEXT_OFFSET), 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("› ", marker_style))),
        marker_area,
    );
    let inner = Rect::new(
        area.x.saturating_add(COMPOSER_TEXT_OFFSET),
        content_y,
        area.width
            .saturating_sub(COMPOSER_TEXT_OFFSET.saturating_add(1)),
        area.height.saturating_sub(u16::from(area.height > 1)),
    );
    frame.render_widget(app.input.widget(), inner);
}

pub(super) fn assistant_error(message: &pi_core::AssistantMessage) -> Option<String> {
    if message.stop_reason != StopReason::Error {
        return None;
    }
    let detail = message
        .error_message
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .unwrap_or("Unknown error");
    if detail
        .get(.."Error:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Error:"))
    {
        Some(detail.to_string())
    } else {
        Some(format!("Error: {detail}"))
    }
}

pub(super) fn assistant_token_usage(message: &pi_core::AssistantMessage) -> u64 {
    let usage = &message.usage;
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    }
}

pub(super) fn assistant_context_usage(message: &pi_core::AssistantMessage) -> u64 {
    message
        .usage
        .input
        .saturating_add(message.usage.cache_read)
        .saturating_add(message.usage.cache_write)
}

pub(super) fn message_token_usage(message: &Message) -> u64 {
    match message {
        Message::Assistant(message) => assistant_token_usage(message),
        Message::ToolResult(message) => {
            message.usage.as_ref().map_or(0, |usage| usage.total_tokens)
        }
        Message::User(_) => 0,
    }
}

pub(super) fn latest_context_usage(messages: &[Message]) -> u64 {
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message) if message.usage.total_tokens > 0 => {
                Some(assistant_context_usage(message))
            }
            _ => None,
        })
        .unwrap_or(0)
}

pub(super) fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub(super) fn context_window(app: &App) -> Option<u64> {
    app.model_specs
        .iter()
        .find(|spec| spec.provider.as_str() == app.provider && spec.id.as_str() == app.model)
        .map(|spec| spec.context_window)
}

pub(super) fn usage_footer(app: &App) -> String {
    let tokens = format!("tokens {}", format_token_count(app.session_tokens));
    match context_window(app) {
        Some(window) if window > 0 => {
            let used_percent = app.context_tokens.saturating_mul(100) / window;
            let remaining_percent = 100u64.saturating_sub(used_percent.min(100));
            format!(
                "context {}/{} ({}% left) · {tokens}",
                format_token_count(app.context_tokens),
                format_token_count(window),
                remaining_percent
            )
        }
        _ => format!(
            "context {} · {tokens}",
            format_token_count(app.context_tokens)
        ),
    }
}

pub(super) fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    palette: UiPalette,
) {
    let area = inset(area, COMPOSER_TEXT_OFFSET);
    let tone = status_tone(app);
    let queued = app.queue.steering.len() + app.queue.follow_up.len();
    let mut model = app.model.clone();
    if app.thinking != "off" {
        model.push(' ');
        model.push_str(&app.thinking);
    }
    let mut spans = vec![
        Span::styled(model, Style::default().fg(Color::Yellow)),
        footer_separator(),
        Span::styled(compact_path(&app.cwd), Style::default().fg(Color::Green)),
    ];
    if area.width >= 90 {
        spans.push(footer_separator());
        spans.push(Span::styled(
            app.session_name
                .as_ref()
                .map_or_else(|| "Workspace".to_string(), Clone::clone),
            Style::default().fg(Color::Magenta),
        ));
    }
    if !matches!(app.status.as_str(), "Ready" | "Working…") {
        spans.push(footer_separator());
        spans.push(Span::styled(
            app.status.clone(),
            Style::default().fg(if tone == Color::Red {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ));
    }
    if queued > 0 {
        spans.push(footer_separator());
        spans.push(Span::styled(
            format!("{queued} queued"),
            Style::default().fg(palette.accent),
        ));
    }
    if area.width >= 72 {
        spans.push(footer_separator());
        spans.push(Span::styled(
            usage_footer(app),
            Style::default().fg(palette.accent),
        ));
    }
    if area.width >= 120 {
        spans.push(footer_separator());
        if app.transcript_selection.is_some() {
            spans.push(Span::styled(
                "⌘C / Ctrl+Shift+C copy",
                Style::default().fg(palette.accent),
            ));
        } else {
            spans.push(Span::styled(
                "/ commands",
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(super) fn footer_separator() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(Color::DarkGray))
}

pub(super) fn status_tone(app: &App) -> Color {
    let status = app.status.to_ascii_lowercase();
    if status.starts_with("error") || status.contains("failed") || status.contains("provider error")
    {
        Color::Red
    } else if app.is_running || app.bash_line.is_some() || app.compacting {
        Color::Yellow
    } else {
        Color::Green
    }
}

pub(super) fn compact_path(path: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return path.to_string();
    };
    path.strip_prefix(&home)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        .map_or_else(|| path.to_string(), |suffix| format!("~{suffix}"))
}

pub(super) fn discover_session_choices(
    current_path: &Path,
    current_cwd: &Path,
) -> Vec<SessionChoice> {
    let root = session_catalog_root(current_path);
    let current_path = canonical_path(current_path);
    let current_cwd = canonical_path(current_cwd);
    let mut choices = session_catalog_paths(&root)
        .into_iter()
        .filter_map(|path| {
            let header = read_session_header(&path)?;
            if canonical_path(&header.cwd) != current_cwd {
                return None;
            }
            let (_, document) = pi_session::SessionLog::open(&path).ok()?;
            let first_message = document
                .entries
                .iter()
                .find_map(|record| {
                    let pi_session::SessionEntry::Message(entry) = &record.entry else {
                        return None;
                    };
                    let Message::User(user) = entry.message.as_standard()? else {
                        return None;
                    };
                    let text = user_text(&user.content);
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    (!text.is_empty()).then_some(text)
                })
                .unwrap_or_else(|| "(no messages)".to_string());
            let modified_at_ms = std::fs::metadata(&path)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| {
                    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                });
            Some(SessionChoice {
                current: canonical_path(&path) == current_path,
                path,
                id: header.id,
                cwd: header.cwd,
                name: document.name.and_then(|name| {
                    let name = name.trim().to_string();
                    (!name.is_empty()).then_some(name)
                }),
                first_message,
                message_count: document.stats.message_count,
                modified_at_ms,
            })
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        right
            .modified_at_ms
            .cmp(&left.modified_at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    choices
}

pub(super) fn session_catalog_root(current_path: &Path) -> PathBuf {
    let parent = current_path.parent().unwrap_or_else(|| Path::new("."));
    let encoded_cwd_directory = parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("--") && name.ends_with("--"));
    if encoded_cwd_directory {
        parent.parent().unwrap_or(parent).to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

pub(super) fn session_catalog_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if is_jsonl_file(&path) {
            paths.push(path);
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let Ok(children) = std::fs::read_dir(path) else {
            continue;
        };
        paths.extend(
            children
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| is_jsonl_file(path)),
        );
    }
    paths
}

pub(super) fn is_jsonl_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
}

pub(super) fn read_session_header(path: &Path) -> Option<pi_session::SessionHeader> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim_end_matches(['\r', '\n'])).ok()
}

pub(super) fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn format_session_age(modified_at_ms: u64, now_ms: u64) -> String {
    let elapsed_ms = now_ms.saturating_sub(modified_at_ms);
    let minutes = elapsed_ms / 60_000;
    let hours = elapsed_ms / 3_600_000;
    let days = elapsed_ms / 86_400_000;
    if minutes < 1 {
        "now".to_string()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if hours < 24 {
        format!("{hours}h")
    } else if days < 7 {
        format!("{days}d")
    } else if days < 30 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

pub(super) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

pub(super) fn input_cursor_position(input: &str, width: u16) -> (usize, usize) {
    let width = usize::from(width.max(1));
    let mut row = 0usize;
    let mut column = 0usize;
    for character in input.chars() {
        if character == '\n' {
            row = row.saturating_add(1);
            column = 0;
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0).max(1);
        if column.saturating_add(character_width) > width {
            row = row.saturating_add(1);
            column = 0;
        }
        column = column.saturating_add(character_width);
        if column >= width {
            row = row.saturating_add(column / width);
            column %= width;
        }
    }
    (row, column)
}

struct TerminalInitializationGuard {
    fullscreen: bool,
    active: bool,
}

impl Drop for TerminalInitializationGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut stdout = io::stdout();
        if self.fullscreen {
            let _ = leave_fullscreen(&mut stdout);
        }
        let _ = execute!(stdout, crossterm::cursor::Show);
        let _ = disable_raw_mode();
    }
}

pub(super) fn setup_terminal(fullscreen: bool) -> io::Result<TuiTerminal> {
    enable_raw_mode()?;
    let mut restore_guard = TerminalInitializationGuard {
        fullscreen,
        active: true,
    };
    let mut stdout = io::stdout();
    if fullscreen {
        enter_fullscreen(&mut stdout)?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;
    restore_guard.active = false;
    Ok(terminal)
}

pub(super) fn restore_terminal(terminal: &mut TuiTerminal, fullscreen: bool) -> io::Result<()> {
    restore_terminal_writer(terminal.backend_mut(), fullscreen, disable_raw_mode)
}

pub(super) fn restore_terminal_writer(
    writer: &mut impl io::Write,
    fullscreen: bool,
    disable_raw: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let mut first_error = None;
    if fullscreen {
        record_terminal_cleanup_error(&mut first_error, leave_fullscreen(writer));
    } else if let Err(error) = write!(writer, "\r\n") {
        first_error.get_or_insert(error);
    }
    record_terminal_cleanup_error(&mut first_error, disable_raw());
    record_terminal_cleanup_error(&mut first_error, execute!(writer, crossterm::cursor::Show));
    record_terminal_cleanup_error(&mut first_error, writer.flush());
    first_error.map_or(Ok(()), Err)
}

pub(super) fn record_terminal_cleanup_error(
    first_error: &mut Option<io::Error>,
    result: io::Result<()>,
) {
    if let Err(error) = result {
        first_error.get_or_insert(error);
    }
}

pub(super) fn enter_fullscreen(writer: &mut impl io::Write) -> io::Result<()> {
    execute!(
        writer,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

pub(super) fn leave_fullscreen(writer: &mut impl io::Write) -> io::Result<()> {
    execute!(
        writer,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )
}

#[cfg(test)]
pub(super) fn transcript_lines(items: &[TranscriptItem]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.extend(transcript_item_lines(
            item,
            TerminalAppearance::Dark,
            0,
            false,
            0,
        ));
    }
    lines
}

pub(super) fn transcript_item_lines(
    item: &TranscriptItem,
    terminal_appearance: TerminalAppearance,
    animation_frame: usize,
    tools_expanded: bool,
    working_elapsed_seconds: u64,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match item {
        TranscriptItem::User(text) => {
            let first_line = lines.len();
            push_plain_text(&mut lines, text, "    ");
            if let Some(line) = lines.get_mut(first_line) {
                line.spans[0] = Span::styled("  › ", Style::default().add_modifier(Modifier::BOLD));
            } else {
                lines.push(Line::from(Span::styled(
                    "  ›",
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
        }
        TranscriptItem::Notice(text) => {
            push_plain_text(&mut lines, text, "  ");
            for line in &mut lines {
                for span in &mut line.spans {
                    span.style = Style::default().fg(Color::DarkGray);
                }
            }
        }
        TranscriptItem::Assistant {
            text,
            streaming,
            error,
        } => {
            lines.extend(render_assistant_markdown_at(
                text,
                *streaming,
                terminal_appearance,
                animation_frame,
                working_elapsed_seconds,
            ));
            if let Some(error) = error {
                if lines.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{STATUS_DOT} "),
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(error.clone(), Style::default().fg(Color::Red)),
                    ]));
                } else {
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        format!("  {error}"),
                        Style::default().fg(Color::Red),
                    )));
                }
            }
        }
        TranscriptItem::Tool {
            name,
            detail,
            input,
            output,
            state,
            ..
        } => {
            let color = match state {
                ToolState::Pending => Color::DarkGray,
                ToolState::Running => Color::Yellow,
                ToolState::Succeeded => Color::Green,
                ToolState::Failed(_) => Color::Red,
            };
            let mut spans = vec![
                Span::styled(format!("{STATUS_DOT} "), Style::default().fg(color)),
                Span::raw(name.clone()),
            ];
            if let Some(detail) = detail {
                spans.push(Span::styled(
                    format!("  {}", truncate_end(detail, 96)),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            lines.push(Line::from(spans));
            if tools_expanded {
                push_tool_payload_lines(&mut lines, "input", input.as_deref());
                push_tool_payload_lines(&mut lines, "output", output.as_deref());
            }
            if let ToolState::Failed(error) = state
                && !error.is_empty()
            {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(truncate_end(error, 120), Style::default().fg(Color::Red)),
                ]));
            }
        }
        TranscriptItem::Shell {
            command,
            output,
            excluded_from_context,
            state,
        } => push_shell_lines(&mut lines, command, output, *excluded_from_context, state),
    }
    lines
}

pub(super) fn push_plain_text(lines: &mut Vec<Line<'static>>, text: &str, prefix: &'static str) {
    lines.extend(
        text.lines()
            .map(|line| Line::from(vec![Span::raw(prefix), Span::raw(line.to_string())])),
    );
}

#[cfg(test)]
pub(super) fn render_assistant_markdown(
    text: &str,
    streaming: bool,
    terminal_appearance: TerminalAppearance,
    animation_frame: usize,
) -> Vec<Line<'static>> {
    let mut lines =
        render_assistant_markdown_at(text, streaming, terminal_appearance, animation_frame, 0);
    if text.is_empty()
        && streaming
        && let Some(indicator) = lines.first_mut().and_then(|line| line.spans.first_mut())
    {
        indicator.style = indicator.style.fg(activity_indicator_color(
            terminal_appearance,
            animation_frame,
        ));
    }
    lines
}

pub(super) fn render_assistant_markdown_at(
    text: &str,
    streaming: bool,
    terminal_appearance: TerminalAppearance,
    animation_frame: usize,
    working_elapsed_seconds: u64,
) -> Vec<Line<'static>> {
    let mut lines = pi_md::render(text, streaming, markdown_theme(terminal_appearance)).lines;
    while lines.first().is_some_and(markdown_line_is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(markdown_line_is_blank) {
        lines.pop();
    }
    if lines.is_empty() && streaming {
        return vec![working_status_line(
            terminal_appearance,
            animation_frame,
            working_elapsed_seconds,
        )];
    }
    if let Some(first) = lines.first_mut() {
        first.spans.insert(
            0,
            Span::styled(
                format!("{STATUS_DOT} "),
                Style::default()
                    .fg(if streaming {
                        activity_indicator_color(terminal_appearance, animation_frame)
                    } else {
                        Color::DarkGray
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }
    lines
}

pub(super) fn working_status_line(
    appearance: TerminalAppearance,
    animation_frame: usize,
    elapsed_seconds: u64,
) -> Line<'static> {
    let leading_color = working_shimmer_color(appearance, animation_frame, 0);
    let mut spans = vec![Span::styled(
        format!("{STATUS_DOT} "),
        Style::default()
            .fg(leading_color)
            .add_modifier(Modifier::BOLD),
    )];
    for (index, character) in "Working".chars().enumerate() {
        spans.push(Span::styled(
            character.to_string(),
            Style::default()
                .fg(working_shimmer_color(appearance, animation_frame, index))
                .add_modifier(Modifier::BOLD),
        ));
    }
    let muted = Style::default().fg(Color::DarkGray);
    spans.extend([
        Span::styled(
            format!(" ({} • ", format_elapsed_compact(elapsed_seconds)),
            muted,
        ),
        Span::styled("esc", muted.add_modifier(Modifier::BOLD)),
        Span::styled(" to interrupt)", muted),
    ]);
    Line::from(spans)
}

pub(super) fn working_shimmer_color(
    appearance: TerminalAppearance,
    animation_frame: usize,
    character_index: usize,
) -> Color {
    let phase = animation_frame % 11;
    let distance = phase.abs_diff(character_index);
    match (appearance, distance) {
        (TerminalAppearance::Light, 0) => Color::Rgb(45, 45, 48),
        (TerminalAppearance::Light, 1) => Color::Rgb(90, 90, 94),
        (TerminalAppearance::Light, 2) => Color::Rgb(132, 132, 137),
        (TerminalAppearance::Light, _) => Color::Rgb(174, 174, 178),
        (TerminalAppearance::Dark, 0) => Color::Rgb(245, 245, 247),
        (TerminalAppearance::Dark, 1) => Color::Rgb(209, 209, 214),
        (TerminalAppearance::Dark, 2) => Color::Rgb(162, 162, 167),
        (TerminalAppearance::Dark, _) => Color::Rgb(99, 99, 102),
    }
}

pub(super) fn format_elapsed_compact(elapsed_seconds: u64) -> String {
    if elapsed_seconds < 60 {
        return format!("{elapsed_seconds}s");
    }
    if elapsed_seconds < 3600 {
        return format!("{}m {:02}s", elapsed_seconds / 60, elapsed_seconds % 60);
    }
    format!(
        "{}h {:02}m {:02}s",
        elapsed_seconds / 3600,
        (elapsed_seconds % 3600) / 60,
        elapsed_seconds % 60
    )
}

pub(super) fn activity_indicator_color(
    appearance: TerminalAppearance,
    animation_frame: usize,
) -> Color {
    let colors = match appearance {
        TerminalAppearance::Light => [
            Color::Rgb(138, 109, 0),
            Color::Rgb(181, 137, 0),
            Color::Rgb(210, 153, 34),
            Color::Rgb(181, 137, 0),
        ],
        TerminalAppearance::Dark => [
            Color::Rgb(110, 93, 22),
            Color::Rgb(198, 144, 38),
            Color::Rgb(242, 204, 96),
            Color::Rgb(198, 144, 38),
        ],
    };
    colors[animation_frame % colors.len()]
}

pub(super) fn markdown_line_is_blank(line: &Line<'_>) -> bool {
    line.style.bg.is_none()
        && line
            .spans
            .iter()
            .all(|span| span.content.trim().is_empty() && span.style.bg.is_none())
}

pub(super) fn push_shell_lines(
    lines: &mut Vec<Line<'static>>,
    command: &str,
    output: &str,
    excluded_from_context: bool,
    state: &ShellState,
) {
    let (color, state_label) = match state {
        ShellState::Running => (Color::Yellow, Some("running".to_string())),
        ShellState::Finished {
            cancelled: true, ..
        } => (Color::Yellow, Some("cancelled".to_string())),
        ShellState::Finished {
            timed_out: true, ..
        } => (Color::Red, Some("timed out".to_string())),
        ShellState::Finished {
            exit_code: Some(code),
            ..
        } if *code != 0 => (Color::Red, Some(format!("exit {code}"))),
        ShellState::Finished {
            truncated: true, ..
        } => (Color::Green, Some("truncated".to_string())),
        ShellState::Finished { .. } => (Color::Green, None),
        ShellState::Failed(error) => (Color::Red, Some(error.clone())),
    };
    let mut header = vec![
        Span::styled(format!("{STATUS_DOT} "), Style::default().fg(color)),
        Span::styled("Ran ", Style::default().fg(Color::DarkGray)),
        Span::raw(command.to_string()),
    ];
    if excluded_from_context {
        header.push(Span::styled(
            "  private",
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(label) = state_label.as_ref()
        && !matches!(state, ShellState::Failed(_))
    {
        header.push(Span::styled(
            format!("  {label}"),
            Style::default().fg(color),
        ));
    }
    lines.push(Line::from(header));

    let output_lines = output.lines().collect::<Vec<_>>();
    let omitted = output_lines.len().saturating_sub(12);
    if omitted > 0 {
        lines.push(Line::from(vec![
            Span::styled("    ⋮ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("… {omitted} earlier lines"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    for (index, output_line) in output_lines.into_iter().skip(omitted).enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                if index == 0 { "    └ " } else { "      " },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                output_line.to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    if output.is_empty() && !matches!(state, ShellState::Running) {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("(no output)", Style::default().fg(Color::DarkGray)),
        ]));
    }
    if let ShellState::Failed(error) = state {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(truncate_end(error, 120), Style::default().fg(Color::Red)),
        ]));
    }
}

pub(super) fn push_tool_payload_lines(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    payload: Option<&str>,
) {
    let Some(payload) = payload.filter(|payload| !payload.is_empty()) else {
        return;
    };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(format!("{label}:"), Style::default().fg(Color::DarkGray)),
    ]));
    for payload_line in payload.lines() {
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(
                payload_line.to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
}

pub(super) fn format_tool_input(value: &serde_json::Value) -> Option<String> {
    serde_json::to_string_pretty(value).ok()
}

pub(super) fn format_tool_result(result: &pi_core::ToolResult) -> String {
    let mut parts = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(details) = &result.details
        && let Ok(details) = serde_json::to_string_pretty(details)
    {
        parts.push(details);
    }
    parts.join("\n")
}

pub(super) fn summarize_tool_args(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    for key in [
        "path",
        "file_path",
        "command",
        "pattern",
        "query",
        "name",
        "url",
    ] {
        if let Some(value) = object.get(key).and_then(serde_json::Value::as_str)
            && !value.is_empty()
        {
            return Some(value.replace('\n', " "));
        }
    }
    if object.is_empty() {
        None
    } else {
        serde_json::to_string(value).ok()
    }
}

pub(super) fn tool_result_text(result: &pi_core::ToolResult) -> String {
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        "Tool failed".to_string()
    } else {
        truncate_end(&text, 120)
    }
}

pub(super) fn truncate_end(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut width = 0usize;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > max_width.saturating_sub(1) {
            break;
        }
        output.push(character);
        width = width.saturating_add(character_width);
    }
    output.push('…');
    output
}

pub(super) fn truncate_start(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut tail = Vec::new();
    let mut width = 0usize;
    for character in text.chars().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > max_width.saturating_sub(1) {
            break;
        }
        tail.push(character);
        width = width.saturating_add(character_width);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}
