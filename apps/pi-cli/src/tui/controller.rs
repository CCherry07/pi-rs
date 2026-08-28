use super::*;

pub(super) struct KeyUi<'a, C> {
    pub(super) clipboard: &'a mut C,
    pub(super) transcript_viewport_height: u16,
}

pub(super) fn handle_key<C: ClipboardWriter>(
    key: KeyEvent,
    app: &mut App,
    session: &Arc<AgentSession>,
    session_handle: &PiSession,
    project_trust: &ProjectTrustService,
    sender: &tokio::sync::mpsc::UnboundedSender<EffectDone>,
    ui: &mut KeyUi<'_, C>,
) {
    if handle_trust_prompt_key(key, app, project_trust) {
        return;
    }
    if handle_bottom_pane_view_key(
        key,
        app,
        session,
        session_handle,
        project_trust,
        sender,
        ui.clipboard,
    ) {
        return;
    }
    if handle_tool_output_key(key, app) {
        return;
    }
    if handle_vertical_navigation(key.code, app) {
        return;
    }
    if handle_transcript_navigation(key, app, ui.transcript_viewport_height) {
        return;
    }
    if is_newline_key(key) {
        app.input.insert_newline();
        app.input_history.reset_navigation();
        app.command_palette.get_mut().reset();
        return;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            match ctrl_c_action(app) {
                CtrlCAction::ClearComposer => {
                    app.input.clear();
                    app.dismissed_completion = None;
                    app.input_history.reset_navigation();
                    app.command_palette.get_mut().reset();
                }
                CtrlCAction::Interrupt => {
                    app.awaiting_assistant = false;
                    session.abort();
                    session.abort_shell();
                    app.status = "Interrupt requested".to_string();
                }
                CtrlCAction::Quit => app.quit = true,
            }
        }
        (KeyCode::Char('d'), modifiers)
            if modifiers.contains(KeyModifiers::CONTROL) && app.input.is_empty() =>
        {
            app.quit = true;
        }
        (KeyCode::Tab, _) => {
            complete_selected_command(app);
        }
        (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::ALT) => {
            submit_editor(
                app,
                session,
                session_handle,
                project_trust,
                sender,
                ui.clipboard,
                EffectMode::FollowUp,
            );
        }
        (KeyCode::Enter, _) => {
            if !complete_selected_command_for_enter(app) {
                submit_editor(
                    app,
                    session,
                    session_handle,
                    project_trust,
                    sender,
                    ui.clipboard,
                    EffectMode::Submit,
                );
            }
        }
        (KeyCode::Esc, _) if completion_panel_visible(app) => {
            app.dismissed_completion = Some(app.input.text());
            app.command_palette.get_mut().reset();
        }
        (KeyCode::Esc, _) => {
            app.awaiting_assistant = false;
            session.abort();
            session.abort_shell();
            if let Ok(queue) = session.clear_queue() {
                let restored = queue
                    .steering
                    .into_iter()
                    .chain(queue.follow_up)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !restored.is_empty() {
                    let input = [restored, app.input.take_text()]
                        .into_iter()
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    app.input.set_text(input);
                }
            }
            app.command_palette.get_mut().reset();
            app.status = "Abort requested".to_string();
        }
        _ => {
            app.dismissed_completion = None;
            app.input.handle_key(key);
            app.input_history.reset_navigation();
            app.command_palette.get_mut().reset();
        }
    }
}

pub(super) fn ctrl_c_action(app: &App) -> CtrlCAction {
    if !app.input.is_empty() {
        CtrlCAction::ClearComposer
    } else if app.is_running || app.bash_line.is_some() || app.compacting {
        CtrlCAction::Interrupt
    } else {
        CtrlCAction::Quit
    }
}

pub(super) fn handle_bottom_pane_view_key(
    key: KeyEvent,
    app: &mut App,
    session: &Arc<AgentSession>,
    session_handle: &PiSession,
    project_trust: &ProjectTrustService,
    sender: &tokio::sync::mpsc::UnboundedSender<EffectDone>,
    clipboard: &mut impl ClipboardWriter,
) -> bool {
    let Some(view) = app.active_bottom_view() else {
        return false;
    };
    let item_count = match view {
        BottomPaneView::Model => app.model_specs.len(),
        BottomPaneView::Thinking => app.thinking_choices().len(),
        BottomPaneView::Resume => app.session_choices.len(),
        BottomPaneView::Tree => app.tree_choices.len(),
        BottomPaneView::Fork => app.fork_choices.len(),
        BottomPaneView::Login => app.login_providers.len(),
        BottomPaneView::Logout => app.logout_providers.len(),
    };
    app.view_selection.get_mut().reconcile_len(item_count);
    match key.code {
        KeyCode::Up => app.view_selection.get_mut().previous(),
        KeyCode::Down => app.view_selection.get_mut().next(),
        KeyCode::Esc => {
            app.pop_bottom_view();
            app.status = "Ready".to_string();
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.pop_bottom_view();
            app.status = "Ready".to_string();
        }
        KeyCode::Enter => {
            let selected = app.view_selection.borrow().selected().unwrap_or_default();
            let command = match view {
                BottomPaneView::Model => app
                    .model_specs
                    .get(selected)
                    .map(|model| format!("/model {}/{}", model.provider, model.id)),
                BottomPaneView::Thinking => app
                    .thinking_choices()
                    .get(selected)
                    .map(|choice| format!("/thinking {}", choice.level.as_str())),
                BottomPaneView::Resume => app
                    .session_choices
                    .get(selected)
                    .map(|choice| format!("/resume {}", choice.path.display())),
                BottomPaneView::Tree => app
                    .tree_choices
                    .get(selected)
                    .map(|choice| format!("/tree {}", choice.id)),
                BottomPaneView::Fork => app
                    .fork_choices
                    .get(selected)
                    .map(|choice| format!("/fork {}", choice.id)),
                BottomPaneView::Login => app
                    .login_providers
                    .get(selected)
                    .map(|provider| format!("/login {}", provider.id)),
                BottomPaneView::Logout => app
                    .logout_providers
                    .get(selected)
                    .map(|provider| format!("/logout {}", provider.id)),
            };
            if let Some(command) = command {
                app.pop_bottom_view();
                app.input.set_text(command);
                submit_editor(
                    app,
                    session,
                    session_handle,
                    project_trust,
                    sender,
                    clipboard,
                    EffectMode::Submit,
                );
            }
        }
        _ => {}
    }
    true
}

pub(super) fn handle_transcript_navigation(
    key: KeyEvent,
    app: &mut App,
    viewport_height: u16,
) -> bool {
    let page_rows = usize::from(viewport_height)
        .saturating_sub(PAGE_SCROLL_OVERLAP)
        .max(1);
    match key.code {
        KeyCode::PageUp => {
            app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(page_rows);
            true
        }
        KeyCode::PageDown => {
            app.scroll_from_bottom = app.scroll_from_bottom.saturating_sub(page_rows);
            true
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_from_bottom = 0;
            true
        }
        _ => false,
    }
}

pub(super) fn handle_trust_prompt_key(
    key: KeyEvent,
    app: &mut App,
    project_trust: &ProjectTrustService,
) -> bool {
    let Some(prompt) = app.trust_prompt.as_mut() else {
        return false;
    };
    match key.code {
        KeyCode::Up => {
            prompt.selected = prompt
                .selected
                .checked_sub(1)
                .unwrap_or_else(|| prompt.options.len().saturating_sub(1));
        }
        KeyCode::Down => {
            prompt.selected = if prompt.selected + 1 >= prompt.options.len() {
                0
            } else {
                prompt.selected + 1
            };
        }
        KeyCode::Enter => {
            let Some(mut prompt) = app.trust_prompt.take() else {
                return true;
            };
            if let Some(response) = prompt.response.take() {
                let _ = response.send(Some(prompt.selected));
                app.status = "Applying project trust…".to_string();
            } else if let Some(option) = prompt.options.get(prompt.selected) {
                app.status = match project_trust.apply_option(&prompt.cwd, option) {
                    Ok(_) => "Project trust saved · restart to apply".to_string(),
                    Err(error) => format!("Project trust error: {error}"),
                };
            }
        }
        KeyCode::Esc | KeyCode::Char('c')
            if matches!(key.code, KeyCode::Esc)
                || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if let Some(mut prompt) = app.trust_prompt.take()
                && let Some(response) = prompt.response.take()
            {
                let _ = response.send(None);
            }
            app.status = "Project trust unchanged".to_string();
        }
        _ => {}
    }
    true
}

pub(super) fn handle_tool_output_key(key: KeyEvent, app: &mut App) -> bool {
    if !matches!(key.code, KeyCode::Char(character) if character.eq_ignore_ascii_case(&'o'))
        || !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return false;
    }
    app.tools_expanded = !app.tools_expanded;
    app.status = format!(
        "Tool output: {}",
        if app.tools_expanded {
            "expanded"
        } else {
            "collapsed"
        }
    );
    true
}

pub(super) fn is_copy_shortcut(key: KeyEvent) -> bool {
    let KeyCode::Char(character) = key.code else {
        return false;
    };
    character.eq_ignore_ascii_case(&'c')
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT)))
}

pub(super) fn handle_copy_shortcut(
    key: KeyEvent,
    app: &mut App,
    surface: &ScreenTextSurface,
    clipboard: &mut impl ClipboardWriter,
) -> bool {
    if !is_copy_shortcut(key) {
        return false;
    }
    let Some(selection) = app.screen_selection else {
        app.status = "No screen text selected".to_string();
        return true;
    };
    let Some(text) = surface.selected_text(selection) else {
        app.status = "No screen text selected".to_string();
        return true;
    };
    copy_screen_text(app, clipboard, &text);
    true
}

fn copy_screen_text(app: &mut App, clipboard: &mut impl ClipboardWriter, text: &str) {
    match clipboard.set_text(text) {
        Ok(()) => app.status = format!("Copied {} characters", text.chars().count()),
        Err(error) => app.status = format!("Copy failed: {error}"),
    }
}

pub(super) fn handle_copy_command(app: &mut App, clipboard: &mut impl ClipboardWriter) -> bool {
    let Some(text) = app.transcript.iter().rev().find_map(|item| match item {
        TranscriptItem::Assistant {
            text,
            streaming: false,
            ..
        } if !text.trim().is_empty() => Some(text.as_str()),
        _ => None,
    }) else {
        app.status = "No assistant messages to copy yet".to_string();
        return true;
    };
    match clipboard.set_text(text) {
        Ok(()) => app.status = "Copied last assistant message to clipboard".to_string(),
        Err(error) => app.status = format!("Copy failed: {error}"),
    }
    true
}

pub(super) fn is_newline_key(key: KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('j'), KeyModifiers::CONTROL) | (KeyCode::Enter, KeyModifiers::SHIFT)
    )
}

pub(super) fn handle_vertical_navigation(code: KeyCode, app: &mut App) -> bool {
    let has_command_suggestions = !suggestions_for_app(app).is_empty();
    match code {
        KeyCode::Up if has_command_suggestions => move_command_selection(app, -1),
        KeyCode::Down if has_command_suggestions => move_command_selection(app, 1),
        KeyCode::Up => {
            let current = app.input.text();
            if let Some(input) = app.input_history.older(&current) {
                app.input.set_text(input);
                app.command_palette.get_mut().reset();
            } else {
                return false;
            }
        }
        KeyCode::Down if app.input_history.is_browsing() => {
            if let Some(input) = app.input_history.newer() {
                app.input.set_text(input);
                app.command_palette.get_mut().reset();
            }
        }
        _ => return false,
    }
    true
}

pub(super) fn handle_mouse(
    mouse: MouseEvent,
    app: &mut App,
    surface: &ScreenTextSurface,
) -> Option<String> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.screen_selection = None;
            if app.trust_prompt.is_none() {
                let lines = app.scroll_input.lines(ScrollDirection::Up);
                app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(lines);
            }
        }
        MouseEventKind::ScrollDown => {
            app.screen_selection = None;
            if app.trust_prompt.is_none() {
                let lines = app.scroll_input.lines(ScrollDirection::Down);
                app.scroll_from_bottom = app.scroll_from_bottom.saturating_sub(lines);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.screen_selection =
                ScreenSelection::begin(surface, Position::new(mouse.column, mouse.row));
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(selection) = &mut app.screen_selection {
                selection.drag_to(surface, Position::new(mouse.column, mouse.row));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let keep = app.screen_selection.as_mut().is_some_and(|selection| {
                selection.finish(surface, Position::new(mouse.column, mouse.row))
            });
            if !keep {
                app.screen_selection = None;
            } else if let Some(text) = app
                .screen_selection
                .and_then(|selection| surface.selected_text(selection))
            {
                return Some(text);
            }
        }
        _ => {}
    }
    None
}

pub(super) fn handle_mouse_event(
    mouse: MouseEvent,
    app: &mut App,
    surface: &ScreenTextSurface,
    clipboard: &mut impl ClipboardWriter,
) {
    if let Some(text) = handle_mouse(mouse, app, surface) {
        copy_screen_text(app, clipboard, &text);
    }
}

pub(super) fn submit_editor(
    app: &mut App,
    session: &Arc<AgentSession>,
    session_handle: &PiSession,
    project_trust: &ProjectTrustService,
    sender: &tokio::sync::mpsc::UnboundedSender<EffectDone>,
    clipboard: &mut impl ClipboardWriter,
    mode: EffectMode,
) {
    let input = app.input.text();
    let raw_input = input.trim();
    if let Some(status) = incomplete_command_status(raw_input, &app.command_specs) {
        app.status = status.to_string();
        return;
    }
    let input = raw_input.to_string();
    if input.is_empty() {
        return;
    }
    app.input_history.record(&input);
    if activate_bottom_view_for_command(app, &input) {
        return;
    }
    match auth_request_for_input(app, &input) {
        Ok(Some(request)) => {
            let provider = request.provider.clone();
            let operation = request.operation;
            app.input.clear();
            app.command_palette.get_mut().reset();
            app.pending_auth = Some(request);
            app.status = match operation {
                AuthOperation::Login => format!("Configure authentication for {provider}"),
                AuthOperation::Logout => format!("Remove stored credential for {provider}"),
            };
            return;
        }
        Ok(None) => {}
        Err(error) => {
            app.input.clear();
            app.command_palette.get_mut().reset();
            app.status = error;
            return;
        }
    }
    if input == "/quit" {
        app.quit = true;
        return;
    }
    if input == "/copy" {
        handle_copy_command(app, clipboard);
        app.input.clear();
        app.command_palette.get_mut().reset();
        return;
    }
    if input == "/clear" {
        app.clear_transcript();
        app.input.clear();
        app.command_palette.get_mut().reset();
        return;
    }
    if input == "/trust" {
        match project_trust.manual_options(session.runtime().cwd()) {
            Ok(options) => {
                app.trust_prompt = Some(TrustPromptState {
                    cwd: session.runtime().cwd().to_path_buf(),
                    options,
                    selected: 0,
                    response: None,
                });
                app.status = "Choose project trust".to_string();
            }
            Err(error) => app.status = format!("Project trust error: {error}"),
        }
        app.input.clear();
        app.command_palette.get_mut().reset();
        return;
    }
    app.input.clear();
    app.command_palette.get_mut().reset();
    app.awaiting_assistant = true;
    app.working_started_at = Some(Instant::now());
    app.status = "Working…".to_string();
    spawn_effect(
        Arc::clone(session),
        session_handle.clone(),
        app.epoch,
        input,
        mode,
        sender.clone(),
    );
}

pub(super) fn activate_bottom_view_for_command(app: &mut App, input: &str) -> bool {
    if input == "/login" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        if app.login_providers.is_empty() {
            app.status = "No login providers available".to_string();
            return true;
        }
        app.push_bottom_view(BottomPaneView::Login);
        app.view_selection
            .get_mut()
            .reconcile_len(app.login_providers.len());
        app.status = "Select provider to configure".to_string();
        return true;
    }
    if input == "/logout" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        if app.logout_providers.is_empty() {
            app.status = "No stored credentials to remove · environment variables and models.json config are unchanged".to_string();
            return true;
        }
        app.push_bottom_view(BottomPaneView::Logout);
        app.view_selection
            .get_mut()
            .reconcile_len(app.logout_providers.len());
        app.status = "Select provider to log out".to_string();
        return true;
    }
    if input == "/model" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        app.push_bottom_view(BottomPaneView::Model);
        app.view_selection
            .get_mut()
            .reconcile_len(app.model_specs.len());
        let selected = app
            .model_specs
            .iter()
            .position(|model| {
                model.provider.as_str() == app.provider && model.id.as_str() == app.model
            })
            .unwrap_or_default();
        app.view_selection.get_mut().select(selected);
        app.status = "Select model and effort".to_string();
        return true;
    }
    if input == "/thinking" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        app.push_bottom_view(BottomPaneView::Thinking);
        let choices = app.thinking_choices();
        app.view_selection.get_mut().reconcile_len(choices.len());
        let selected = choices
            .iter()
            .position(|choice| choice.level.as_str() == app.thinking)
            .unwrap_or_default();
        app.view_selection.get_mut().select(selected);
        app.status = "Select thinking level".to_string();
        return true;
    }
    if input == "/resume" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        app.push_bottom_view(BottomPaneView::Resume);
        app.view_selection
            .get_mut()
            .reconcile_len(app.session_choices.len());
        let selected = app
            .session_choices
            .iter()
            .position(|choice| choice.current)
            .unwrap_or_default();
        app.view_selection.get_mut().select(selected);
        app.status = "Resume a session".to_string();
        return true;
    }
    if input == "/tree" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        if app.tree_choices.is_empty() {
            app.status = "No entries in session".to_string();
            return true;
        }
        app.push_bottom_view(BottomPaneView::Tree);
        app.view_selection
            .get_mut()
            .reconcile_len(app.tree_choices.len());
        let selected = app
            .tree_choices
            .iter()
            .position(|choice| choice.current)
            .unwrap_or_else(|| app.tree_choices.len().saturating_sub(1));
        app.view_selection.get_mut().select(selected);
        app.status = "Navigate session tree".to_string();
        return true;
    }
    if input == "/fork" {
        app.input.clear();
        app.command_palette.get_mut().reset();
        if app.fork_choices.is_empty() {
            app.status = "No messages to fork from".to_string();
            return true;
        }
        app.push_bottom_view(BottomPaneView::Fork);
        app.view_selection
            .get_mut()
            .reconcile_len(app.fork_choices.len());
        app.view_selection
            .get_mut()
            .select(app.fork_choices.len().saturating_sub(1));
        app.status = "Fork from a user message".to_string();
        return true;
    }
    false
}

pub(super) fn auth_request_for_input(
    app: &App,
    input: &str,
) -> Result<Option<AuthRequest>, String> {
    let (operation, providers, argument) = if let Some(argument) = command_argument(input, "/login")
    {
        (AuthOperation::Login, &app.login_providers, argument)
    } else if let Some(argument) = command_argument(input, "/logout") {
        (AuthOperation::Logout, &app.logout_providers, argument)
    } else {
        return Ok(None);
    };
    if argument.is_empty() {
        return Ok(None);
    }
    let provider = providers
        .iter()
        .find(|provider| provider.id.eq_ignore_ascii_case(argument))
        .ok_or_else(|| match operation {
            AuthOperation::Login => format!("Unknown login provider: {argument}"),
            AuthOperation::Logout => format!("No stored credential for {argument}"),
        })?;
    Ok(Some(AuthRequest {
        operation,
        provider: provider.id.clone(),
    }))
}

fn command_argument<'a>(input: &'a str, command: &str) -> Option<&'a str> {
    let input = input.trim();
    let rest = input.strip_prefix(command)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

pub(super) fn move_command_selection(app: &mut App, delta: i8) {
    let count = suggestions_for_app(app).len();
    let selection = app.command_palette.get_mut();
    selection.reconcile_len(count);
    if delta < 0 {
        selection.previous();
    } else {
        selection.next();
    }
}

pub(super) fn selected_command(app: &App) -> Option<CommandSuggestion> {
    let suggestions = suggestions_for_app(app);
    let index = app
        .command_palette
        .borrow()
        .selected()
        .unwrap_or_default()
        .min(suggestions.len().checked_sub(1)?);
    suggestions.into_iter().nth(index)
}

pub(super) fn complete_selected_command(app: &mut App) -> bool {
    let Some(suggestion) = selected_command(app) else {
        return false;
    };
    let mut input = suggestion.invocation;
    if suggestion.argument_hint.is_some() {
        input.push(' ');
    }
    app.input.set_text(input);
    app.dismissed_completion = None;
    app.input_history.reset_navigation();
    app.command_palette.get_mut().reset();
    true
}

pub(super) fn complete_selected_command_for_enter(app: &mut App) -> bool {
    let Some(suggestion) = selected_command(app) else {
        return false;
    };
    if app.input.text().trim() == suggestion.invocation {
        return false;
    }
    let apply_on_enter = suggestion.apply_on_enter;
    if !complete_selected_command(app) {
        return false;
    }
    !apply_on_enter
}

pub(super) fn command_query(input: &str) -> Option<&str> {
    let prefix = input.trim();
    if prefix.starts_with('/') && !prefix.contains(char::is_whitespace) {
        Some(prefix)
    } else {
        None
    }
}

pub(super) fn model_query(input: &str) -> Option<&str> {
    let input = input.trim();
    let rest = input.strip_prefix("/model")?;
    if rest.is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

pub(super) fn resume_query(input: &str) -> Option<&str> {
    let input = input.trim();
    let rest = input.strip_prefix("/resume")?;
    if rest.is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

pub(super) fn suggestions_for_app(app: &App) -> Vec<CommandSuggestion> {
    let input = app.input.text();
    if app.dismissed_completion.as_deref() == Some(input.as_str()) {
        return Vec::new();
    }
    if input.trim().starts_with("/model ")
        && let Some(query) = model_query(&input)
    {
        return model_suggestions(
            query,
            &app.model_specs,
            app.provider.as_str(),
            app.model.as_str(),
        );
    }
    if input.trim().starts_with("/resume ")
        && let Some(query) = resume_query(&input)
    {
        return resume_suggestions(query, &app.session_choices);
    }
    command_suggestions(&input, &app.command_specs)
}

pub(super) fn completion_panel_visible(app: &App) -> bool {
    let input = app.input.text();
    app.dismissed_completion.as_deref() != Some(input.as_str())
        && (command_query(&input).is_some()
            || model_query(&input).is_some()
            || resume_query(&input).is_some())
}

pub(super) fn resume_suggestions(query: &str, choices: &[SessionChoice]) -> Vec<CommandSuggestion> {
    let query = query.to_lowercase();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let now_ms = unix_time_ms();
    choices
        .iter()
        .filter(|choice| {
            let searchable = format!(
                "{} {} {} {} {}",
                choice.id,
                choice.name.as_deref().unwrap_or_default(),
                choice.first_message,
                choice.cwd.display(),
                choice.path.display()
            )
            .to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        })
        .map(|choice| {
            let label = choice
                .name
                .clone()
                .unwrap_or_else(|| choice.first_message.clone());
            let current = if choice.current { " (current)" } else { "" };
            CommandSuggestion {
                invocation: format!("/resume {}", choice.path.display()),
                label: Some(format!("{label}{current}")),
                description: format!(
                    "{} · {} · {}",
                    choice.id,
                    format_session_age(choice.modified_at_ms, now_ms),
                    compact_path(&choice.cwd.to_string_lossy())
                ),
                argument_hint: None,
                apply_on_enter: true,
            }
        })
        .collect()
}

pub(super) fn builtin_help_text() -> String {
    [
        ("/new [path]", "start a new session"),
        ("/resume [query|path]", "list or open sessions"),
        ("/reload", "reload all plugins and resources"),
        ("/trust", "change trust for this project"),
        ("/login [provider]", "configure provider authentication"),
        ("/logout", "remove a stored provider credential"),
        ("/model [provider/model|id]", "list or change model"),
        ("/thinking <level>", "change the thinking level"),
        ("/compact [instructions]", "compact the context"),
        ("/fork", "fork from a previous user message"),
        ("/clone", "clone the session at its current position"),
        ("/tree", "navigate the current session tree"),
        ("/name [name]", "set or show the session name"),
        ("/session", "show session info and stats"),
        ("/copy", "copy the last assistant message"),
        ("/clear", "clear the display"),
        ("/help", "show this command list"),
        ("/quit", "exit"),
        ("!cmd", "run a shell command and include its output"),
        ("!!cmd", "run a shell command without including its output"),
    ]
    .into_iter()
    .map(|(command, description)| format!("- `{command}` — {description}"))
    .fold("Commands\n\n".to_string(), |mut help, line| {
        help.push_str(&line);
        help.push('\n');
        help
    })
}

pub(super) fn model_suggestions(
    query: &str,
    model_specs: &[ModelSpec],
    current_provider: &str,
    current_model: &str,
) -> Vec<CommandSuggestion> {
    let query = query.to_lowercase();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let mut models = model_specs
        .iter()
        .filter(|model| {
            let searchable =
                format!("{}/{} {}", model.provider, model.id, model.name).to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        })
        .collect::<Vec<_>>();
    models.sort_by_key(|model| {
        !(model.provider.as_str() == current_provider && model.id.as_str() == current_model)
    });
    models
        .into_iter()
        .map(|model| {
            let current =
                model.provider.as_str() == current_provider && model.id.as_str() == current_model;
            CommandSuggestion {
                invocation: format!("/model {}/{}", model.provider, model.id),
                label: None,
                description: if current {
                    format!("{} · current", model.name)
                } else {
                    model.name.clone()
                },
                argument_hint: None,
                apply_on_enter: true,
            }
        })
        .collect()
}

pub(super) fn incomplete_command_status(
    input: &str,
    command_specs: &[CommandSpec],
) -> Option<&'static str> {
    let prefix = command_query(input)?;
    let suggestions = command_suggestions(prefix, command_specs);
    if suggestions
        .iter()
        .any(|suggestion| suggestion.invocation == prefix)
    {
        None
    } else if suggestions.is_empty() {
        Some("No matching commands")
    } else {
        Some("Choose a command · ↑↓ select · Tab complete")
    }
}

pub(super) fn spawn_effect(
    session: Arc<AgentSession>,
    session_handle: PiSession,
    epoch: u64,
    input: String,
    mode: EffectMode,
    sender: tokio::sync::mpsc::UnboundedSender<EffectDone>,
) {
    tokio::spawn(async move {
        let refresh_transcript = input.starts_with("/tree ");
        let status = run_effect(&session_handle, &session, input, mode).await;
        let _ = sender.send(EffectDone {
            epoch,
            status,
            refresh_transcript,
        });
    });
}

pub(super) async fn run_effect(
    session_handle: &PiSession,
    session: &AgentSession,
    input: String,
    mode: EffectMode,
) -> Result<String, String> {
    if input == "/clone" {
        let leaf = session
            .log()
            .leaf_id()
            .ok_or_else(|| "Nothing to clone yet".to_string())?;
        session_handle
            .fork_session(leaf, ForkPosition::At)
            .await
            .map_err(|error| error.to_string())?;
        return Ok("Cloned to new session".to_string());
    }
    if let Some(entry_id) = input.strip_prefix("/fork ").map(str::trim) {
        session_handle
            .fork_session(entry_id, ForkPosition::Before)
            .await
            .map_err(|error| error.to_string())?;
        return Ok("Forked to new session".to_string());
    }
    if let Some(entry_id) = input.strip_prefix("/tree ").map(str::trim) {
        session
            .checkout(Some(entry_id))
            .await
            .map_err(|error| error.to_string())?;
        return Ok("Navigated to selected point".to_string());
    }
    if input == "/reload" {
        session_handle
            .reload()
            .await
            .map_err(|error| error.to_string())?;
        return Ok("Reloaded complete session generation".to_string());
    }
    if let Some(arguments) = input.strip_prefix("/compact")
        && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
    {
        let instructions = arguments.trim();
        session
            .compact((!instructions.is_empty()).then(|| instructions.to_string()))
            .await
            .map_err(|error| error.to_string())?;
        return Ok("Compaction complete".to_string());
    }
    if let Some(arguments) = input.strip_prefix("/name")
        && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
    {
        let requested = arguments.trim();
        if requested.is_empty() {
            return session
                .log()
                .name()
                .map(|name| format!("Session name: {name}"))
                .ok_or_else(|| "usage: /name <name>".to_string());
        }
        session
            .set_name(Some(requested.to_string()))
            .await
            .map_err(|error| error.to_string())?;
        return Ok(format!("Session name set: {requested}"));
    }
    if input == "/session" {
        let metadata = session
            .log()
            .metadata()
            .map_err(|error| error.to_string())?;
        let stats = session.log().stats();
        let document = session.log().load().map_err(|error| error.to_string())?;
        let usage = aggregate_session_usage(document.entries.iter().map(|record| &record.entry));
        let name = session
            .log()
            .name()
            .map(|name| format!("Name: {name}\n"))
            .unwrap_or_default();
        return Ok(format!(
            "Session Info\n\n{name}File: {}\nID: {}\nMessages: {}\nTokens: {} total ({} cached, {} uncached)\nCost: ${:.3}",
            metadata.path.display(),
            metadata.id,
            stats.message_count,
            usage.total_tokens,
            usage.cache_read,
            usage.input.saturating_add(usage.cache_write),
            usage.cost.total,
        ));
    }
    if input == "/help" {
        return Ok(builtin_help_text());
    }
    if let Some(arguments) = input.strip_prefix("/new")
        && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
    {
        let requested = arguments.trim();
        let path = if requested.is_empty() {
            session
                .log()
                .path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(format!("{}.jsonl", uuid::Uuid::now_v7()))
        } else {
            std::path::PathBuf::from(requested)
        };
        session_handle
            .new_session(session.runtime().cwd(), &path)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(format!("Created {}", path.display()));
    }
    if input == "/resume" {
        return Err("no resumable sessions found; pass /resume <session.jsonl>".to_string());
    }
    if let Some(path) = input.strip_prefix("/resume ").map(str::trim) {
        if path.is_empty() {
            return Err("usage: /resume <session.jsonl>".to_string());
        }
        session_handle
            .resume_session(path)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(format!("Resumed {path}"));
    }
    if input == "/model" {
        let state = session.runtime().agent().state();
        let models = session.runtime().available_models();
        if models.is_empty() {
            return Ok(format!(
                "Current model: {}/{} · no catalog models loaded",
                state.provider_id, state.model_id
            ));
        }
        let choices = models
            .into_iter()
            .map(|model| {
                let selected = model.provider == state.provider_id && model.id == state.model_id;
                format!(
                    "{}{}/{} ({})",
                    if selected { "› " } else { "  " },
                    model.provider,
                    model.id,
                    model.name
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        return Ok(format!("Models: {choices}"));
    }
    if let Some(model) = input.strip_prefix("/model ").map(str::trim) {
        if model.is_empty() {
            return Err("usage: /model <provider/model|model-id>".to_string());
        }
        let current_provider = session.runtime().agent().state().provider_id;
        let runtime = session.runtime();
        let registered = runtime.resolve_model_reference(&current_provider, model);
        if let Some(registered) = &registered
            && !runtime.provider_is_available(&registered.provider)
        {
            return Err(format!(
                "provider {} is not configured for this generation",
                registered.provider
            ));
        }
        let resolved = runtime.resolve_available_model_reference(&current_provider, model);
        let (provider, model_id) = if let Some(model) = resolved {
            (model.provider, model.id)
        } else if let Some((provider, model_id)) = model.split_once('/') {
            let provider = ProviderId::new(provider);
            if session.runtime().provider_is_available(&provider) {
                (provider, ModelId::new(model_id))
            } else {
                (current_provider, ModelId::new(model))
            }
        } else {
            (current_provider, ModelId::new(model))
        };
        session
            .set_model(provider.clone(), model_id.clone())
            .map_err(|error| error.to_string())?;
        return Ok(format!("Model: {provider}/{model_id}"));
    }
    if let Some(level) = input.strip_prefix("/thinking ").map(str::trim) {
        let level = level.parse().map_err(|error: String| error)?;
        session
            .set_thinking_level(level)
            .map_err(|error| error.to_string())?;
        return Ok(format!("Thinking: {}", level.as_str()));
    }
    if let Some((command, excluded)) = shell_command(&input) {
        let result = session
            .execute_shell(
                command,
                ShellExecutionOptions {
                    exclude_from_context: excluded,
                    ..ShellExecutionOptions::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        return Ok(match result.exit_code {
            Some(code) => format!("Shell exited {code}"),
            None if result.cancelled => "Shell cancelled".to_string(),
            None => "Shell ended".to_string(),
        });
    }
    let outcome = match mode {
        EffectMode::Submit => session.submit(input).await,
        EffectMode::FollowUp => session.follow_up(input).await,
    }
    .map_err(|error| error.to_string())?;
    Ok(match outcome {
        SubmitOutcome::Agent(outcome) => match outcome.stop {
            AgentLoopStop::Completed => "Ready".to_string(),
            AgentLoopStop::Aborted => "Stopped".to_string(),
            AgentLoopStop::ProviderError => "Provider error".to_string(),
            AgentLoopStop::MaxToolIterations => "Tool limit reached".to_string(),
            AgentLoopStop::TerminatedByTools => "Stopped by tool".to_string(),
        },
        SubmitOutcome::Handled => "Ready".to_string(),
        SubmitOutcome::Queued { kind, .. } => format!("Queued {kind:?}"),
        _ => "Ready".to_string(),
    })
}
