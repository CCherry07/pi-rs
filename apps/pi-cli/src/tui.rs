use std::io::{self, BufRead, BufReader, Stdout, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use pi_agent::AgentLoopStop;
use pi_core::{
    AgentEvent, CommandSpec, ContentBlock, Message, ModelId, ModelSpec, ProviderId, StopReason,
    ToolCallId,
};
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionRuntime, AgentSessionSnapshot, QueueSnapshot,
    SessionEntry, ShellExecutionOptions, SubmitOutcome,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};
use ratatui_textarea::{
    CursorMove, Input as TextAreaInput, Key as TextAreaKey, TextArea, WrapMode,
};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc};
use termina::style::RgbColor;
use termina::{Event as TerminaEvent, PlatformTerminal, Terminal as _};
use tui_widget_list::{ListBuilder, ListState, ListView};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::clipboard::{ClipboardWriter, SystemClipboard};
use crate::output::{assistant_text, shell_command};
use crate::project_trust::{ProjectTrustOption, ProjectTrustPromptRequest, ProjectTrustService};
use crate::transcript_selection::{TranscriptSelection, TranscriptSurface};

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

const TERMINAL_COLOR_QUERY_TIMEOUT: Duration = Duration::from_millis(80);
const ACTIVITY_ANIMATION_INTERVAL: Duration = Duration::from_millis(220);
const ACTIVITY_FRAME_COUNT: usize = 4;
const STATUS_DOT: &str = "•";
const MOUSE_SCROLL_LINES: usize = 3;

pub async fn select_project_trust(
    fullscreen: bool,
    cwd: &Path,
    options: &[ProjectTrustOption],
) -> Result<Option<usize>, String> {
    let mut terminal = setup_terminal(fullscreen).map_err(|error| error.to_string())?;
    let result = select_project_trust_loop(&mut terminal, cwd, options).await;
    let restored = restore_terminal(&mut terminal, fullscreen).map_err(|error| error.to_string());
    match (result, restored) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(selected), Ok(())) => Ok(selected),
    }
}

async fn select_project_trust_loop(
    terminal: &mut TuiTerminal,
    cwd: &Path,
    options: &[ProjectTrustOption],
) -> Result<Option<usize>, String> {
    let mut selected = 0usize;
    let mut events = EventStream::new();
    loop {
        terminal
            .draw(|frame| draw_project_trust_prompt(frame, cwd, options, selected))
            .map_err(|error| error.to_string())?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Up => {
                    selected = selected
                        .checked_sub(1)
                        .unwrap_or_else(|| options.len().saturating_sub(1));
                }
                KeyCode::Down => {
                    selected = if selected + 1 >= options.len() {
                        0
                    } else {
                        selected + 1
                    };
                }
                KeyCode::Enter => return Ok(Some(selected)),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                _ => {}
            },
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.to_string()),
            None => return Ok(None),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalAppearance {
    Light,
    Dark,
}

fn code_block_background(appearance: TerminalAppearance) -> Color {
    markdown_theme(appearance).code_background()
}

fn markdown_theme(appearance: TerminalAppearance) -> pi_agent_md::MarkdownTheme {
    let appearance = match appearance {
        TerminalAppearance::Light => pi_agent_md::Appearance::Light,
        TerminalAppearance::Dark => pi_agent_md::Appearance::Dark,
    };
    pi_agent_md::MarkdownTheme::new(appearance)
}

#[derive(Clone, Debug)]
struct ComposerInput {
    editor: TextArea<'static>,
}

impl Default for ComposerInput {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl ComposerInput {
    fn from_text(text: &str) -> Self {
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

    fn text(&self) -> String {
        self.editor.lines().join("\n")
    }

    fn set_text(&mut self, text: impl AsRef<str>) {
        *self = Self::from_text(text.as_ref());
    }

    fn take_text(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }

    fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn insert_newline(&mut self) {
        self.editor.insert_newline();
    }

    fn insert_str(&mut self, text: impl AsRef<str>) {
        self.editor.insert_str(text);
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.editor.delete_line_by_head(),
            (KeyCode::Char('-' | '_'), KeyModifiers::CONTROL) => self.editor.undo(),
            _ => self.editor.input(textarea_input(key)),
        }
    }

    fn widget(&self) -> &TextArea<'static> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiPalette {
    composer_background: Option<Color>,
    terminal_appearance: TerminalAppearance,
}

impl UiPalette {
    fn detect() -> Self {
        let background = query_terminal_background().or_else(background_from_colorfgbg);
        Self::from_background(background)
    }

    fn from_background(background: Option<RgbColor>) -> Self {
        let appearance = background
            .map(terminal_appearance)
            .unwrap_or(TerminalAppearance::Dark);
        Self {
            composer_background: background.map(composer_surface_color),
            terminal_appearance: appearance,
        }
    }
}

fn selection_background(appearance: TerminalAppearance) -> Color {
    match appearance {
        TerminalAppearance::Light => Color::Rgb(161, 204, 251),
        TerminalAppearance::Dark => Color::Rgb(10, 78, 152),
    }
}

fn query_terminal_background() -> Option<RgbColor> {
    let mut terminal = PlatformTerminal::new().ok()?;
    terminal.enter_raw_mode().ok()?;
    let result = (|| -> io::Result<Option<RgbColor>> {
        let query = Osc::ChangeDynamicColors(
            DynamicColorNumber::TextBackgroundColor,
            vec![ColorOrQuery::Query],
        );
        write!(terminal, "{query}")?;
        terminal.flush()?;
        if !terminal.poll(
            is_background_color_response,
            Some(TERMINAL_COLOR_QUERY_TIMEOUT),
        )? {
            return Ok(None);
        }
        Ok(background_from_terminal_event(
            terminal.read(is_background_color_response)?,
        ))
    })();
    let _ = terminal.enter_cooked_mode();
    result.ok().flatten()
}

fn is_background_color_response(event: &TerminaEvent) -> bool {
    matches!(
        event,
        TerminaEvent::Osc(Osc::ChangeDynamicColors(
            DynamicColorNumber::TextBackgroundColor,
            colors,
        )) if colors.iter().any(|color| matches!(color, ColorOrQuery::Color(_)))
    )
}

fn background_from_terminal_event(event: TerminaEvent) -> Option<RgbColor> {
    let TerminaEvent::Osc(Osc::ChangeDynamicColors(
        DynamicColorNumber::TextBackgroundColor,
        colors,
    )) = event
    else {
        return None;
    };
    colors.into_iter().find_map(|color| match color {
        ColorOrQuery::Color(color) => Some(color),
        ColorOrQuery::Query => None,
    })
}

fn background_from_colorfgbg() -> Option<RgbColor> {
    let value = std::env::var("COLORFGBG").ok()?;
    colorfgbg_background(&value)
}

fn colorfgbg_background(value: &str) -> Option<RgbColor> {
    let index = value.rsplit(';').next()?.trim().parse::<u8>().ok()?;
    const ANSI: [RgbColor; 16] = [
        RgbColor::new(0, 0, 0),
        RgbColor::new(128, 0, 0),
        RgbColor::new(0, 128, 0),
        RgbColor::new(128, 128, 0),
        RgbColor::new(0, 0, 128),
        RgbColor::new(128, 0, 128),
        RgbColor::new(0, 128, 128),
        RgbColor::new(192, 192, 192),
        RgbColor::new(128, 128, 128),
        RgbColor::new(255, 0, 0),
        RgbColor::new(0, 255, 0),
        RgbColor::new(255, 255, 0),
        RgbColor::new(0, 0, 255),
        RgbColor::new(255, 0, 255),
        RgbColor::new(0, 255, 255),
        RgbColor::new(255, 255, 255),
    ];
    ANSI.get(usize::from(index)).copied()
}

fn composer_surface_color(background: RgbColor) -> Color {
    let (target, amount) = if terminal_appearance(background) == TerminalAppearance::Light {
        (0, 4)
    } else {
        (255, 7)
    };
    Color::Rgb(
        blend_channel(background.red, target, amount),
        blend_channel(background.green, target, amount),
        blend_channel(background.blue, target, amount),
    )
}

fn terminal_appearance(background: RgbColor) -> TerminalAppearance {
    let luminance = u32::from(background.red) * 299
        + u32::from(background.green) * 587
        + u32::from(background.blue) * 114;
    if luminance >= 128_000 {
        TerminalAppearance::Light
    } else {
        TerminalAppearance::Dark
    }
}

fn blend_channel(value: u8, target: u8, percent: u16) -> u8 {
    let value = i32::from(value);
    let target = i32::from(target);
    let blended = value + (target - value) * i32::from(percent) / 100;
    u8::try_from(blended).unwrap_or(target as u8)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolState {
    Pending,
    Running,
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellState {
    Running,
    Finished {
        exit_code: Option<i32>,
        cancelled: bool,
        timed_out: bool,
        truncated: bool,
    },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TranscriptItem {
    User(String),
    Assistant {
        text: String,
        streaming: bool,
        error: Option<String>,
    },
    Tool {
        id: ToolCallId,
        name: String,
        detail: Option<String>,
        input: Option<String>,
        output: Option<String>,
        state: ToolState,
    },
    Shell {
        command: String,
        output: String,
        excluded_from_context: bool,
        state: ShellState,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct InputHistory {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: String,
}

impl InputHistory {
    fn from_transcript(transcript: &[TranscriptItem]) -> Self {
        let mut history = Self::default();
        for item in transcript {
            if let TranscriptItem::User(input) = item {
                history.record(input);
            }
        }
        history
    }

    fn record(&mut self, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        if self.entries.last().is_none_or(|previous| previous != input) {
            self.entries.push(input.to_string());
        }
        self.reset_navigation();
    }

    fn older(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let index = match self.cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
        };
        self.cursor = Some(index);
        Some(self.entries[index].clone())
    }

    fn newer(&mut self) -> Option<String> {
        let index = self.cursor?;
        if index + 1 < self.entries.len() {
            let next = index + 1;
            self.cursor = Some(next);
            Some(self.entries[next].clone())
        } else {
            self.cursor = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    fn reset_navigation(&mut self) {
        self.cursor = None;
        self.draft.clear();
    }

    fn is_browsing(&self) -> bool {
        self.cursor.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionChoice {
    path: PathBuf,
    id: String,
    cwd: PathBuf,
    name: Option<String>,
    first_message: String,
    message_count: u64,
    modified_at_ms: u64,
    current: bool,
}

struct App {
    transcript: Vec<TranscriptItem>,
    input: ComposerInput,
    input_history: InputHistory,
    status: String,
    queue: QueueSnapshot,
    command_specs: Vec<CommandSpec>,
    model_specs: Vec<ModelSpec>,
    session_choices: Vec<SessionChoice>,
    command_selection: usize,
    streaming_assistant: Option<usize>,
    awaiting_assistant: bool,
    bash_line: Option<usize>,
    provider: String,
    model: String,
    thinking: String,
    cwd: String,
    session_name: Option<String>,
    session_tokens: u64,
    context_tokens: u64,
    is_running: bool,
    compacting: bool,
    tools_expanded: bool,
    animation_frame: usize,
    scroll_from_bottom: usize,
    transcript_selection: Option<TranscriptSelection>,
    trust_prompt: Option<TrustPromptState>,
    epoch: u64,
    quit: bool,
}

impl App {
    fn new(session: &AgentSession, snapshot: &AgentSessionSnapshot) -> Self {
        let mut transcript = Vec::new();
        let mut session_tokens: u64 = 0;
        if let Ok(document) = session.log().load()
            && let Ok(branch) = document.branch()
        {
            for record in branch {
                push_history_entry(&mut transcript, &record.entry);
                if let SessionEntry::Message(entry) = &record.entry {
                    session_tokens = session_tokens
                        .saturating_add(entry.message.as_standard().map_or(0, message_token_usage));
                }
            }
        }
        let context_tokens = latest_context_usage(&snapshot.agent.messages);
        let streaming_assistant = snapshot
            .agent
            .streaming_message
            .as_ref()
            .and_then(|message| {
                let text = assistant_text(&Message::assistant(message.clone()))?;
                if text.is_empty() {
                    return None;
                }
                transcript.push(TranscriptItem::Assistant {
                    text,
                    streaming: true,
                    error: None,
                });
                Some(transcript.len() - 1)
            });
        let bash_line = snapshot.bash.as_ref().map(|bash| {
            transcript.push(TranscriptItem::Shell {
                command: bash.command.clone(),
                output: bash.output.clone(),
                excluded_from_context: bash.exclude_from_context,
                state: ShellState::Running,
            });
            transcript.len() - 1
        });
        let compacting = snapshot.compaction.is_some();
        let status = if compacting {
            "Compacting context…"
        } else if bash_line.is_some() {
            "Shell running… Esc cancels"
        } else if snapshot.agent.is_running {
            "Agent running… Esc stops"
        } else {
            "Ready"
        };
        let awaiting_assistant = snapshot.agent.is_running
            && streaming_assistant.is_none()
            && snapshot.agent.pending_tool_calls.is_empty()
            && bash_line.is_none()
            && !compacting;
        let session_choices =
            discover_session_choices(session.log().path(), session.runtime().cwd());
        let input_history = InputHistory::from_transcript(&transcript);
        Self {
            transcript,
            input: ComposerInput::default(),
            input_history,
            status: status.to_string(),
            queue: snapshot.queue.clone(),
            command_specs: session.runtime().command_specs(),
            model_specs: session.runtime().models(),
            session_choices,
            command_selection: 0,
            streaming_assistant,
            awaiting_assistant,
            bash_line,
            provider: snapshot.agent.provider_id.to_string(),
            model: snapshot.agent.model_id.to_string(),
            thinking: snapshot.agent.thinking_level.as_str().to_string(),
            cwd: session.runtime().cwd().display().to_string(),
            session_name: snapshot.name.clone(),
            session_tokens,
            context_tokens,
            is_running: snapshot.agent.is_running,
            compacting,
            tools_expanded: false,
            animation_frame: 0,
            scroll_from_bottom: 0,
            transcript_selection: None,
            trust_prompt: None,
            epoch: 1,
            quit: false,
        }
    }

    fn sync_snapshot(&mut self, snapshot: &AgentSessionSnapshot) {
        self.queue = snapshot.queue.clone();
        self.provider = snapshot.agent.provider_id.to_string();
        self.model = snapshot.agent.model_id.to_string();
        self.thinking = snapshot.agent.thinking_level.as_str().to_string();
        self.session_name = snapshot.name.clone();
        self.context_tokens = latest_context_usage(&snapshot.agent.messages);
        self.is_running = snapshot.agent.is_running;
        self.compacting = snapshot.compaction.is_some();
        if !self.is_running {
            self.awaiting_assistant = false;
        }
    }

    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.streaming_assistant = None;
        self.awaiting_assistant = false;
        self.bash_line = None;
        self.scroll_from_bottom = 0;
        self.transcript_selection = None;
    }

    fn has_active_animation(&self) -> bool {
        self.awaiting_assistant
            || self.streaming_assistant.is_some_and(|index| {
                matches!(
                    self.transcript.get(index),
                    Some(TranscriptItem::Assistant {
                        streaming: true,
                        ..
                    })
                )
            })
    }

    fn advance_animation(&mut self) {
        self.animation_frame = (self.animation_frame + 1) % ACTIVITY_FRAME_COUNT;
    }

    fn apply_session_event(&mut self, event: AgentSessionEvent) {
        match event {
            AgentSessionEvent::Agent(event) => self.apply_agent_event(*event),
            AgentSessionEvent::AgentSettled => {
                self.awaiting_assistant = false;
                if let Some(index) = self.streaming_assistant.take()
                    && let Some(TranscriptItem::Assistant { streaming, .. }) =
                        self.transcript.get_mut(index)
                {
                    *streaming = false;
                }
                self.is_running = false;
                self.status = "Ready".to_string();
            }
            AgentSessionEvent::QueueUpdate {
                steering,
                follow_up,
            } => {
                self.queue = QueueSnapshot {
                    steering,
                    follow_up,
                };
            }
            AgentSessionEvent::CompactionStart { .. } => {
                self.awaiting_assistant = false;
                self.compacting = true;
                self.status = "Compacting context…".to_string();
            }
            AgentSessionEvent::CompactionEnd { error_message, .. } => {
                self.compacting = false;
                self.status = error_message.unwrap_or_else(|| "Compaction complete".to_string());
            }
            AgentSessionEvent::ThinkingLevelChanged { level } => {
                self.status = format!("Thinking: {}", level.as_str());
            }
            AgentSessionEvent::BashExecutionStart {
                command,
                exclude_from_context,
                ..
            } => {
                self.awaiting_assistant = false;
                self.transcript.push(TranscriptItem::Shell {
                    command,
                    output: String::new(),
                    excluded_from_context: exclude_from_context,
                    state: ShellState::Running,
                });
                self.bash_line = Some(self.transcript.len() - 1);
                self.status = "Shell running… Esc cancels".to_string();
            }
            AgentSessionEvent::BashExecutionUpdate { delta, .. } => {
                if let Some(index) = self.bash_line
                    && let Some(TranscriptItem::Shell { output, .. }) =
                        self.transcript.get_mut(index)
                {
                    output.push_str(&delta);
                }
            }
            AgentSessionEvent::BashExecutionEnd {
                result,
                error_message,
                ..
            } => {
                if let Some(index) = self.bash_line.take()
                    && let Some(TranscriptItem::Shell { output, state, .. }) =
                        self.transcript.get_mut(index)
                {
                    if let Some(error) = error_message.as_ref() {
                        *state = ShellState::Failed(error.clone());
                    } else if let Some(result) = result.as_ref() {
                        *output = result.output.clone();
                        *state = ShellState::Finished {
                            exit_code: result.exit_code,
                            cancelled: result.cancelled,
                            timed_out: result.timed_out,
                            truncated: result.truncated,
                        };
                    }
                }
                self.status = error_message.unwrap_or_else(|| "Shell complete".to_string());
            }
            AgentSessionEvent::SessionInfoChanged { name } => {
                self.session_name = name.clone();
                self.status = name.map_or_else(
                    || "Session name cleared".to_string(),
                    |name| format!("Session: {name}"),
                );
            }
            _ => {}
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AgentStart => {
                self.awaiting_assistant = true;
                self.is_running = true;
                self.status = "Agent running… Esc stops".to_string();
            }
            AgentEvent::MessageStart { message } => match message {
                Message::User(user) => {
                    self.transcript
                        .push(TranscriptItem::User(user_message_display_text(
                            &user.content,
                        )));
                }
                Message::Assistant(_) => {
                    self.awaiting_assistant = false;
                    self.transcript.push(TranscriptItem::Assistant {
                        text: String::new(),
                        streaming: true,
                        error: None,
                    });
                    self.streaming_assistant = Some(self.transcript.len() - 1);
                }
                Message::ToolResult(_) => {}
            },
            AgentEvent::MessageUpdate { message, .. } => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if let Some(index) = self.streaming_assistant
                    && let Some(TranscriptItem::Assistant {
                        text: current,
                        streaming,
                        ..
                    }) = self.transcript.get_mut(index)
                {
                    *current = text;
                    *streaming = true;
                }
            }
            AgentEvent::MessageEnd {
                message: Message::Assistant(message),
            } => {
                self.session_tokens = self
                    .session_tokens
                    .saturating_add(assistant_token_usage(&message));
                self.context_tokens = assistant_context_usage(&message);
                let text = assistant_text(&Message::Assistant(message.clone())).unwrap_or_default();
                let error = assistant_error(&message);
                if let Some(index) = self.streaming_assistant.take() {
                    if text.trim().is_empty()
                        && error.is_none()
                        && index + 1 == self.transcript.len()
                    {
                        self.transcript.pop();
                    } else if let Some(TranscriptItem::Assistant {
                        text: current,
                        streaming,
                        error: current_error,
                    }) = self.transcript.get_mut(index)
                    {
                        *current = text;
                        *streaming = false;
                        *current_error = error;
                    }
                } else if !text.trim().is_empty() || error.is_some() {
                    self.transcript.push(TranscriptItem::Assistant {
                        text,
                        streaming: false,
                        error,
                    });
                }
                for call in message.tool_calls() {
                    self.upsert_tool(
                        call.id,
                        call.name,
                        summarize_tool_args(&call.arguments),
                        format_tool_input(&call.arguments),
                        None,
                        ToolState::Pending,
                    );
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.awaiting_assistant = false;
                self.upsert_tool(
                    tool_call_id,
                    tool_name.clone(),
                    summarize_tool_args(&args),
                    format_tool_input(&args),
                    None,
                    ToolState::Running,
                );
                self.status = format!("Running {tool_name}…");
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let state = if is_error {
                    ToolState::Failed(tool_result_text(&result))
                } else {
                    ToolState::Succeeded
                };
                self.upsert_tool(
                    tool_call_id,
                    tool_name.clone(),
                    None,
                    None,
                    Some(format_tool_result(&result)),
                    state,
                );
                self.status = if is_error {
                    format!("{tool_name} failed")
                } else {
                    format!("{tool_name} complete")
                };
            }
            AgentEvent::AgentEnd { .. } => {
                self.awaiting_assistant = false;
                self.is_running = false;
            }
            AgentEvent::ToolExecutionUpdate { tool_name, .. } => {
                self.status = format!("Running {tool_name}…");
            }
            AgentEvent::TurnStart => self.awaiting_assistant = true,
            AgentEvent::TurnEnd { .. } | AgentEvent::MessageEnd { .. } => {}
        }
    }

    fn upsert_tool(
        &mut self,
        id: ToolCallId,
        name: String,
        detail: Option<String>,
        input: Option<String>,
        output: Option<String>,
        state: ToolState,
    ) {
        if let Some(TranscriptItem::Tool {
            name: current_name,
            detail: current_detail,
            input: current_input,
            output: current_output,
            state: current_state,
            ..
        }) = self.transcript.iter_mut().rev().find(
            |item| matches!(item, TranscriptItem::Tool { id: current_id, .. } if current_id == &id),
        ) {
            *current_name = name;
            if detail.is_some() {
                *current_detail = detail;
            }
            if input.is_some() {
                *current_input = input;
            }
            if output.is_some() {
                *current_output = output;
            }
            *current_state = state;
            return;
        }
        self.transcript.push(TranscriptItem::Tool {
            id,
            name,
            detail,
            input,
            output,
            state,
        });
    }
}

struct TrustPromptState {
    cwd: PathBuf,
    options: Vec<ProjectTrustOption>,
    selected: usize,
    response: Option<tokio::sync::oneshot::Sender<Option<usize>>>,
}

impl From<ProjectTrustPromptRequest> for TrustPromptState {
    fn from(request: ProjectTrustPromptRequest) -> Self {
        Self {
            cwd: request.cwd,
            options: request.options,
            selected: 0,
            response: Some(request.response),
        }
    }
}

enum EffectMode {
    Submit,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSuggestion {
    invocation: String,
    label: Option<String>,
    description: String,
    argument_hint: Option<String>,
    apply_on_enter: bool,
}

struct EffectDone {
    epoch: u64,
    status: Result<String, String>,
}

pub async fn run(
    runtime: AgentSessionRuntime,
    fullscreen: bool,
    initial_prompt: Option<String>,
    project_trust: ProjectTrustService,
    trust_requests: tokio::sync::mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
) -> Result<(), String> {
    let palette = UiPalette::detect();
    let mut terminal = setup_terminal(fullscreen).map_err(|error| error.to_string())?;
    let result = run_loop(
        &mut terminal,
        runtime,
        initial_prompt,
        palette,
        project_trust,
        trust_requests,
    )
    .await;
    restore_terminal(&mut terminal, fullscreen).map_err(|error| error.to_string())?;
    result
}

async fn run_loop(
    terminal: &mut TuiTerminal,
    runtime: AgentSessionRuntime,
    initial_prompt: Option<String>,
    palette: UiPalette,
    project_trust: ProjectTrustService,
    mut trust_requests: tokio::sync::mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
) -> Result<(), String> {
    let mut session_changes = runtime.subscribe();
    let mut session = runtime.session();
    let mut subscription = session.subscribe();
    let mut app = App::new(&session, &subscription.snapshot);
    let (effect_sender, mut effect_receiver) = tokio::sync::mpsc::unbounded_channel();
    if let Some(prompt) = initial_prompt {
        app.input_history.record(&prompt);
        app.awaiting_assistant = true;
        app.status = "Working…".to_string();
        spawn_effect(
            Arc::clone(&session),
            runtime.clone(),
            app.epoch,
            prompt,
            EffectMode::Submit,
            effect_sender.clone(),
        );
    }
    let mut events = EventStream::new();
    let mut clipboard = SystemClipboard::default();
    let mut animation_tick = tokio::time::interval(ACTIVITY_ANIMATION_INTERVAL);
    animation_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !app.quit {
        let surface = {
            let completed = terminal
                .draw(|frame| draw(frame, &app, palette))
                .map_err(|error| error.to_string())?;
            let areas = ui_areas(completed.area, &app);
            TranscriptSurface::capture(completed.buffer, areas.transcript)
        };
        tokio::select! {
            terminal_event = events.next() => {
                match terminal_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        if app.trust_prompt.is_some() {
                            handle_key(
                                key,
                                &mut app,
                                &session,
                                &runtime,
                                &project_trust,
                                &effect_sender,
                            );
                        } else if !handle_copy_shortcut(key, &mut app, &surface, &mut clipboard) {
                            app.transcript_selection = None;
                            handle_key(
                                key,
                                &mut app,
                                &session,
                                &runtime,
                                &project_trust,
                                &effect_sender,
                            );
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) if app.trust_prompt.is_none() => {
                        handle_mouse(mouse, &mut app, &surface)
                    }
                    Some(Ok(Event::Paste(text))) if app.trust_prompt.is_none() => {
                        app.transcript_selection = None;
                        app.input.insert_str(text);
                        app.input_history.reset_navigation();
                        app.command_selection = 0;
                    }
                    Some(Ok(Event::Resize(_, _))) => app.transcript_selection = None,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.to_string()),
                    None => app.quit = true,
                }
            }
            session_event = subscription.events.recv() => match session_event {
                Ok(event) if event.revision > subscription.snapshot.revision => {
                    app.transcript_selection = None;
                    subscription.snapshot.revision = event.revision;
                    app.apply_session_event(event.event);
                    app.sync_snapshot(&session.snapshot());
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let snapshot = session.snapshot();
                    subscription.snapshot = snapshot.clone();
                    let input = std::mem::take(&mut app.input);
                    let input_history = std::mem::take(&mut app.input_history);
                    let epoch = app.epoch;
                    let animation_frame = app.animation_frame;
                    let tools_expanded = app.tools_expanded;
                    let scroll_from_bottom = app.scroll_from_bottom;
                    let trust_prompt = app.trust_prompt.take();
                    let mut recovered = App::new(&session, &snapshot);
                    recovered.input = input;
                    recovered.input_history = input_history;
                    recovered.epoch = epoch;
                    recovered.animation_frame = animation_frame;
                    recovered.tools_expanded = tools_expanded;
                    recovered.scroll_from_bottom = scroll_from_bottom;
                    recovered.trust_prompt = trust_prompt;
                    recovered.status = "Caught up after UI lag".to_string();
                    app = recovered;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => app.quit = true,
            },
            Some(done) = effect_receiver.recv() => {
                if done.epoch == app.epoch {
                    app.awaiting_assistant = false;
                    app.status = done.status.unwrap_or_else(|error| format!("Error: {error}"));
                    app.sync_snapshot(&session.snapshot());
                }
            }
            Some(request) = trust_requests.recv() => {
                app.trust_prompt = Some(request.into());
                app.status = "Choose project trust".to_string();
            }
            _ = animation_tick.tick(), if app.has_active_animation() => {
                app.advance_animation();
            }
            changed = session_changes.changed() => {
                if changed.is_err() {
                    app.quit = true;
                    continue;
                }
                session = runtime.session();
                subscription = session.subscribe();
                let next_epoch = app.epoch.saturating_add(1);
                let tools_expanded = app.tools_expanded;
                app = App::new(&session, &subscription.snapshot);
                app.epoch = next_epoch;
                app.tools_expanded = tools_expanded;
                app.status = "Session generation replaced".to_string();
            }
        }
    }
    session.abort();
    session.abort_shell();
    Ok(())
}

fn handle_key(
    key: KeyEvent,
    app: &mut App,
    session: &Arc<AgentSession>,
    runtime: &AgentSessionRuntime,
    project_trust: &ProjectTrustService,
    sender: &tokio::sync::mpsc::UnboundedSender<EffectDone>,
) {
    if handle_trust_prompt_key(key, app, project_trust) {
        return;
    }
    if handle_tool_output_key(key, app) {
        return;
    }
    if handle_vertical_navigation(key.code, app) {
        return;
    }
    if is_newline_key(key) {
        app.input.insert_newline();
        app.input_history.reset_navigation();
        app.command_selection = 0;
        return;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) if app.input.is_empty() => app.quit = true,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.input.clear();
            app.input_history.reset_navigation();
            app.command_selection = 0;
        }
        (KeyCode::Tab, _) => {
            complete_selected_command(app);
        }
        (KeyCode::PageUp, _) => {
            app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(10);
        }
        (KeyCode::PageDown, _) => {
            app.scroll_from_bottom = app.scroll_from_bottom.saturating_sub(10);
        }
        (KeyCode::End, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_from_bottom = 0;
        }
        (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::ALT) => {
            submit_editor(
                app,
                session,
                runtime,
                project_trust,
                sender,
                EffectMode::FollowUp,
            );
        }
        (KeyCode::Enter, _) => {
            if !complete_selected_command_for_enter(app) {
                submit_editor(
                    app,
                    session,
                    runtime,
                    project_trust,
                    sender,
                    EffectMode::Submit,
                );
            }
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
            app.command_selection = 0;
            app.status = "Abort requested".to_string();
        }
        _ => {
            app.input.handle_key(key);
            app.input_history.reset_navigation();
            app.command_selection = 0;
        }
    }
}

fn handle_trust_prompt_key(
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

fn handle_tool_output_key(key: KeyEvent, app: &mut App) -> bool {
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

fn is_copy_shortcut(key: KeyEvent) -> bool {
    let KeyCode::Char(character) = key.code else {
        return false;
    };
    character.eq_ignore_ascii_case(&'c')
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT)))
}

fn handle_copy_shortcut(
    key: KeyEvent,
    app: &mut App,
    surface: &TranscriptSurface,
    clipboard: &mut impl ClipboardWriter,
) -> bool {
    if !is_copy_shortcut(key) {
        return false;
    }
    let Some(selection) = app.transcript_selection else {
        app.status = "No transcript text selected".to_string();
        return true;
    };
    let Some(text) = surface.selected_text(selection) else {
        app.status = "No transcript text selected".to_string();
        return true;
    };
    match clipboard.set_text(&text) {
        Ok(()) => app.status = format!("Copied {} characters", text.chars().count()),
        Err(error) => app.status = format!("Copy failed: {error}"),
    }
    true
}

fn is_newline_key(key: KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('j'), KeyModifiers::CONTROL) | (KeyCode::Enter, KeyModifiers::SHIFT)
    )
}

fn handle_vertical_navigation(code: KeyCode, app: &mut App) -> bool {
    let has_command_suggestions = !suggestions_for_app(app).is_empty();
    match code {
        KeyCode::Up if has_command_suggestions => move_command_selection(app, -1),
        KeyCode::Down if has_command_suggestions => move_command_selection(app, 1),
        KeyCode::Up => {
            let current = app.input.text();
            if let Some(input) = app.input_history.older(&current) {
                app.input.set_text(input);
                app.command_selection = 0;
            } else {
                return false;
            }
        }
        KeyCode::Down if app.input_history.is_browsing() => {
            if let Some(input) = app.input_history.newer() {
                app.input.set_text(input);
                app.command_selection = 0;
            }
        }
        _ => return false,
    }
    true
}

fn handle_mouse(mouse: MouseEvent, app: &mut App, surface: &TranscriptSurface) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.transcript_selection = None;
            app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(MOUSE_SCROLL_LINES);
        }
        MouseEventKind::ScrollDown => {
            app.transcript_selection = None;
            app.scroll_from_bottom = app.scroll_from_bottom.saturating_sub(MOUSE_SCROLL_LINES);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.transcript_selection =
                TranscriptSelection::begin(surface, Position::new(mouse.column, mouse.row));
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(selection) = &mut app.transcript_selection {
                selection.drag_to(surface, Position::new(mouse.column, mouse.row));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let keep = app.transcript_selection.as_mut().is_some_and(|selection| {
                selection.finish(surface, Position::new(mouse.column, mouse.row))
            });
            if !keep {
                app.transcript_selection = None;
            }
        }
        _ => {}
    }
}

fn submit_editor(
    app: &mut App,
    session: &Arc<AgentSession>,
    runtime: &AgentSessionRuntime,
    project_trust: &ProjectTrustService,
    sender: &tokio::sync::mpsc::UnboundedSender<EffectDone>,
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
    if input == "/quit" {
        app.quit = true;
        return;
    }
    if input == "/clear" {
        app.clear_transcript();
        app.input.clear();
        app.command_selection = 0;
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
        app.command_selection = 0;
        return;
    }
    app.input.clear();
    app.command_selection = 0;
    app.awaiting_assistant = true;
    app.status = "Working…".to_string();
    spawn_effect(
        Arc::clone(session),
        runtime.clone(),
        app.epoch,
        input,
        mode,
        sender.clone(),
    );
}

fn move_command_selection(app: &mut App, delta: i8) {
    let count = suggestions_for_app(app).len();
    if count == 0 {
        app.command_selection = 0;
        return;
    }
    let current = app.command_selection.min(count - 1);
    app.command_selection = if delta < 0 {
        current.checked_sub(1).unwrap_or(count - 1)
    } else if current + 1 == count {
        0
    } else {
        current + 1
    };
}

fn selected_command(app: &App) -> Option<CommandSuggestion> {
    let suggestions = suggestions_for_app(app);
    let index = app.command_selection.min(suggestions.len().checked_sub(1)?);
    suggestions.into_iter().nth(index)
}

fn complete_selected_command(app: &mut App) -> bool {
    let Some(suggestion) = selected_command(app) else {
        return false;
    };
    let mut input = suggestion.invocation;
    if suggestion.argument_hint.is_some() {
        input.push(' ');
    }
    app.input.set_text(input);
    app.input_history.reset_navigation();
    app.command_selection = 0;
    true
}

fn complete_selected_command_for_enter(app: &mut App) -> bool {
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

fn command_query(input: &str) -> Option<&str> {
    let prefix = input.trim();
    if prefix.starts_with('/') && !prefix.contains(char::is_whitespace) {
        Some(prefix)
    } else {
        None
    }
}

fn model_query(input: &str) -> Option<&str> {
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

fn resume_query(input: &str) -> Option<&str> {
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

fn suggestions_for_app(app: &App) -> Vec<CommandSuggestion> {
    if let Some(query) = model_query(&app.input.text()) {
        return model_suggestions(
            query,
            &app.model_specs,
            app.provider.as_str(),
            app.model.as_str(),
        );
    }
    if let Some(query) = resume_query(&app.input.text()) {
        return resume_suggestions(query, &app.session_choices);
    }
    command_suggestions(&app.input.text(), &app.command_specs)
}

fn resume_suggestions(query: &str, choices: &[SessionChoice]) -> Vec<CommandSuggestion> {
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
            let current = if choice.current { "current · " } else { "" };
            CommandSuggestion {
                invocation: format!("/resume {}", choice.path.display()),
                label: Some(label),
                description: format!(
                    "{current}{} msgs · {} · {}",
                    choice.message_count,
                    format_session_age(choice.modified_at_ms, now_ms),
                    compact_path(&choice.cwd.to_string_lossy())
                ),
                argument_hint: None,
                apply_on_enter: true,
            }
        })
        .collect()
}

fn model_suggestions(
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

fn incomplete_command_status(input: &str, command_specs: &[CommandSpec]) -> Option<&'static str> {
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

fn spawn_effect(
    session: Arc<AgentSession>,
    runtime: AgentSessionRuntime,
    epoch: u64,
    input: String,
    mode: EffectMode,
    sender: tokio::sync::mpsc::UnboundedSender<EffectDone>,
) {
    tokio::spawn(async move {
        let status = run_effect(&runtime, &session, input, mode).await;
        let _ = sender.send(EffectDone { epoch, status });
    });
}

async fn run_effect(
    runtime: &AgentSessionRuntime,
    session: &AgentSession,
    input: String,
    mode: EffectMode,
) -> Result<String, String> {
    if input == "/reload" {
        runtime.reload().await.map_err(|error| error.to_string())?;
        return Ok("Reloaded complete session generation".to_string());
    }
    if input == "/compact" {
        session
            .compact(None)
            .await
            .map_err(|error| error.to_string())?;
        return Ok("Compaction complete".to_string());
    }
    if input == "/help" {
        return Ok("Commands: /new [path] · /resume [query|path] · /reload · /trust · /model [provider/model|id] · /thinking <level> · /compact · /clear · /quit · !cmd · !!cmd".to_string());
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
        runtime
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
        runtime
            .switch_session(path)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(format!("Resumed {path}"));
    }
    if input == "/model" {
        let state = session.runtime().agent().state();
        let models = session.runtime().models();
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
        let resolved = session
            .runtime()
            .resolve_model_reference(&current_provider, model);
        let (provider, model_id) = if let Some(model) = resolved {
            (model.provider, model.id)
        } else if let Some((provider, model_id)) = model.split_once('/') {
            let provider = ProviderId::new(provider);
            if session.runtime().has_provider(&provider) {
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct UiAreas {
    transcript: Rect,
    context: Rect,
    composer: Rect,
    footer: Rect,
    gutter: u16,
}

fn ui_areas(root: Rect, app: &App) -> UiAreas {
    if root.is_empty() {
        return UiAreas::default();
    }

    let gutter = horizontal_gutter(root.width);
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
    let footer_height = u16::from(root.height >= 5);
    let available_after_chrome = root.height.saturating_sub(footer_height);
    let composer_height = desired_composer_height
        .min(available_after_chrome.saturating_sub(1))
        .max(available_after_chrome.min(3));
    let suggestions = suggestions_for_app(app);
    let queue_count = app.queue.steering.len() + app.queue.follow_up.len();
    let has_suggestion_query = command_query(&input).is_some()
        || model_query(&input).is_some()
        || resume_query(&input).is_some();
    let context_count = if suggestions.is_empty() && has_suggestion_query {
        1
    } else if suggestions.is_empty() {
        queue_count.min(5)
    } else {
        suggestions.len().min(5).saturating_add(1)
    };
    let desired_context_height = u16::try_from(context_count).unwrap_or(6);
    let context_budget = root
        .height
        .saturating_sub(footer_height)
        .saturating_sub(composer_height)
        .saturating_sub(1);
    let context_height = desired_context_height.min(context_budget);
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(context_height),
            Constraint::Length(composer_height),
            Constraint::Length(footer_height),
        ])
        .split(root);

    UiAreas {
        transcript: areas[0],
        context: areas[1],
        composer: areas[2],
        footer: areas[3],
        gutter,
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App, palette: UiPalette) {
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
    if !areas.context.is_empty() {
        draw_context_panel(frame, areas.context, app, areas.gutter, &suggestions);
    }
    draw_composer(frame, areas.composer, app, palette);
    if !areas.footer.is_empty() {
        draw_footer(frame, areas.footer, app, areas.gutter);
    }
}

fn draw_project_trust_prompt(
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

fn horizontal_gutter(width: u16) -> u16 {
    match width {
        0..=39 => 0,
        40..=79 => 1,
        _ => 2,
    }
}

fn inset(area: Rect, horizontal: u16) -> Rect {
    area.inner(Margin::new(horizontal.min(area.width / 3), 0))
}

fn draw_transcript(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    gutter: u16,
    palette: UiPalette,
) {
    if area.is_empty() {
        return;
    }
    let show_working_placeholder =
        app.streaming_assistant.is_none() && (app.awaiting_assistant || app.status == "Working…");
    let working_placeholder = show_working_placeholder.then(|| TranscriptItem::Assistant {
        text: String::new(),
        streaming: true,
        error: None,
    });
    if app.transcript.is_empty() && working_placeholder.is_none() {
        draw_empty_state(frame, inset(area, gutter), app);
        return;
    }

    struct TranscriptBlock<'a> {
        item: &'a TranscriptItem,
        lines: Vec<Line<'static>>,
        start: usize,
        content_height: usize,
        height: usize,
        code_background_rows: Vec<(usize, usize)>,
    }

    let content_area = area;
    let user_text_width = area.width.saturating_sub(3).max(1);
    let mut blocks =
        Vec::with_capacity(app.transcript.len() + usize::from(show_working_placeholder));
    let mut next_start = 0usize;
    for (index, item) in app
        .transcript
        .iter()
        .chain(working_placeholder.iter())
        .enumerate()
    {
        let is_user = matches!(item, TranscriptItem::User(_));
        if index > 0 {
            next_start = next_start.saturating_add(1);
        }
        let lines = transcript_item_lines(
            item,
            palette.terminal_appearance,
            app.animation_frame,
            app.tools_expanded,
        );
        let code_background_rows = if is_user {
            Vec::new()
        } else {
            wrapped_line_background_ranges(
                &lines,
                content_area.width.max(1),
                code_block_background(palette.terminal_appearance),
            )
        };
        let content_height = if let TranscriptItem::User(text) = item {
            Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .line_count(user_text_width)
                .max(1)
        } else {
            Paragraph::new(lines.clone())
                .wrap(Wrap { trim: false })
                .line_count(content_area.width.max(1))
                .max(1)
        };
        let height = content_height.saturating_add(usize::from(is_user) * 2);
        blocks.push(TranscriptBlock {
            item,
            lines,
            start: next_start,
            content_height,
            height,
            code_background_rows,
        });
        next_start = next_start.saturating_add(height);
    }

    let trailing_spacing = usize::from(next_start > 0);
    let line_count = next_start.saturating_add(trailing_spacing);
    let viewport = usize::from(area.height);
    let max_scroll = line_count.saturating_sub(viewport);
    let from_bottom = app.scroll_from_bottom.min(max_scroll);
    let scroll = max_scroll.saturating_sub(from_bottom);
    let viewport_end = scroll.saturating_add(viewport);
    let bottom_padding = viewport.saturating_sub(line_count);

    for block in blocks {
        let block_end = block.start.saturating_add(block.height);
        let visible_start = block.start.max(scroll);
        let visible_end = block_end.min(viewport_end);
        if visible_start >= visible_end {
            continue;
        }

        let visible_y = bottom_padding.saturating_add(visible_start.saturating_sub(scroll));
        let screen_y = area
            .y
            .saturating_add(u16::try_from(visible_y).unwrap_or(u16::MAX));
        let visible_height = u16::try_from(visible_end.saturating_sub(visible_start))
            .unwrap_or(u16::MAX)
            .min(area.bottom().saturating_sub(screen_y));

        if let TranscriptItem::User(text) = block.item {
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
                let content_y =
                    bottom_padding.saturating_add(content_visible_start.saturating_sub(scroll));
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
            for (background_start, background_end) in block.code_background_rows {
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
            frame.render_widget(
                Paragraph::new(block.lines)
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

fn wrapped_line_background_ranges(
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

fn draw_submitted_prompt(frame: &mut ratatui::Frame<'_>, area: Rect, text: &str, scroll: usize) {
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

fn draw_empty_state(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let width = area.width;
    let top_margin = u16::from(area.height >= 8);
    let height = area.height.saturating_sub(top_margin).min(7);
    if width < 20 || height < 5 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("π  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled("Ask pi anything", Style::default().fg(Color::DarkGray)),
            ])),
            area,
        );
        return;
    }
    let card = Rect::new(area.x, area.y.saturating_add(top_margin), width, height);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner_width = usize::from(width.saturating_sub(6));
    let directory = truncate_start(&compact_path(&app.cwd), inner_width.saturating_sub(11));
    let mut model = app.model.clone();
    if app.thinking != "off" {
        model.push(' ');
        model.push_str(&app.thinking);
    }
    let model = truncate_end(&model, inner_width.saturating_sub(11));
    let text = vec![
        Line::from(vec![
            Span::styled("π_ ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("pi", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" (v{})", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::default(),
        Line::from(vec![
            Span::styled("model:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(model),
        ]),
        Line::from(vec![
            Span::styled("directory:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(directory),
        ]),
        Line::default(),
    ];
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        card,
    );
    let tip_y = card.bottom().saturating_add(1);
    if tip_y < area.bottom() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Tip: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("type "),
                Span::styled("/", Style::default().fg(Color::Cyan)),
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
        );
    }
}

fn draw_context_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    gutter: u16,
    suggestions: &[CommandSuggestion],
) {
    let area = inset(area, gutter.saturating_add(1));
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
        draw_command_palette(frame, area, app.command_selection, suggestions);
        return;
    };
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_command_palette(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    selection: usize,
    suggestions: &[CommandSuggestion],
) {
    let [list_area, help_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let selected_index = selection.min(suggestions.len() - 1);
    let model_selector = suggestions
        .first()
        .is_some_and(|suggestion| suggestion.invocation.starts_with("/model "));
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
    let builder = ListBuilder::new(|context| {
        let suggestion = &suggestions[context.index];
        let marker = if context.is_selected { "› " } else { "  " };
        let hint = suggestion
            .argument_hint
            .as_deref()
            .map_or_else(String::new, |hint| format!(" {hint}"));
        let style = if context.is_selected {
            Style::default().add_modifier(Modifier::BOLD)
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
        (
            Line::from(vec![
                Span::styled(marker, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("{label}{}", " ".repeat(label_padding)), style),
                Span::styled(
                    truncate_end(&suggestion.description, description_width),
                    style,
                ),
            ]),
            1,
        )
    });
    let list = ListView::new(builder, suggestions.len())
        .scroll_padding(2)
        .infinite_scrolling(true);
    let mut state = ListState::new_with_index(Some(selected_index));
    frame.render_stateful_widget(list, list_area, &mut state);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                if model_selector {
                    "↑↓ select · type to filter · enter apply"
                } else if resume_selector {
                    "↑↓ select · type to filter · enter resume"
                } else {
                    "↑↓ select · tab complete"
                },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(" · {}/{}", selected_index + 1, suggestions.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        help_area,
    );
}

fn draw_composer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App, palette: UiPalette) {
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
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let content_y = area.y.saturating_add(u16::from(area.height > 1));
    let marker_area = Rect::new(area.x, content_y, area.width.min(2), 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("› ", marker_style))),
        marker_area,
    );
    let inner = Rect::new(
        area.x.saturating_add(2),
        content_y,
        area.width.saturating_sub(3),
        area.height.saturating_sub(u16::from(area.height > 1)),
    );
    frame.render_widget(app.input.widget(), inner);
}

fn assistant_error(message: &pi_core::AssistantMessage) -> Option<String> {
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

fn assistant_token_usage(message: &pi_core::AssistantMessage) -> u64 {
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

fn assistant_context_usage(message: &pi_core::AssistantMessage) -> u64 {
    message
        .usage
        .input
        .saturating_add(message.usage.cache_read)
        .saturating_add(message.usage.cache_write)
}

fn message_token_usage(message: &Message) -> u64 {
    match message {
        Message::Assistant(message) => assistant_token_usage(message),
        Message::ToolResult(message) => {
            message.usage.as_ref().map_or(0, |usage| usage.total_tokens)
        }
        Message::User(_) => 0,
    }
}

fn latest_context_usage(messages: &[Message]) -> u64 {
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

fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn context_window(app: &App) -> Option<u64> {
    app.model_specs
        .iter()
        .find(|spec| spec.provider.as_str() == app.provider && spec.id.as_str() == app.model)
        .map(|spec| spec.context_window)
}

fn usage_footer(app: &App) -> String {
    let tokens = format!("tokens {}", format_token_count(app.session_tokens));
    match context_window(app) {
        Some(window) if window > 0 => {
            let percent = app.context_tokens.saturating_mul(100) / window;
            format!(
                "{tokens} · context {}/{} ({}%)",
                format_token_count(app.context_tokens),
                format_token_count(window),
                percent.min(999)
            )
        }
        _ => format!(
            "{tokens} · context {}",
            format_token_count(app.context_tokens)
        ),
    }
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App, gutter: u16) {
    let area = inset(area, gutter.saturating_add(1));
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
    if area.width >= 100 {
        spans.push(footer_separator());
        spans.push(Span::styled(
            app.session_name
                .as_ref()
                .map_or_else(|| app.provider.clone(), Clone::clone),
            Style::default().fg(Color::Magenta),
        ));
    }
    spans.push(footer_separator());
    spans.push(Span::styled(
        format!("{STATUS_DOT} "),
        Style::default().fg(tone),
    ));
    spans.push(Span::styled(
        app.status.clone(),
        Style::default().fg(if tone == Color::Red {
            Color::Red
        } else {
            Color::DarkGray
        }),
    ));
    if queued > 0 {
        spans.push(footer_separator());
        spans.push(Span::styled(
            format!("{queued} queued"),
            Style::default().fg(Color::Cyan),
        ));
    }
    if area.width >= 72 {
        spans.push(footer_separator());
        spans.push(Span::styled(
            usage_footer(app),
            Style::default().fg(Color::Cyan),
        ));
    }
    if area.width >= 120 {
        spans.push(footer_separator());
        if app.transcript_selection.is_some() {
            spans.push(Span::styled(
                "⌘C / Ctrl+Shift+C copy",
                Style::default().fg(Color::Cyan),
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

fn footer_separator() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(Color::DarkGray))
}

fn status_tone(app: &App) -> Color {
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

fn compact_path(path: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return path.to_string();
    };
    path.strip_prefix(&home)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        .map_or_else(|| path.to_string(), |suffix| format!("~{suffix}"))
}

fn discover_session_choices(current_path: &Path, current_cwd: &Path) -> Vec<SessionChoice> {
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

fn session_catalog_root(current_path: &Path) -> PathBuf {
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

fn session_catalog_paths(root: &Path) -> Vec<PathBuf> {
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

fn is_jsonl_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
}

fn read_session_header(path: &Path) -> Option<pi_session::SessionHeader> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim_end_matches(['\r', '\n'])).ok()
}

fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn format_session_age(modified_at_ms: u64, now_ms: u64) -> String {
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

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn input_cursor_position(input: &str, width: u16) -> (usize, usize) {
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

fn setup_terminal(fullscreen: bool) -> io::Result<TuiTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if fullscreen {
        enter_fullscreen(&mut stdout)?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut TuiTerminal, fullscreen: bool) -> io::Result<()> {
    disable_raw_mode()?;
    terminal.show_cursor()?;
    if fullscreen {
        leave_fullscreen(terminal.backend_mut())?;
    } else {
        println!();
    }
    Ok(())
}

fn enter_fullscreen(writer: &mut impl io::Write) -> io::Result<()> {
    execute!(
        writer,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

fn leave_fullscreen(writer: &mut impl io::Write) -> io::Result<()> {
    execute!(
        writer,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )
}

#[cfg(test)]
fn transcript_lines(items: &[TranscriptItem]) -> Vec<Line<'static>> {
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
        ));
    }
    lines
}

fn transcript_item_lines(
    item: &TranscriptItem,
    terminal_appearance: TerminalAppearance,
    animation_frame: usize,
    tools_expanded: bool,
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
        TranscriptItem::Assistant {
            text,
            streaming,
            error,
        } => {
            lines.extend(render_assistant_markdown(
                text,
                *streaming,
                terminal_appearance,
                animation_frame,
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

fn push_plain_text(lines: &mut Vec<Line<'static>>, text: &str, prefix: &'static str) {
    lines.extend(
        text.lines()
            .map(|line| Line::from(vec![Span::raw(prefix), Span::raw(line.to_string())])),
    );
}

fn render_assistant_markdown(
    text: &str,
    streaming: bool,
    terminal_appearance: TerminalAppearance,
    animation_frame: usize,
) -> Vec<Line<'static>> {
    let mut lines = pi_agent_md::render(text, streaming, markdown_theme(terminal_appearance)).lines;
    while lines.first().is_some_and(markdown_line_is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(markdown_line_is_blank) {
        lines.pop();
    }
    if lines.is_empty() && streaming {
        lines.push(Line::from(Span::styled(
            "Working",
            Style::default().fg(Color::DarkGray),
        )));
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

fn activity_indicator_color(appearance: TerminalAppearance, animation_frame: usize) -> Color {
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
    colors[animation_frame % ACTIVITY_FRAME_COUNT]
}

fn markdown_line_is_blank(line: &Line<'_>) -> bool {
    line.style.bg.is_none()
        && line
            .spans
            .iter()
            .all(|span| span.content.trim().is_empty() && span.style.bg.is_none())
}

fn push_shell_lines(
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

fn push_tool_payload_lines(lines: &mut Vec<Line<'static>>, label: &str, payload: Option<&str>) {
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

fn format_tool_input(value: &serde_json::Value) -> Option<String> {
    serde_json::to_string_pretty(value).ok()
}

fn format_tool_result(result: &pi_core::ToolResult) -> String {
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

fn summarize_tool_args(value: &serde_json::Value) -> Option<String> {
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

fn tool_result_text(result: &pi_core::ToolResult) -> String {
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

fn truncate_end(text: &str, max_width: usize) -> String {
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

fn truncate_start(text: &str, max_width: usize) -> String {
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

fn command_suggestions(input: &str, command_specs: &[CommandSpec]) -> Vec<CommandSuggestion> {
    let Some(prefix) = command_query(input) else {
        return Vec::new();
    };
    let builtins = [
        ("/new", "new session", Some("[path]")),
        ("/resume", "list or open sessions", Some("[query|path]")),
        ("/reload", "reload all plugins/resources", None),
        ("/trust", "change trust for this project", None),
        (
            "/model",
            "list or change model",
            Some("[provider/model|id]"),
        ),
        (
            "/thinking",
            "off|minimal|low|medium|high|xhigh",
            Some("<level>"),
        ),
        ("/compact", "compact context", None),
        ("/clear", "clear display", None),
        ("/help", "show commands", None),
        ("/quit", "exit", None),
    ];
    let mut suggestions = builtins
        .into_iter()
        .filter(|(invocation, _, _)| invocation.starts_with(prefix))
        .map(
            |(invocation, description, argument_hint)| CommandSuggestion {
                invocation: invocation.to_string(),
                label: None,
                description: description.to_string(),
                argument_hint: argument_hint.map(str::to_string),
                apply_on_enter: false,
            },
        )
        .collect::<Vec<_>>();
    for spec in command_specs {
        let invocation = format!("/{}", spec.name);
        if !invocation.starts_with(prefix)
            || suggestions
                .iter()
                .any(|suggestion| suggestion.invocation == invocation)
        {
            continue;
        }
        suggestions.push(CommandSuggestion {
            invocation,
            label: None,
            description: spec.description.clone(),
            argument_hint: spec.argument_hint.clone(),
            apply_on_enter: false,
        });
    }
    suggestions
}

fn user_message_display_text(content: &[ContentBlock]) -> String {
    let text = user_text(content);
    collapse_skill_invocation(&text).unwrap_or(text)
}

fn collapse_skill_invocation(text: &str) -> Option<String> {
    let rest = text.strip_prefix("<skill name=\"")?;
    let (name, rest) = rest.split_once("\" location=\"")?;
    let (_, body) = rest.split_once("\">\n")?;
    let (_, trailing) = body.split_once("\n</skill>")?;
    let mut display = format!("/skill:{name}");
    if !trailing.is_empty() {
        let arguments = trailing.strip_prefix("\n\n")?.trim();
        if !arguments.is_empty() {
            display.push(' ');
            display.push_str(arguments);
        }
    }
    Some(display)
}

fn user_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_history_entry(transcript: &mut Vec<TranscriptItem>, entry: &SessionEntry) {
    if let SessionEntry::Message(message) = entry {
        push_history_message(transcript, &message.message);
    }
}

fn push_history_message(transcript: &mut Vec<TranscriptItem>, message: &pi_session::AgentMessage) {
    if let Some(message) = message.as_standard() {
        match message {
            Message::User(user) => {
                transcript.push(TranscriptItem::User(user_message_display_text(
                    &user.content,
                )));
            }
            Message::Assistant(assistant) => {
                let text = assistant_text(message).unwrap_or_default();
                let error = assistant_error(assistant);
                if !text.trim().is_empty() || error.is_some() {
                    transcript.push(TranscriptItem::Assistant {
                        text,
                        streaming: false,
                        error,
                    });
                }
                for call in assistant.tool_calls() {
                    transcript.push(TranscriptItem::Tool {
                        id: call.id,
                        name: call.name,
                        detail: summarize_tool_args(&call.arguments),
                        input: format_tool_input(&call.arguments),
                        output: None,
                        state: ToolState::Pending,
                    });
                }
            }
            Message::ToolResult(result) => {
                let state = if result.is_error {
                    let error = user_text(&result.content);
                    ToolState::Failed(if error.trim().is_empty() {
                        "Tool failed".to_string()
                    } else {
                        truncate_end(error.trim(), 120)
                    })
                } else {
                    ToolState::Succeeded
                };
                if let Some(TranscriptItem::Tool {
                    name,
                    output,
                    state: current_state,
                    ..
                }) = transcript.iter_mut().rev().find(|item| {
                    matches!(item, TranscriptItem::Tool { id, .. } if id == &result.tool_call_id)
                }) {
                    *name = result.tool_name.clone();
                    *output = Some(user_text(&result.content));
                    *current_state = state;
                } else {
                    transcript.push(TranscriptItem::Tool {
                        id: result.tool_call_id.clone(),
                        name: result.tool_name.clone(),
                        detail: None,
                        input: None,
                        output: Some(user_text(&result.content)),
                        state,
                    });
                }
            }
        }
        return;
    }
    if message.role() == "bashExecution"
        && let Some(value) = message.as_custom()
    {
        let command = value
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let output = value
            .get("output")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let exit_code = value
            .get("exitCode")
            .and_then(serde_json::Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        transcript.push(TranscriptItem::Shell {
            command: command.to_string(),
            output: output.to_string(),
            excluded_from_context: value
                .get("excludeFromContext")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            state: ShellState::Finished {
                exit_code,
                cancelled: value
                    .get("cancelled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                timed_out: value
                    .get("timedOut")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                truncated: value
                    .get("truncated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[derive(Default)]
    struct RecordingClipboard {
        text: Option<String>,
    }

    impl ClipboardWriter for RecordingClipboard {
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            self.text = Some(text.to_string());
            Ok(())
        }
    }

    fn demo_app() -> App {
        App {
            transcript: vec![
                TranscriptItem::User("把插件 reload 的边界再收紧一点。".to_string()),
                TranscriptItem::Assistant {
                    text: "我会保留正在运行的 session，并原子替换完整插件 generation。\n\n- 新请求看到新 generation\n- 当前请求继续使用旧 generation".to_string(),
                    streaming: false,
                    error: None,
                },
                TranscriptItem::Tool {
                    id: ToolCallId::new("call-read"),
                    name: "read".to_string(),
                    detail: Some("crates/pi-runtime/src/lib.rs".to_string()),
                    input: Some("{\n  \"path\": \"crates/pi-runtime/src/lib.rs\"\n}".to_string()),
                    output: Some("file contents".to_string()),
                    state: ToolState::Succeeded,
                },
                TranscriptItem::Shell {
                    command: "cargo test -p pi-runtime".to_string(),
                    output: "running 18 tests\ntest result: ok. 18 passed".to_string(),
                    excluded_from_context: false,
                    state: ShellState::Finished {
                        exit_code: Some(0),
                        cancelled: false,
                        timed_out: false,
                        truncated: false,
                    },
                },
            ],
            input: ComposerInput::from_text("继续完善 reload 测试"),
            input_history: InputHistory::default(),
            status: "Ready".to_string(),
            queue: QueueSnapshot {
                steering: Vec::new(),
                follow_up: vec!["完成后跑完整测试".to_string()],
            },
            command_specs: vec![CommandSpec {
                name: "skill:code-review".to_string(),
                description: "Review changes against repository standards".to_string(),
                argument_hint: Some("[task]".to_string()),
            }],
            model_specs: Vec::new(),
            session_choices: Vec::new(),
            command_selection: 0,
            streaming_assistant: None,
            awaiting_assistant: false,
            bash_line: None,
            provider: "openai-compatible".to_string(),
            model: "gpt-5.6-sol".to_string(),
            thinking: "high".to_string(),
            cwd: "/Users/cherry/Documents/pi_rs".to_string(),
            session_name: None,
            session_tokens: 12_450,
            context_tokens: 8_192,
            is_running: false,
            compacting: false,
            tools_expanded: false,
            animation_frame: 0,
            scroll_from_bottom: 0,
            transcript_selection: None,
            trust_prompt: None,
            epoch: 1,
            quit: false,
        }
    }

    fn render_app(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));
        terminal.draw(|frame| draw(frame, app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer[(x, y)].symbol());
            }
            screen.push_str(line.trim_end());
            screen.push('\n');
        }
        screen
    }

    #[test]
    fn trust_prompt_replaces_chat_and_applies_the_selected_option() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join(".pi/skills")).unwrap();
        let (service, _) =
            ProjectTrustService::new(&directory.path().join("agent"), None, true).unwrap();
        let mut app = demo_app();
        app.trust_prompt = Some(TrustPromptState {
            cwd: project.clone(),
            options: service.manual_options(&project).unwrap(),
            selected: 0,
            response: None,
        });

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("Trust project folder?"));
        assert!(screen.contains("› Trust"));
        assert!(!screen.contains("把插件 reload"));
        assert!(handle_trust_prompt_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut app,
            &service,
        ));
        assert!(app.trust_prompt.is_none());
        assert_eq!(
            service.evaluate(&project).unwrap(),
            crate::project_trust::ProjectTrustEvaluation::Known(true)
        );
    }

    fn mouse_event(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn blank_surface(width: u16, height: u16) -> TranscriptSurface {
        let area = Rect::new(0, 0, width, height);
        TranscriptSurface::capture(&ratatui::buffer::Buffer::empty(area), area)
    }

    #[test]
    fn input_history_navigates_older_and_newer_then_restores_draft() {
        let mut history = InputHistory::default();
        history.record("first prompt");
        history.record("second prompt");

        assert_eq!(
            history.older("unfinished draft"),
            Some("second prompt".into())
        );
        assert_eq!(history.older("second prompt"), Some("first prompt".into()));
        assert_eq!(history.older("first prompt"), Some("first prompt".into()));
        assert_eq!(history.newer(), Some("second prompt".into()));
        assert_eq!(history.newer(), Some("unfinished draft".into()));
        assert_eq!(history.newer(), None);
    }

    #[test]
    fn input_history_ignores_empty_and_consecutive_duplicate_entries() {
        let mut history = InputHistory::default();
        history.record("  ");
        history.record("same prompt");
        history.record(" same prompt ");

        assert_eq!(history.entries, vec!["same prompt"]);
    }

    #[test]
    fn vertical_navigation_prefers_command_suggestions_over_input_history() {
        let mut app = demo_app();
        app.input.set_text("/h");
        app.input_history.record("historic prompt");

        assert!(handle_vertical_navigation(KeyCode::Down, &mut app));
        assert_eq!(app.input.text(), "/h");
        assert!(!app.input_history.is_browsing());
    }

    #[test]
    fn polished_layout_renders_semantic_regions() {
        let screen = render_app(&demo_app(), 100, 32);
        if std::env::var_os("PI_PRINT_TUI_TEST").is_some() {
            println!("{screen}");
        }
        assert!(screen.contains('›'));
        assert!(screen.contains('•'));
        assert!(screen.contains("gpt-5.6-sol high"));
        assert!(screen.contains("~/Documents/pi_rs"));
        assert!(screen.contains("Ran cargo test -p pi-runtime"));
        assert!(screen.contains("1 queued"));
        assert!(screen.contains("tokens 12.4k"));
        assert!(screen.contains("context 8.2k"));
        assert!(!screen.contains("You"));
        assert!(!screen.contains("Message"));
        assert!(!screen.contains('✓'));
        assert!(!screen.contains('×'));
    }

    #[test]
    fn completed_tool_output_is_collapsed_by_default() {
        let items = vec![TranscriptItem::Tool {
            id: ToolCallId::new("call-collapsed"),
            name: "read".to_string(),
            detail: Some("notes.txt".to_string()),
            input: Some("hidden input".to_string()),
            output: Some("hidden output".to_string()),
            state: ToolState::Succeeded,
        }];

        let rendered = transcript_lines(&items)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("read"));
        assert!(rendered.contains("notes.txt"));
        assert!(!rendered.contains("hidden input"));
        assert!(!rendered.contains("hidden output"));
    }

    #[test]
    fn expanded_tool_output_shows_complete_input_and_output() {
        let item = TranscriptItem::Tool {
            id: ToolCallId::new("call-expanded"),
            name: "read".to_string(),
            detail: Some("notes.txt".to_string()),
            input: Some("complete input".to_string()),
            output: Some(
                (1..=14)
                    .map(|line| format!("output line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            state: ToolState::Succeeded,
        };

        let rendered = transcript_item_lines(&item, TerminalAppearance::Dark, 0, true)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("complete input"));
        assert!(rendered.contains("output line 14"));
    }

    #[test]
    fn ctrl_o_toggles_all_tool_output_and_reports_the_state() {
        let mut app = demo_app();
        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);

        assert!(handle_tool_output_key(key, &mut app));
        assert!(app.tools_expanded);
        assert_eq!(app.status, "Tool output: expanded");

        assert!(handle_tool_output_key(key, &mut app));
        assert!(!app.tools_expanded);
        assert_eq!(app.status, "Tool output: collapsed");
    }

    #[test]
    fn transcript_status_uses_one_dot_with_state_colors() {
        let states = [
            (ToolState::Pending, Color::DarkGray),
            (ToolState::Running, Color::Yellow),
            (ToolState::Succeeded, Color::Green),
            (ToolState::Failed("boom".to_string()), Color::Red),
        ];
        let items = states
            .iter()
            .enumerate()
            .map(|(index, (state, _))| TranscriptItem::Tool {
                id: ToolCallId::new(format!("call-{index}")),
                name: format!("tool-{index}"),
                detail: None,
                input: None,
                output: None,
                state: state.clone(),
            })
            .collect::<Vec<_>>();
        let lines = transcript_lines(&items);
        let marker_lines = lines
            .iter()
            .filter(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.content == format!("{STATUS_DOT} "))
            })
            .collect::<Vec<_>>();

        assert_eq!(marker_lines.len(), states.len());
        for (line, (_, expected_color)) in marker_lines.into_iter().zip(states) {
            assert_eq!(line.spans[0].style.fg, Some(expected_color));
        }
    }

    #[test]
    fn transcript_items_share_one_line_of_vertical_spacing() {
        let items = vec![
            TranscriptItem::User("user".to_string()),
            TranscriptItem::Assistant {
                text: "assistant".to_string(),
                streaming: false,
                error: None,
            },
            TranscriptItem::Tool {
                id: ToolCallId::new("call-spacing"),
                name: "read".to_string(),
                detail: None,
                input: None,
                output: None,
                state: ToolState::Running,
            },
            TranscriptItem::Shell {
                command: "pwd".to_string(),
                output: String::new(),
                excluded_from_context: false,
                state: ShellState::Running,
            },
        ];

        let lines = transcript_lines(&items);

        assert_eq!(lines.len(), 7);
        for spacer in [1, 3, 5] {
            assert!(lines[spacer].spans.is_empty());
        }
        assert!(lines[0].spans[0].content.contains('›'));
        for item_line in [2, 4, 6] {
            assert_eq!(lines[item_line].spans[0].content, "• ");
        }
    }

    #[test]
    fn assistant_markdown_theme_omits_hashes_and_emoji_alerts() {
        let lines = render_assistant_markdown(
            "# Release notes\n\n> [!WARNING]\n> Check the migration.",
            false,
            TerminalAppearance::Dark,
            0,
        );
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Release notes"));
        assert!(!rendered.contains("# Release notes"));
        assert!(rendered.contains("• Warning"));
        assert!(!rendered.contains('⚠'));
        assert!(!rendered.contains('❗'));
        assert!(!rendered.contains('🔴'));
    }

    #[test]
    fn assistant_markdown_renders_inline_styles_links_and_tables() {
        let markdown = concat!(
            "Use **bold**, *italic*, and `code`.\n\n",
            "Read [the docs](https://example.com/docs).\n\n",
            "| Feature | Status |\n",
            "| --- | --- |\n",
            "| Markdown | ready |",
        );

        let lines = render_assistant_markdown(markdown, false, TerminalAppearance::Dark, 0);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(lines[0].spans[0].content, format!("{STATUS_DOT} "));
        assert!(lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
            })
        }));
        assert!(lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
            })
        }));
        assert!(rendered.contains("the docs"));
        assert!(rendered.contains("Feature"));
        assert!(rendered.contains("Markdown"));
        assert!(!rendered.contains("**"));
        assert!(!rendered.contains("](https://"));
        assert!(!rendered.contains("| ---"));
    }

    #[test]
    fn assistant_markdown_does_not_add_a_global_body_indent() {
        let lines = render_assistant_markdown(
            "first line\ncontinued\n\nnext paragraph",
            false,
            TerminalAppearance::Dark,
            0,
        );

        assert_eq!(lines[0].spans[0].content, format!("{STATUS_DOT} "));
        for expected in ["continued", "next paragraph"] {
            let line = lines
                .iter()
                .find(|line| line.to_string().contains(expected))
                .expect("rendered body line");
            assert_eq!(
                line.spans.first().map(|span| span.content.as_ref()),
                Some(expected),
                "body text must begin at the transcript edge"
            );
        }
    }

    #[test]
    fn assistant_markdown_hides_fences_and_preserves_code_backgrounds() {
        let markdown = concat!(
            "Use the `.vue` extension.\n\n",
            "```rust\n",
            "fn main() { println!(\"hello\"); }\n",
            "```",
        );

        let lines = render_assistant_markdown(markdown, false, TerminalAppearance::Dark, 0);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!rendered.contains("```"));
        let background = code_block_background(TerminalAppearance::Dark);
        let inline_code = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == ".vue")
            .expect("inline code span");
        assert!(inline_code.style.fg.is_some());
        assert!(inline_code.style.bg.is_some());
        assert_ne!(inline_code.style.bg, Some(background));
        let code_line = lines
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .expect("fenced code line");
        assert_eq!(code_line.style.bg, Some(background));
        let prose_line = lines
            .iter()
            .find(|line| line.to_string().contains("Use the"))
            .expect("prose line");
        assert_eq!(prose_line.style.bg, None);
    }

    #[test]
    fn assistant_markdown_code_is_readable_on_a_light_background() {
        let lines = render_assistant_markdown(
            "```rust\nfn main() {}\n```",
            false,
            TerminalAppearance::Light,
            0,
        );
        let keyword = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "fn")
            .expect("highlighted Rust keyword");
        let contrast = contrast_ratio(
            keyword.style.fg.expect("keyword foreground"),
            (232, 232, 236),
        );

        assert!(
            contrast >= 4.5,
            "Rust keyword contrast against the light code background was only {contrast:.2}:1"
        );
    }

    #[test]
    fn assistant_markdown_code_remains_readable_on_a_dark_background() {
        let lines = render_assistant_markdown(
            "```rust\nfn main() {}\n```",
            false,
            TerminalAppearance::Dark,
            0,
        );
        let keyword = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "fn")
            .expect("highlighted Rust keyword");
        let contrast = contrast_ratio(keyword.style.fg.expect("keyword foreground"), (16, 16, 17));

        assert!(
            contrast >= 4.5,
            "Rust keyword contrast against the dark code background was only {contrast:.2}:1"
        );
    }

    fn contrast_ratio(foreground: Color, background: (u8, u8, u8)) -> f64 {
        let Color::Rgb(red, green, blue) = foreground else {
            panic!("expected a true-color syntax highlight, got {foreground:?}");
        };
        let foreground = relative_luminance((red, green, blue));
        let background = relative_luminance(background);
        let (lighter, darker) = if foreground > background {
            (foreground, background)
        } else {
            (background, foreground)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
        fn channel(value: u8) -> f64 {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }

        0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }

    #[test]
    fn streaming_markdown_tolerates_an_unclosed_inline_marker() {
        let lines =
            render_assistant_markdown("Writing **partial", true, TerminalAppearance::Dark, 0);
        let rendered = lines.iter().map(Line::to_string).collect::<String>();

        assert!(rendered.contains("Writing"));
        assert!(rendered.contains("partial"));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(110, 93, 22)));
        assert!(!rendered.contains('▌'));
    }

    #[test]
    fn working_indicator_breathes_color_without_changing_shape() {
        let dark_frames = (0..4)
            .map(|frame| {
                let span = render_assistant_markdown("", true, TerminalAppearance::Dark, frame)[0]
                    .spans[0]
                    .clone();
                (span.content.to_string(), span.style.fg)
            })
            .collect::<Vec<_>>();
        let light_colors = (0..4)
            .map(|frame| {
                render_assistant_markdown("", true, TerminalAppearance::Light, frame)[0].spans[0]
                    .style
                    .fg
            })
            .collect::<Vec<_>>();

        assert!(dark_frames.iter().all(|(indicator, _)| indicator == "• "));
        assert_eq!(
            dark_frames
                .iter()
                .map(|(_, color)| *color)
                .collect::<Vec<_>>(),
            [
                Some(Color::Rgb(110, 93, 22)),
                Some(Color::Rgb(198, 144, 38)),
                Some(Color::Rgb(242, 204, 96)),
                Some(Color::Rgb(198, 144, 38)),
            ]
        );
        assert_eq!(
            light_colors,
            [
                Some(Color::Rgb(138, 109, 0)),
                Some(Color::Rgb(181, 137, 0)),
                Some(Color::Rgb(210, 153, 34)),
                Some(Color::Rgb(181, 137, 0)),
            ]
        );
        assert!(
            !dark_frames
                .iter()
                .any(|(indicator, _)| indicator.contains('▌'))
        );
    }

    #[test]
    fn local_working_status_renders_a_transcript_placeholder_before_session_events() {
        let mut app = demo_app();
        app.transcript.clear();
        app.status = "Working…".to_string();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        let transcript_bottom = ui_areas(Rect::new(0, 0, 80, 12), &app).transcript.bottom();

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        assert!(
            find_buffer_row(
                terminal.backend().buffer(),
                80,
                transcript_bottom,
                "Working"
            )
            .is_some(),
            "the transcript should acknowledge submission before any provider event arrives"
        );
    }

    #[test]
    fn provider_stream_replaces_the_local_working_placeholder_without_duplication() {
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::Assistant {
            text: String::new(),
            streaming: true,
            error: None,
        }];
        app.streaming_assistant = Some(0);
        app.awaiting_assistant = true;
        app.status = "Working…".to_string();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        let transcript_bottom = ui_areas(Rect::new(0, 0, 80, 12), &app).transcript.bottom();

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        let working_rows = (0..transcript_bottom)
            .filter(|&y| {
                (0..80)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Working")
            })
            .count();
        assert_eq!(working_rows, 1);
    }

    #[test]
    fn skill_user_message_displays_as_the_original_command() {
        let expanded = concat!(
            "<skill name=\"ask-matt\" location=\"/skills/ask-matt/SKILL.md\">\n",
            "References are relative to /skills/ask-matt.\n\n",
            "# Ask Matt\n\nChoose a workflow.\n",
            "</skill>\n\nhi"
        );
        let message = Message::User(pi_core::UserMessage::text(expanded, 0));
        let mut live = demo_app();
        live.transcript.clear();

        live.apply_session_event(AgentSessionEvent::Agent(Box::new(
            AgentEvent::MessageStart {
                message: message.clone(),
            },
        )));
        assert_eq!(
            live.transcript,
            vec![TranscriptItem::User("/skill:ask-matt hi".to_string())]
        );

        let mut restored = Vec::new();
        push_history_message(&mut restored, &pi_session::AgentMessage::from(message));
        assert_eq!(
            restored,
            vec![TranscriptItem::User("/skill:ask-matt hi".to_string())]
        );
    }

    #[test]
    fn provider_error_message_is_rendered_in_the_transcript() {
        let mut app = demo_app();
        app.transcript.clear();
        let failure = pi_core::AssistantMessage {
            content: Vec::new(),
            api: "openai-completions".to_string(),
            provider: ProviderId::new("openai-compatible"),
            model: ModelId::new("gpt-5.6-sol"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: pi_core::Usage::default(),
            stop_reason: pi_core::StopReason::Error,
            error_message: Some("provider rejected request: invalid API key".to_string()),
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 0,
        };
        let persisted_failure = failure.clone();

        app.apply_session_event(AgentSessionEvent::Agent(Box::new(
            AgentEvent::MessageStart {
                message: Message::assistant(failure.clone()),
            },
        )));
        app.apply_session_event(AgentSessionEvent::Agent(Box::new(AgentEvent::MessageEnd {
            message: Message::assistant(failure),
        })));

        let screen = render_app(&app, 100, 12);
        assert!(
            screen.contains("Error: provider rejected request: invalid API key"),
            "provider error details should remain visible in the transcript:\n{screen}"
        );

        let mut restored = Vec::new();
        push_history_message(
            &mut restored,
            &pi_session::AgentMessage::from(Message::assistant(persisted_failure)),
        );
        let restored = transcript_lines(&restored)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            restored.contains("Error: provider rejected request: invalid API key"),
            "provider errors should remain visible after restoring the session: {restored}"
        );
    }

    #[test]
    fn composer_surface_tracks_light_and_dark_terminal_backgrounds() {
        let light = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));
        assert_eq!(light.composer_background, Some(Color::Rgb(245, 245, 245)));
        assert_eq!(light.terminal_appearance, TerminalAppearance::Light);

        let dark = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        assert_eq!(dark.composer_background, Some(Color::Rgb(17, 17, 17)));
        assert_eq!(dark.terminal_appearance, TerminalAppearance::Dark);

        let unknown = UiPalette::from_background(None);
        assert_eq!(unknown.composer_background, None);
        assert_eq!(unknown.terminal_appearance, TerminalAppearance::Dark);
    }

    #[test]
    fn resumed_transcript_cannot_emit_terminal_control_sequences() {
        let mut app = demo_app();
        for item in [
            TranscriptItem::Assistant {
                text: "before\x1b[?1049lafter".to_string(),
                streaming: false,
                error: None,
            },
            TranscriptItem::Shell {
                command: "printf escape".to_string(),
                output: "before\x1b[?1049lafter".to_string(),
                excluded_from_context: false,
                state: ShellState::Finished {
                    exit_code: Some(0),
                    cancelled: false,
                    timed_out: false,
                    truncated: false,
                },
            },
        ] {
            app.transcript = vec![item];
            let mut output = Vec::new();
            {
                let backend = CrosstermBackend::new(&mut output);
                let mut terminal = Terminal::new(backend).unwrap();
                let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
                terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
            }

            assert!(!contains_bytes(&output, b"\x1b[?1049l"));
        }
    }

    #[test]
    fn fullscreen_modes_capture_mouse_without_translating_scroll_to_arrow_keys() {
        let mut enter = Vec::new();
        enter_fullscreen(&mut enter).unwrap();
        assert!(contains_bytes(&enter, b"\x1b[?1049h"));
        assert!(contains_bytes(&enter, b"\x1b[?1000h"));
        assert!(
            !contains_bytes(&enter, b"\x1b[?1007h"),
            "alternate-scroll turns wheel gestures into arrow keys and conflicts with input history"
        );
        assert!(contains_bytes(&enter, b"\x1b[?2004h"));
        assert!(contains_bytes(&enter, b"\x1b[>1u"));
        assert!(!contains_bytes(&enter, b"\x1b[>9u"));

        let mut leave = Vec::new();
        leave_fullscreen(&mut leave).unwrap();
        assert!(contains_bytes(&leave, b"\x1b[?1000l"));
        assert!(!contains_bytes(&leave, b"\x1b[?1007l"));
        assert!(contains_bytes(&leave, b"\x1b[?2004l"));
        assert!(contains_bytes(&leave, b"\x1b[<1u"));
        assert!(contains_bytes(&leave, b"\x1b[?1049l"));
    }

    #[test]
    fn mouse_wheel_scrolls_only_the_tui_transcript() {
        let mut app = demo_app();
        let surface = blank_surface(80, 24);
        assert_eq!(app.scroll_from_bottom, 0);

        handle_mouse(mouse_event(MouseEventKind::ScrollUp), &mut app, &surface);
        assert_eq!(app.scroll_from_bottom, MOUSE_SCROLL_LINES);

        handle_mouse(mouse_event(MouseEventKind::Moved), &mut app, &surface);
        assert_eq!(app.scroll_from_bottom, MOUSE_SCROLL_LINES);

        handle_mouse(mouse_event(MouseEventKind::ScrollDown), &mut app, &surface);
        assert_eq!(app.scroll_from_bottom, 0);
    }

    #[test]
    fn mouse_wheel_changes_the_visible_transcript_viewport() {
        let mut app = demo_app();
        app.transcript = (0..20)
            .map(|index| TranscriptItem::Assistant {
                text: format!("message-{index:02}"),
                streaming: false,
                error: None,
            })
            .collect();
        app.input.clear();
        app.queue = QueueSnapshot::default();

        let bottom = render_app(&app, 80, 12);
        assert!(bottom.contains("message-19"));

        handle_mouse(
            mouse_event(MouseEventKind::ScrollUp),
            &mut app,
            &blank_surface(80, 12),
        );
        let scrolled = render_app(&app, 80, 12);
        assert_ne!(scrolled, bottom);
        assert!(!scrolled.contains("message-19"));
        assert!(scrolled.contains("message-17"));
    }

    #[test]
    fn mouse_drag_selects_the_visible_assistant_text() {
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::Assistant {
            text: "copy me".to_string(),
            streaming: false,
            error: None,
        }];
        app.input.clear();
        app.queue = QueueSnapshot::default();
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_buffer_position(buffer, 40, 8, "copy me").expect("assistant text");
        let surface = TranscriptSurface::capture(buffer, Rect::new(0, 0, 40, 8));

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            &surface,
        );
        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: x + 3,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            &surface,
        );
        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: x + 3,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            &surface,
        );

        let selection = app.transcript_selection.expect("finished selection");
        assert_eq!(surface.selected_text(selection).as_deref(), Some("copy"));
        assert_eq!(app.status, "Ready");
        assert!(render_app(&app, 140, 12).contains("Ctrl+Shift+C copy"));
    }

    #[test]
    fn dragged_transcript_selection_is_visibly_highlighted() {
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::Assistant {
            text: "highlight me".to_string(),
            streaming: false,
            error: None,
        }];
        app.input.clear();
        app.queue = QueueSnapshot::default();
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_buffer_position(buffer, 40, 8, "highlight me").expect("assistant text");
        let surface = TranscriptSurface::capture(buffer, Rect::new(0, 0, 40, 8));
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            handle_mouse(
                MouseEvent {
                    kind,
                    column: if matches!(kind, MouseEventKind::Down(_)) {
                        x
                    } else {
                        x + 8
                    },
                    row: y,
                    modifiers: KeyModifiers::NONE,
                },
                &mut app,
                &surface,
            );
        }

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        for selected_x in x..=x + 8 {
            assert_eq!(buffer[(selected_x, y)].bg, Color::Rgb(10, 78, 152));
        }
        assert_ne!(buffer[(x + 9, y)].bg, Color::Rgb(10, 78, 152));
    }

    #[test]
    fn copy_shortcut_writes_the_visible_selection_to_the_clipboard() {
        let mut app = demo_app();
        let area = Rect::new(0, 0, 12, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        buffer.set_string(0, 0, "copy me", Style::default());
        let surface = TranscriptSurface::capture(&buffer, area);
        app.transcript_selection = Some(TranscriptSelection::new(
            Position::new(0, 0),
            Position::new(3, 0),
        ));
        let mut clipboard = RecordingClipboard::default();

        let consumed = handle_copy_shortcut(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER),
            &mut app,
            &surface,
            &mut clipboard,
        );

        assert!(consumed);
        assert_eq!(clipboard.text.as_deref(), Some("copy"));
        assert_eq!(app.status, "Copied 4 characters");
    }

    #[test]
    fn copy_shortcuts_support_command_and_ctrl_shift_without_stealing_ctrl_c() {
        assert!(is_copy_shortcut(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::SUPER
        )));
        assert!(is_copy_shortcut(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!is_copy_shortcut(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn transcript_scrollbar_thumb_reaches_bottom_with_the_viewport() {
        let mut app = demo_app();
        app.transcript = (0..20)
            .map(|index| TranscriptItem::Assistant {
                text: format!("message-{index:02}"),
                streaming: false,
                error: None,
            })
            .collect();
        app.scroll_from_bottom = 0;

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal
            .draw(|frame| draw_transcript(frame, frame.area(), &app, 0, palette))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let thumb_rows = (0..9)
            .filter(|&y| buffer[(39, y)].symbol() == "▐")
            .collect::<Vec<_>>();

        assert!(!thumb_rows.is_empty(), "expected a visible scrollbar thumb");
        assert_eq!(thumb_rows.last(), Some(&8));
    }

    #[test]
    fn arrows_browse_input_history_but_preserve_command_navigation() {
        let mut app = demo_app();
        app.input_history.record("/resume previous.jsonl");
        app.input.clear();

        assert!(handle_vertical_navigation(KeyCode::Up, &mut app));
        assert_eq!(app.input.text(), "/resume previous.jsonl");
        assert!(handle_vertical_navigation(KeyCode::Down, &mut app));
        assert!(app.input.is_empty());

        app.input.set_text("/");
        assert!(handle_vertical_navigation(KeyCode::Down, &mut app));
        assert_eq!(app.command_selection, 1);
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn composer_surface_is_applied_to_the_whole_input_area() {
        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = demo_app();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal
            .draw(|frame| draw_composer(frame, frame.area(), &app, palette))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(245, 245, 245));
        assert_eq!(buffer[(39, 2)].bg, Color::Rgb(245, 245, 245));
        assert_eq!(buffer[(20, 0)].symbol(), " ");
    }

    #[test]
    fn composer_component_edits_at_the_cursor() {
        let mut input = ComposerInput::from_text("ac");

        input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));

        assert_eq!(input.text(), "abc");
        assert_eq!(input.editor.cursor(), (0, 2));
    }

    #[test]
    fn composer_newline_shortcuts_match_pi() {
        assert!(is_newline_key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL
        )));
        assert!(is_newline_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        )));
        assert!(!is_newline_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn composer_ctrl_u_deletes_to_the_start_of_the_line() {
        let mut input = ComposerInput::from_text("keep this");

        input.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

        assert_eq!(input.text(), "");
    }

    #[test]
    fn composer_ctrl_minus_undoes_the_last_edit() {
        let mut input = ComposerInput::default();
        input.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        input.handle_key(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::CONTROL));

        assert_eq!(input.text(), "");
    }

    #[test]
    fn transcript_uses_a_blank_bottom_row_instead_of_a_separator_line() {
        let backend = TestBackend::new(40, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::Assistant {
            text: "answer".to_string(),
            streaming: false,
            error: None,
        }];
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal
            .draw(|frame| draw_transcript(frame, frame.area(), &app, 0, palette))
            .unwrap();

        let buffer = terminal.backend().buffer();
        for x in 0..40 {
            assert_eq!(buffer[(x, 3)].symbol(), " ");
            assert_eq!(buffer[(x, 3)].bg, Color::Reset);
        }
    }

    #[test]
    fn user_and_assistant_leave_one_blank_row_before_the_composer() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = demo_app();
        app.transcript = vec![
            TranscriptItem::User("question".to_string()),
            TranscriptItem::Assistant {
                text: "answer".to_string(),
                streaming: false,
                error: None,
            },
        ];
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));

        terminal
            .draw(|frame| draw_transcript(frame, frame.area(), &app, 0, palette))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let user_y = find_buffer_row(buffer, 40, 6, "question").unwrap();
        let assistant_y = find_buffer_row(buffer, 40, 6, "answer").unwrap();
        let transcript_bottom = 6u16;
        assert_eq!(assistant_y.saturating_sub(user_y), 3);
        assert_eq!(transcript_bottom.saturating_sub(assistant_y), 2);
        for x in 0..40 {
            assert_eq!(buffer[(x, transcript_bottom - 1)].symbol(), " ");
        }
    }

    #[test]
    fn transcript_surfaces_keep_one_external_blank_row_between_items_and_composer() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = demo_app();
        app.transcript = vec![
            TranscriptItem::User("first request".to_string()),
            TranscriptItem::Assistant {
                text: "first answer".to_string(),
                streaming: false,
                error: None,
            },
            TranscriptItem::User("second request".to_string()),
        ];
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));

        terminal
            .draw(|frame| draw_transcript(frame, frame.area(), &app, 0, palette))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let first_user_y = find_buffer_row(buffer, 40, 10, "first request").unwrap();
        let assistant_y = find_buffer_row(buffer, 40, 10, "first answer").unwrap();
        let second_user_y = find_buffer_row(buffer, 40, 10, "second request").unwrap();
        assert_eq!(assistant_y.saturating_sub(first_user_y), 3);
        assert_eq!(second_user_y.saturating_sub(assistant_y), 3);
        for y in [first_user_y + 2, assistant_y + 1, 9] {
            assert_eq!(buffer[(20, y)].symbol(), " ");
            assert_eq!(buffer[(20, y)].bg, Color::Reset);
        }
    }

    #[test]
    fn dark_mode_styles_submitted_and_active_composers_without_a_separator() {
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::User("dark request".to_string())];
        app.input.clear();
        app.queue = QueueSnapshot::default();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        let buffer = terminal.backend().buffer();
        let user_y = find_buffer_row(buffer, 80, 12, "dark request").unwrap();
        let composer_y = find_buffer_row(buffer, 80, 12, "Ask pi to do anything").unwrap();
        for y in user_y.saturating_sub(1)..=user_y.saturating_add(1) {
            assert_eq!(buffer[(0, y)].bg, Color::Rgb(17, 17, 17));
            assert_eq!(buffer[(79, y)].bg, Color::Rgb(17, 17, 17));
        }
        assert_eq!(
            buffer[(0, composer_y.saturating_sub(1))].bg,
            Color::Rgb(17, 17, 17)
        );
        assert_eq!(
            buffer[(79, composer_y.saturating_add(1))].bg,
            Color::Rgb(17, 17, 17)
        );
        let transcript_bottom_y = composer_y.saturating_sub(2);
        assert_eq!(buffer[(40, transcript_bottom_y)].symbol(), " ");
        assert_eq!(buffer[(40, transcript_bottom_y)].bg, Color::Reset);
    }

    #[test]
    fn assistant_code_block_keeps_a_full_width_background() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::Assistant {
            text: "```rust\nfn main() {}\n```".to_string(),
            streaming: false,
            error: None,
        }];
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal
            .draw(|frame| draw_transcript(frame, frame.area(), &app, 0, palette))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let code_y = find_buffer_row(buffer, 40, 6, "fn main").expect("rendered Rust code");
        for x in 0..40 {
            assert_eq!(buffer[(x, code_y)].bg, Color::Rgb(232, 232, 236));
        }
    }

    #[test]
    fn submitted_prompt_keeps_a_full_width_composer_surface_in_place() {
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::User("submitted prompt".to_string())];
        app.input.clear();
        app.queue = QueueSnapshot::default();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        let buffer = terminal.backend().buffer();
        let user_y = find_buffer_row(buffer, 80, 12, "submitted prompt").unwrap();
        let composer_y = find_buffer_row(buffer, 80, 12, "Ask pi to do anything").unwrap();
        assert_eq!(composer_y.saturating_sub(user_y), 4);
        for y in user_y.saturating_sub(1)..=user_y.saturating_add(1) {
            assert_eq!(buffer[(0, y)].bg, Color::Rgb(245, 245, 245));
            assert_eq!(buffer[(79, y)].bg, Color::Rgb(245, 245, 245));
        }
    }

    #[test]
    fn transcript_status_dot_aligns_with_the_composer_icon() {
        let mut app = demo_app();
        app.transcript = vec![
            TranscriptItem::User("aligned request".to_string()),
            TranscriptItem::Assistant {
                text: "response".to_string(),
                streaming: false,
                error: None,
            },
        ];
        app.input.clear();
        app.queue = QueueSnapshot::default();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        let buffer = terminal.backend().buffer();
        let (dot_x, _) = find_buffer_position(buffer, 80, 12, STATUS_DOT).unwrap();
        let (_, composer_y) =
            find_buffer_position(buffer, 80, 12, "Ask pi to do anything").unwrap();
        let composer_icon_x = (0..80)
            .find(|&x| buffer[(x, composer_y)].symbol() == "›")
            .unwrap();
        assert_eq!(dot_x, composer_icon_x);
    }

    fn find_buffer_row(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<u16> {
        (0..height).find(|&y| {
            let row = (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            row.contains(needle)
        })
    }

    fn find_buffer_position(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        (0..height).find_map(|y| {
            (0..width)
                .find(|&x| {
                    (x..width)
                        .map(|column| buffer[(column, y)].symbol())
                        .collect::<String>()
                        .starts_with(needle)
                })
                .map(|x| (x, y))
        })
    }

    #[test]
    fn colorfgbg_uses_the_last_ansi_slot_as_background() {
        assert_eq!(
            colorfgbg_background("0;15"),
            Some(RgbColor::new(255, 255, 255))
        );
        assert_eq!(colorfgbg_background("15;0"), Some(RgbColor::new(0, 0, 0)));
        assert_eq!(colorfgbg_background("0;256"), None);
        assert_eq!(colorfgbg_background("invalid"), None);
    }

    #[test]
    fn empty_state_uses_a_compact_startup_card() {
        let mut app = demo_app();
        app.transcript.clear();
        app.input.clear();
        app.queue = QueueSnapshot::default();

        let screen = render_app(&app, 80, 20);

        assert!(screen.contains("π_ pi (v0.1.0)"));
        assert!(screen.contains("model"));
        assert!(screen.contains("directory"));
        assert!(screen.contains("gpt-5.6-sol"));
        assert!(screen.contains("Ask pi to do anything"));
        assert!(screen.contains("Tip: type / for commands or ! for shell"));
        let card_top = screen.lines().find(|line| line.contains('╭')).unwrap();
        assert_eq!(UnicodeWidthStr::width(card_top), 78);
    }

    #[test]
    fn narrow_layout_stays_renderable() {
        let mut app = demo_app();
        app.input.set_text("/");
        let screen = render_app(&app, 38, 12);
        assert!(screen.contains("/new"));
        assert!(screen.contains("tab complete"));
    }

    #[test]
    fn skill_palette_never_shortens_the_skill_name_with_an_ellipsis() {
        let mut app = demo_app();
        app.command_specs = vec![CommandSpec {
            name: "skill:a-very-long-skill-name".to_string(),
            description: "A description that may be shortened instead".to_string(),
            argument_hint: Some("[task]".to_string()),
        }];
        app.input.set_text("/skill:");

        let screen = render_app(&app, 42, 12);

        assert!(screen.contains("/skill:a-very-long-skill-name"));
        assert!(!screen.contains("/skill:a-very-long-s…"));
    }

    #[test]
    fn skill_prefix_exposes_discovered_skill_commands() {
        let specs = vec![CommandSpec {
            name: "skill:code-review".to_string(),
            description: "Review the current changes".to_string(),
            argument_hint: Some("[task]".to_string()),
        }];
        let suggestions = command_suggestions("/skill:", &specs);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].invocation, "/skill:code-review");
        assert_eq!(suggestions[0].description, "Review the current changes");

        let mut app = demo_app();
        app.input.set_text("/skill:");
        let screen = render_app(&app, 100, 24);
        assert!(screen.contains("↑↓ select · tab complete · 1/1"));
        assert!(screen.contains("/skill:code-review"));
        assert!(screen.contains("Review changes against repository standards"));
        complete_selected_command(&mut app);
        assert_eq!(app.input.text(), "/skill:code-review ");
    }

    #[test]
    fn command_selection_moves_and_wraps() {
        let mut app = demo_app();
        app.input.set_text("/");
        let suggestion_count = command_suggestions(&app.input.text(), &app.command_specs).len();

        move_command_selection(&mut app, -1);
        assert_eq!(app.command_selection, suggestion_count - 1);

        move_command_selection(&mut app, 1);
        assert_eq!(app.command_selection, 0);

        move_command_selection(&mut app, 1);
        assert_eq!(app.command_selection, 1);
    }

    #[test]
    fn completion_uses_the_selected_command() {
        let mut app = demo_app();
        app.input.set_text("/");
        app.command_selection = 2;

        assert!(complete_selected_command(&mut app));
        assert_eq!(app.input.text(), "/reload");
        assert_eq!(app.command_selection, 0);
    }

    #[test]
    fn command_panel_scrolls_to_keep_the_selection_visible() {
        let mut app = demo_app();
        app.command_specs = (0..7)
            .map(|index| CommandSpec {
                name: format!("custom:command-{index}"),
                description: format!("Custom command number {index}"),
                argument_hint: None,
            })
            .collect();
        app.input.set_text("/custom:");
        app.command_selection = 4;

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("↑↓ select · tab complete · 5/7"));
        assert!(screen.contains("› /custom:command-4"));
        assert!(!screen.contains("/custom:command-0"));
        assert!(screen.contains("/custom:command-6"));
    }

    #[tokio::test]
    async fn app_reads_skill_commands_from_the_active_runtime_generation() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(
                pi_plugin_openai::OpenAiCompatiblePlugin::new(
                    pi_plugin_openai::OpenAiCompatibleConfig::without_api_key(
                        "https://example.invalid/v1",
                    ),
                )
                .unwrap(),
            )
            .agent_plugin(pi_plugin_skills::SkillsPlugin::from_skills([
                pi_plugin_skills::SkillInfo {
                    name: "runtime-skill".to_string(),
                    description: "Loaded by the active runtime".to_string(),
                    file_path: directory.path().join("runtime-skill/SKILL.md"),
                    content: "runtime body".to_string(),
                    disable_model_invocation: false,
                },
            ]))
            .agent_options(pi_agent::AgentOptions {
                provider_id: pi_core::ProviderId::new("openai-compatible"),
                cwd: directory.path().to_path_buf(),
                ..pi_agent::AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();

        let app = App::new(&session, &session.snapshot());
        let suggestions = command_suggestions("/skill:", &app.command_specs);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].invocation, "/skill:runtime-skill");
        assert_eq!(suggestions[0].description, "Loaded by the active runtime");
        session.shutdown().await;
    }

    #[tokio::test]
    async fn model_command_opens_models_from_the_active_runtime_registry() {
        let directory = tempfile::tempdir().unwrap();
        let models_path = directory.path().join("models.json");
        std::fs::write(
            &models_path,
            r#"{
              "providers": {
                "custom": {
                  "baseUrl": "https://example.invalid/v1",
                  "api": "openai-completions",
                  "models": [
                    { "id": "alpha", "name": "Alpha Registered" },
                    { "id": "beta", "name": "Beta Registered" }
                  ]
                }
              }
            }"#,
        )
        .unwrap();
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(
                pi_plugin_models::ModelsPlugin::load(pi_plugin_models::ModelsPluginOptions::new(
                    &models_path,
                ))
                .unwrap(),
            )
            .agent_options(pi_agent::AgentOptions {
                provider_id: ProviderId::new("custom"),
                model_id: ModelId::new("alpha"),
                cwd: directory.path().to_path_buf(),
                ..pi_agent::AgentOptions::default()
            })
            .system_prompt(pi_runtime::SystemPrompt::Final("test".to_string()))
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let mut app = App::new(&session, &session.snapshot());
        app.input.set_text("/model");

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("/model custom/alpha"));
        assert!(screen.contains("Alpha Registered"));
        assert!(screen.contains("/model custom/beta"));
        assert!(screen.contains("Beta Registered"));
        assert!(screen.contains("↑↓ select · type to filter · enter apply"));
        session.shutdown().await;
    }

    #[test]
    fn resume_catalog_discovers_session_information_for_the_current_cwd() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        let cwd = directory.path().join("workspace");
        let other_cwd = directory.path().join("other-workspace");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&other_cwd).unwrap();

        let current_path = sessions.join("current.jsonl");
        let current = pi_session::SessionLog::create(
            &current_path,
            pi_session::SessionHeader::new("current", &cwd),
        )
        .unwrap();
        current
            .append_message(Message::User(pi_core::UserMessage::text(
                "Current conversation",
                0,
            )))
            .unwrap();

        let previous_path = sessions.join("previous.jsonl");
        let previous = pi_session::SessionLog::create(
            &previous_path,
            pi_session::SessionHeader::new("previous", &cwd),
        )
        .unwrap();
        previous
            .append_message(Message::User(pi_core::UserMessage::text(
                "Fix the resume selector",
                0,
            )))
            .unwrap();
        previous
            .set_name(Some("Resume polish".to_string()))
            .unwrap();

        pi_session::SessionLog::create(
            sessions.join("other.jsonl"),
            pi_session::SessionHeader::new("other", &other_cwd),
        )
        .unwrap();

        let choices = discover_session_choices(&current_path, &cwd);

        assert_eq!(choices.len(), 2);
        assert!(choices.iter().any(|choice| choice.current));
        let previous = choices
            .iter()
            .find(|choice| choice.id == "previous")
            .unwrap();
        assert_eq!(previous.name.as_deref(), Some("Resume polish"));
        assert_eq!(previous.first_message, "Fix the resume selector");
        assert_eq!(previous.message_count, 1);
        assert_eq!(previous.path, previous_path);
    }

    #[test]
    fn resume_command_lists_filters_and_applies_session_information() {
        let mut app = demo_app();
        app.session_choices = vec![
            SessionChoice {
                path: PathBuf::from("/tmp/sessions/resume polish.jsonl"),
                id: "resume-polish".to_string(),
                cwd: PathBuf::from("/Users/cherry/Documents/pi_rs"),
                name: Some("Resume polish".to_string()),
                first_message: "Improve the session picker".to_string(),
                message_count: 12,
                modified_at_ms: unix_time_ms(),
                current: false,
            },
            SessionChoice {
                path: PathBuf::from("/tmp/sessions/older.jsonl"),
                id: "older".to_string(),
                cwd: PathBuf::from("/Users/cherry/Documents/pi_rs"),
                name: None,
                first_message: "Older conversation".to_string(),
                message_count: 4,
                modified_at_ms: 0,
                current: true,
            },
        ];
        app.input.set_text("/resume");

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("Resume polish"));
        assert!(screen.contains("Older conversation"));
        assert!(screen.contains("12 msgs"));
        assert!(screen.contains("current"));
        assert!(screen.contains("type to filter · enter resume"));

        app.input.set_text("/resume polish");
        let suggestions = suggestions_for_app(&app);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].label.as_deref(), Some("Resume polish"));
        assert!(!complete_selected_command_for_enter(&mut app));
        assert_eq!(
            app.input.text(),
            "/resume /tmp/sessions/resume polish.jsonl"
        );
    }

    #[test]
    fn model_selector_filters_and_applies_the_selected_registered_model() {
        let mut app = demo_app();
        app.provider = "custom".to_string();
        app.model = "alpha".to_string();
        app.model_specs = vec![
            ModelSpec::new("custom", "alpha", "Alpha Registered", "openai-completions"),
            ModelSpec::new("custom", "beta", "Beta Registered", "openai-completions"),
        ];
        app.input.set_text("/model beta");

        let suggestions = suggestions_for_app(&app);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].invocation, "/model custom/beta");
        assert!(suggestions[0].apply_on_enter);
        assert!(!complete_selected_command_for_enter(&mut app));
        assert_eq!(app.input.text(), "/model custom/beta");
    }

    #[test]
    fn unmatched_command_query_uses_the_generic_empty_state() {
        let mut app = demo_app();
        app.command_specs.clear();
        app.input.set_text("/missing");
        let screen = render_app(&app, 80, 20);
        assert!(screen.contains("No matching commands"));
        assert!(!screen.contains("skill"));
        assert_eq!(
            incomplete_command_status(&app.input.text(), &app.command_specs),
            Some("No matching commands")
        );
    }

    #[test]
    fn command_submission_feedback_is_generic_for_every_registered_prefix() {
        let specs = vec![CommandSpec {
            name: "custom:deploy".to_string(),
            description: "Deploy the project".to_string(),
            argument_hint: None,
        }];

        assert_eq!(
            incomplete_command_status("/custom:", &specs),
            Some("Choose a command · ↑↓ select · Tab complete")
        );
        assert_eq!(incomplete_command_status("/custom:deploy", &specs), None);
    }

    #[test]
    fn cursor_math_uses_terminal_cell_width() {
        assert_eq!(input_cursor_position("你好", 8), (0, 4));
        assert_eq!(input_cursor_position("你好", 3), (1, 2));
        assert_eq!(input_cursor_position("abc", 3), (1, 0));
        assert_eq!(input_cursor_position("a\n好", 8), (1, 2));
    }

    #[test]
    fn truncation_preserves_requested_display_width() {
        let end = truncate_end("你好 world", 7);
        let start = truncate_start("/Users/cherry/Documents/pi_rs", 12);
        assert!(UnicodeWidthStr::width(end.as_str()) <= 7);
        assert!(UnicodeWidthStr::width(start.as_str()) <= 12);
        assert!(end.ends_with('…'));
        assert!(start.starts_with('…'));
    }
}
