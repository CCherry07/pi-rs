use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Stdout, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    StreamEvent, ThinkingLevel, ToolCallId, UiMultiSelectAction, UiMultiSelectOption,
    UiMultiSelectResponse,
};
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionSnapshot, EntryOrder, EntryQuery, ForkPosition,
    PiSession, QueueSnapshot, SessionEntry, SessionRuntimeInventory, ShellExecutionOptions,
    SubmitOutcome, aggregate_document_usage, current_session_context_tokens,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Widget, Wrap,
};
use ratatui_textarea::{
    CursorMove, Input as TextAreaInput, Key as TextAreaKey, TextArea, WrapMode,
};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc};
use termina::style::RgbColor;
use termina::{Event as TerminaEvent, PlatformTerminal, Terminal as _};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::clipboard::{ClipboardWriter, SystemClipboard};
use crate::config::AuthCommand;
use crate::output::{assistant_text, shell_command};
use crate::plugin_ui::{
    PluginConfirmationRequest, PluginMultiSelectionRequest, PluginSelectionRequest,
};
use crate::project_trust::{ProjectTrustOption, ProjectTrustPromptRequest, ProjectTrustService};
use crate::text_selection::{ScreenSelection, ScreenTextSurface};
use crate::{InteractiveRequestReceivers, auth, auth::AuthProviderInfo};

mod components;
mod controller;
mod message;
mod view;

use components::{ComposerInput, SelectionList};
use controller::*;
use message::AppMessage;
use view::*;

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

struct TerminalSession {
    terminal: TuiTerminal,
    fullscreen: bool,
    active: bool,
}

impl TerminalSession {
    fn new(fullscreen: bool) -> io::Result<Self> {
        Ok(Self {
            terminal: setup_terminal(fullscreen)?,
            fullscreen,
            active: true,
        })
    }

    fn terminal_mut(&mut self) -> &mut TuiTerminal {
        &mut self.terminal
    }

    fn finish(mut self) -> io::Result<()> {
        self.restore()
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        restore_terminal(&mut self.terminal, self.fullscreen)
    }

    fn resume(&mut self) -> io::Result<()> {
        if self.active {
            return Ok(());
        }
        self.terminal = setup_terminal(self.fullscreen)?;
        self.active = true;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

const TERMINAL_COLOR_QUERY_TIMEOUT: Duration = Duration::from_millis(80);
const ACTIVITY_ANIMATION_INTERVAL: Duration = Duration::from_millis(80);
const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);
const ACTIVITY_FRAME_COUNT: usize = 11;
const STATUS_DOT: &str = "•";
const MOUSE_SCROLL_LINES_PER_TICK: usize = 3;
const PAGE_SCROLL_OVERLAP: usize = 4;
const COMPOSER_TEXT_OFFSET: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollDirection {
    Up,
    Down,
}

#[derive(Debug)]
struct ScrollInputNormalizer {
    events_per_tick: usize,
    carried_lines: usize,
    direction: Option<ScrollDirection>,
}

impl ScrollInputNormalizer {
    fn for_terminal() -> Self {
        let events_per_tick = match std::env::var("TERM_PROGRAM").as_deref() {
            Ok("WarpTerminal") => 9,
            Ok("WezTerm" | "iTerm.app" | "vscode") => 1,
            Ok("Apple_Terminal" | "ghostty" | "kitty") => 3,
            _ => 3,
        };
        Self::with_events_per_tick(events_per_tick)
    }

    fn with_events_per_tick(events_per_tick: usize) -> Self {
        Self {
            events_per_tick: events_per_tick.max(1),
            carried_lines: 0,
            direction: None,
        }
    }

    fn lines(&mut self, direction: ScrollDirection) -> usize {
        if self.direction != Some(direction) {
            self.carried_lines = 0;
            self.direction = Some(direction);
        }
        self.carried_lines = self
            .carried_lines
            .saturating_add(MOUSE_SCROLL_LINES_PER_TICK);
        let lines = self.carried_lines / self.events_per_tick;
        self.carried_lines %= self.events_per_tick;
        lines
    }
}

pub(crate) async fn select_project_trust(
    fullscreen: bool,
    cwd: &Path,
    options: &[ProjectTrustOption],
) -> Result<Option<usize>, String> {
    let mut terminal = TerminalSession::new(fullscreen).map_err(|error| error.to_string())?;
    let result = select_project_trust_loop(terminal.terminal_mut(), cwd, options).await;
    let restored = terminal.finish().map_err(|error| error.to_string());
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

fn markdown_theme(appearance: TerminalAppearance) -> pi_md::MarkdownTheme {
    let appearance = match appearance {
        TerminalAppearance::Light => pi_md::Appearance::Light,
        TerminalAppearance::Dark => pi_md::Appearance::Dark,
    };
    pi_md::MarkdownTheme::new(appearance)
}

// Editable composer behavior lives in `tui/components/composer.rs`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiPalette {
    composer_background: Option<Color>,
    accent: Color,
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
            accent: match appearance {
                TerminalAppearance::Light => Color::Rgb(0, 95, 135),
                TerminalAppearance::Dark => Color::Cyan,
            },
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
        (255, 12)
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
    Notice(String),
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

#[derive(Debug)]
struct CachedTranscriptBlock {
    user_text: Option<String>,
    lines: Vec<Line<'static>>,
    streaming: bool,
    working: bool,
    start: usize,
    content_height: usize,
    height: usize,
    code_background_rows: Vec<(usize, usize)>,
}

#[derive(Debug)]
struct CachedTranscriptLayout {
    blocks: Vec<CachedTranscriptBlock>,
    startup_height: usize,
    line_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TranscriptLayoutKey {
    width: u16,
    gutter: u16,
    appearance: TerminalAppearance,
    working_elapsed_width: usize,
    tools_expanded: bool,
}

#[derive(Debug)]
struct TranscriptLayoutEntry {
    key: TranscriptLayoutKey,
    layout: Arc<CachedTranscriptLayout>,
}

#[derive(Debug, Default)]
struct TranscriptLayoutCache {
    transcript: Vec<TranscriptItem>,
    show_startup_header: bool,
    show_working_placeholder: bool,
    entries: Vec<TranscriptLayoutEntry>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionEntryChoice {
    id: String,
    label: String,
    description: String,
    current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BottomPaneView {
    Model,
    Thinking,
    Resume,
    Tree,
    Fork,
    Login,
    Logout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthOperation {
    Login,
    Logout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthRequest {
    operation: AuthOperation,
    provider: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThinkingChoice {
    level: ThinkingLevel,
    description: &'static str,
}

const THINKING_CHOICES: [ThinkingChoice; 7] = [
    ThinkingChoice {
        level: ThinkingLevel::Off,
        description: "No reasoning",
    },
    ThinkingChoice {
        level: ThinkingLevel::Minimal,
        description: "Very brief reasoning (~1k tokens)",
    },
    ThinkingChoice {
        level: ThinkingLevel::Low,
        description: "Light reasoning (~2k tokens)",
    },
    ThinkingChoice {
        level: ThinkingLevel::Medium,
        description: "Moderate reasoning (~8k tokens)",
    },
    ThinkingChoice {
        level: ThinkingLevel::High,
        description: "Deep reasoning (~16k tokens)",
    },
    ThinkingChoice {
        level: ThinkingLevel::XHigh,
        description: "Extra-high reasoning (~32k tokens)",
    },
    ThinkingChoice {
        level: ThinkingLevel::Max,
        description: "Maximum reasoning",
    },
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RegisteredPluginInventory {
    js_extensions: Vec<String>,
    rust_plugins: Vec<String>,
}

impl RegisteredPluginInventory {
    fn from_runtime(inventory: &SessionRuntimeInventory) -> Self {
        Self {
            js_extensions: inventory.js_extensions().to_vec(),
            rust_plugins: inventory
                .configured_native_plugins()
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

struct App {
    transcript: Vec<TranscriptItem>,
    show_startup_header: bool,
    transcript_layout_cache: RefCell<TranscriptLayoutCache>,
    input: ComposerInput,
    input_history: InputHistory,
    status: String,
    queue: QueueSnapshot,
    command_specs: Vec<CommandSpec>,
    model_specs: Vec<ModelSpec>,
    registered_plugins: RegisteredPluginInventory,
    session_choices: Vec<SessionChoice>,
    tree_choices: Vec<SessionEntryChoice>,
    fork_choices: Vec<SessionEntryChoice>,
    login_providers: Vec<AuthProviderInfo>,
    logout_providers: Vec<AuthProviderInfo>,
    command_palette: RefCell<SelectionList>,
    dismissed_completion: Option<String>,
    view_stack: Vec<BottomPaneView>,
    view_selection: RefCell<SelectionList>,
    streaming_assistant: Option<usize>,
    awaiting_assistant: bool,
    working_started_at: Option<Instant>,
    bash_line: Option<usize>,
    provider: String,
    model: String,
    thinking: String,
    cwd: String,
    session_name: Option<String>,
    session_tokens: u64,
    context_tokens: Option<u64>,
    is_running: bool,
    compacting: bool,
    tools_expanded: bool,
    animation_frame: usize,
    scroll_from_bottom: usize,
    scroll_input: ScrollInputNormalizer,
    screen_selection: Option<ScreenSelection>,
    trust_prompt: Option<TrustPromptState>,
    confirmation_prompt: Option<ConfirmationPromptState>,
    selection_prompt: Option<SelectionPromptState>,
    multi_selection_prompt: Option<MultiSelectionPromptState>,
    pending_auth: Option<AuthRequest>,
    epoch: u64,
    quit: bool,
}

impl App {
    fn new(session: &AgentSession, snapshot: &AgentSessionSnapshot) -> Self {
        let mut transcript = Vec::new();
        let mut session_tokens = 0;
        let mut context_tokens = Some(latest_context_usage(&snapshot.agent.messages).tokens);
        if let Ok(document) = session.log().load()
            && let Ok(branch) = document.branch()
        {
            for record in &branch {
                push_history_entry(&mut transcript, &record.entry);
            }
            session_tokens = aggregate_document_usage(&document).total_tokens;
            let messages = snapshot
                .agent
                .messages
                .iter()
                .cloned()
                .map(pi_session::AgentMessage::from)
                .collect::<Vec<_>>();
            context_tokens = current_session_context_tokens(
                &branch
                    .iter()
                    .map(|record| (*record).clone())
                    .collect::<Vec<_>>(),
                &messages,
            )
            .map(|estimate| estimate.tokens);
        }
        let streaming_assistant = snapshot
            .agent
            .streaming_message
            .as_ref()
            .and_then(|stream| stream.snapshot())
            .and_then(|message| {
                let text = assistant_text(&Message::Assistant(Arc::new(message)))?;
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
        let (tree_choices, fork_choices) = session_entry_choices(session);
        let input_history = InputHistory::from_transcript(&transcript);
        let registered_plugins =
            RegisteredPluginInventory::from_runtime(session.runtime_inventory());
        Self {
            transcript,
            show_startup_header: true,
            transcript_layout_cache: RefCell::new(TranscriptLayoutCache::default()),
            input: ComposerInput::default(),
            input_history,
            status: status.to_string(),
            queue: snapshot.queue.clone(),
            command_specs: session.runtime().command_specs(),
            model_specs: session.runtime().available_models(),
            registered_plugins,
            session_choices,
            tree_choices,
            fork_choices,
            login_providers: Vec::new(),
            logout_providers: Vec::new(),
            command_palette: RefCell::new(SelectionList::default()),
            dismissed_completion: None,
            view_stack: Vec::new(),
            view_selection: RefCell::new(SelectionList::default()),
            streaming_assistant,
            awaiting_assistant,
            working_started_at: snapshot.agent.is_running.then(Instant::now),
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
            scroll_input: ScrollInputNormalizer::for_terminal(),
            screen_selection: None,
            trust_prompt: None,
            confirmation_prompt: None,
            selection_prompt: None,
            multi_selection_prompt: None,
            pending_auth: None,
            epoch: 1,
            quit: false,
        }
    }

    /// Reduce one semantic UI message into application state.
    ///
    /// Terminal input is routed by the controller; runtime notifications enter
    /// through this message seam so the event loop does not mutate view state
    /// field-by-field.
    fn update(&mut self, message: AppMessage) {
        match message {
            AppMessage::SessionEvent { event, snapshot } => {
                self.screen_selection = None;
                self.apply_session_event(*event);
                self.sync_snapshot(&snapshot);
            }
            AppMessage::EffectCompleted(done) if done.epoch == self.epoch => {
                self.awaiting_assistant = false;
                match done.status {
                    Ok(status) if status.contains('\n') => {
                        self.transcript.push(TranscriptItem::Notice(status));
                        self.status = "Ready".to_string();
                    }
                    Ok(status) => self.status = status,
                    Err(error) => self.status = format!("Error: {error}"),
                }
            }
            AppMessage::EffectCompleted(_) => {}
            AppMessage::TrustRequested(request) => {
                self.screen_selection = None;
                self.trust_prompt = Some(request.into());
                self.status = "Choose project trust".to_string();
            }
            AppMessage::ConfirmationRequested(request) => {
                self.screen_selection = None;
                self.confirmation_prompt = Some(request.into());
                self.status = "Confirmation required".to_string();
            }
            AppMessage::SelectionRequested(request) => {
                self.screen_selection = None;
                self.selection_prompt = Some(request.into());
                self.status = "Selection required".to_string();
            }
            AppMessage::MultiSelectionRequested(request) => {
                self.screen_selection = None;
                self.multi_selection_prompt = Some(request.into());
                self.status = "Multi-selection required".to_string();
            }
            AppMessage::AnimationTick => self.advance_animation(),
            AppMessage::Quit => self.quit = true,
        }
    }

    fn sync_snapshot(&mut self, snapshot: &AgentSessionSnapshot) {
        self.queue = snapshot.queue.clone();
        self.provider = snapshot.agent.provider_id.to_string();
        self.model = snapshot.agent.model_id.to_string();
        self.thinking = snapshot.agent.thinking_level.as_str().to_string();
        self.session_name = snapshot.name.clone();
        let estimate = latest_context_usage(&snapshot.agent.messages);
        if self.context_tokens.is_some() {
            self.context_tokens = Some(estimate.tokens);
        }
        self.is_running = snapshot.agent.is_running;
        self.compacting = snapshot.compaction.is_some();
        if !self.is_running {
            self.awaiting_assistant = false;
            self.working_started_at = None;
        }
    }

    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.streaming_assistant = None;
        self.awaiting_assistant = false;
        self.working_started_at = None;
        self.bash_line = None;
        self.scroll_from_bottom = 0;
        self.screen_selection = None;
    }

    fn active_bottom_view(&self) -> Option<BottomPaneView> {
        self.view_stack.last().copied()
    }

    fn thinking_choices(&self) -> Vec<ThinkingChoice> {
        let Some(model) = self.model_specs.iter().find(|model| {
            model.provider.as_str() == self.provider && model.id.as_str() == self.model
        }) else {
            return THINKING_CHOICES.to_vec();
        };
        if !model.reasoning {
            return vec![THINKING_CHOICES[0]];
        }
        THINKING_CHOICES
            .iter()
            .copied()
            .filter(|choice| {
                let level = choice.level.as_str();
                match model.thinking_level_map.get(level) {
                    Some(None) => false,
                    Some(Some(_)) => true,
                    None => !matches!(choice.level, ThinkingLevel::XHigh | ThinkingLevel::Max),
                }
            })
            .collect()
    }

    fn push_bottom_view(&mut self, view: BottomPaneView) {
        self.view_stack.push(view);
        self.view_selection.get_mut().reset();
    }

    fn pop_bottom_view(&mut self) {
        self.view_stack.pop();
        self.view_selection.get_mut().reset();
    }

    fn refresh_auth_choices(&mut self, agent_dir: &Path) -> Result<(), String> {
        self.login_providers = auth::login_provider_catalog(agent_dir)?;
        self.logout_providers = auth::logout_provider_catalog(agent_dir)?;
        Ok(())
    }

    fn has_active_animation(&self) -> bool {
        self.screen_selection.is_none()
            && (self.working_started_at.is_some()
                || self.awaiting_assistant
                || self.streaming_assistant.is_some_and(|index| {
                    matches!(
                        self.transcript.get(index),
                        Some(TranscriptItem::Assistant {
                            streaming: true,
                            ..
                        })
                    )
                }))
    }

    fn advance_animation(&mut self) {
        self.animation_frame = (self.animation_frame + 1) % ACTIVITY_FRAME_COUNT;
    }

    fn working_elapsed_seconds(&self) -> u64 {
        self.working_started_at
            .map_or(0, |started_at| started_at.elapsed().as_secs())
    }

    fn apply_session_event(&mut self, event: AgentSessionEvent) {
        match event {
            AgentSessionEvent::Agent(event) => self.apply_agent_event(*event),
            AgentSessionEvent::AgentEnd { messages, .. } => {
                self.apply_agent_event(AgentEvent::AgentEnd { messages });
            }
            AgentSessionEvent::AgentSettled => {
                self.awaiting_assistant = false;
                self.working_started_at = None;
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
            AgentSessionEvent::CompactionEnd {
                result,
                aborted,
                error_message,
                ..
            } => {
                self.compacting = false;
                if result.is_some() && !aborted {
                    self.context_tokens = None;
                }
                self.status = error_message.unwrap_or_else(|| "Compaction complete".to_string());
            }
            AgentSessionEvent::EntryAppended { entry } => {
                self.session_tokens = self
                    .session_tokens
                    .saturating_add(session_entry_token_usage(&entry.entry));
            }
            AgentSessionEvent::UsageRecorded { usage } => {
                self.session_tokens = self
                    .session_tokens
                    .saturating_add(usage.input)
                    .saturating_add(usage.output)
                    .saturating_add(usage.cache_read)
                    .saturating_add(usage.cache_write);
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
            AgentSessionEvent::PluginNotice { message, .. } => {
                self.status.clone_from(&message);
                self.transcript.push(TranscriptItem::Notice(message));
            }
            _ => {}
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AgentStart => {
                self.awaiting_assistant = true;
                self.working_started_at.get_or_insert_with(Instant::now);
                self.is_running = true;
                self.status = "Agent running… Esc stops".to_string();
            }
            AgentEvent::MessageStart { message } => match message {
                Message::User(user) => {
                    self.transcript
                        .push(TranscriptItem::User(user_text(&user.content)));
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
                Message::Custom(message) => {
                    if message.display {
                        self.transcript.push(TranscriptItem::Notice(format!(
                            "[{}]\n{}",
                            message.custom_type,
                            custom_content_text(&message.content)
                        )));
                    }
                }
            },
            AgentEvent::MessageUpdate { update, .. } => {
                if let StreamEvent::TextDelta { delta, .. } = update.as_ref()
                    && let Some(index) = self.streaming_assistant
                    && let Some(TranscriptItem::Assistant {
                        text: current,
                        streaming,
                        ..
                    }) = self.transcript.get_mut(index)
                {
                    current.push_str(delta);
                    *streaming = true;
                }
            }
            AgentEvent::MessageEnd {
                message: Message::Assistant(message),
            } => {
                if !matches!(message.stop_reason, StopReason::Aborted | StopReason::Error)
                    && pi_session::calculate_context_tokens(&message.usage) > 0
                {
                    self.context_tokens = Some(assistant_context_usage(&message));
                }
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
                self.working_started_at = None;
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

struct ConfirmationPromptState {
    title: String,
    message: String,
    response: Option<tokio::sync::oneshot::Sender<bool>>,
}

struct SelectionPromptState {
    title: String,
    options: Vec<String>,
    selected: usize,
    response: Option<tokio::sync::oneshot::Sender<Option<usize>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultiSelectionFocus {
    List,
    Search,
    Filters,
}

struct MultiSelectionPromptState {
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
    focus: MultiSelectionFocus,
    summary_lines: Vec<String>,
    pending_action: Option<String>,
    response: Option<tokio::sync::oneshot::Sender<Option<UiMultiSelectResponse>>>,
}

impl MultiSelectionPromptState {
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

impl From<PluginConfirmationRequest> for ConfirmationPromptState {
    fn from(request: PluginConfirmationRequest) -> Self {
        Self {
            title: request.title,
            message: request.message,
            response: Some(request.response),
        }
    }
}

impl From<PluginSelectionRequest> for SelectionPromptState {
    fn from(request: PluginSelectionRequest) -> Self {
        Self {
            title: request.title,
            options: request.options,
            selected: 0,
            response: Some(request.response),
        }
    }
}

impl From<PluginMultiSelectionRequest> for MultiSelectionPromptState {
    fn from(request: PluginMultiSelectionRequest) -> Self {
        let mut active_categories = request
            .request
            .initial_active_categories
            .into_iter()
            .collect::<HashSet<_>>();
        if active_categories.is_empty() {
            active_categories.extend(request.request.categories.iter().map(|(id, _)| id.clone()));
        }
        let sort_mode = request
            .request
            .initial_sort_mode
            .min(request.request.sort_modes.len().saturating_sub(1));
        Self {
            title: request.request.title,
            options: request.request.options,
            actions: request.request.actions,
            categories: request.request.categories,
            active_categories,
            pending_categories: None,
            sort_modes: request.request.sort_modes,
            sort_mode,
            selected: request.request.initially_selected.into_iter().collect(),
            cursor: 0,
            filter_cursor: 0,
            query: request.request.initial_query,
            focus: MultiSelectionFocus::List,
            summary_lines: request.request.summary_lines,
            pending_action: None,
            response: Some(request.response),
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CtrlCAction {
    ClearComposer,
    Interrupt,
    Quit,
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
    refresh_transcript: bool,
}

pub(crate) async fn run(
    session_handle: PiSession,
    fullscreen: bool,
    initial_prompt: Option<String>,
    project_trust: ProjectTrustService,
    interactive_requests: InteractiveRequestReceivers,
    agent_dir: PathBuf,
) -> Result<(), String> {
    let palette = UiPalette::detect();
    let mut terminal = TerminalSession::new(fullscreen).map_err(|error| error.to_string())?;
    let result = run_loop(
        &mut terminal,
        session_handle,
        initial_prompt,
        palette,
        project_trust,
        interactive_requests,
        agent_dir,
    )
    .await;
    terminal.finish().map_err(|error| error.to_string())?;
    result
}

fn render_tui_frame(
    terminal: &mut TuiTerminal,
    app: &App,
    palette: UiPalette,
) -> Result<(ScreenTextSurface, u16), String> {
    let completed = terminal
        .draw(|frame| draw(frame, app, palette))
        .map_err(|error| error.to_string())?;
    let areas = ui_areas(completed.area, app);
    Ok((
        ScreenTextSurface::capture(completed.buffer),
        areas.transcript.height,
    ))
}

fn app_for_session(
    session: &AgentSession,
    snapshot: &AgentSessionSnapshot,
    agent_dir: &Path,
) -> App {
    let mut app = App::new(session, snapshot);
    if let Err(error) = app.refresh_auth_choices(agent_dir) {
        app.status = format!("Could not read provider credentials: {error}");
    }
    app
}

async fn run_loop(
    terminal: &mut TerminalSession,
    session_handle: PiSession,
    initial_prompt: Option<String>,
    palette: UiPalette,
    project_trust: ProjectTrustService,
    interactive_requests: InteractiveRequestReceivers,
    agent_dir: PathBuf,
) -> Result<(), String> {
    let InteractiveRequestReceivers {
        project_trust: mut trust_requests,
        plugin_confirmation: mut confirmation_requests,
        plugin_selection: mut selection_requests,
        plugin_multi_selection: mut multi_selection_requests,
    } = interactive_requests;
    let mut session_changes = session_handle.subscribe();
    let mut session = session_handle.current();
    let mut subscription = session.subscribe();
    let mut app = app_for_session(&session, &subscription.snapshot, &agent_dir);
    let (effect_sender, mut effect_receiver) = tokio::sync::mpsc::unbounded_channel();
    if let Some(prompt) = initial_prompt {
        app.input_history.record(&prompt);
        app.awaiting_assistant = true;
        app.working_started_at = Some(Instant::now());
        app.status = "Working…".to_string();
        spawn_effect(
            Arc::clone(&session),
            session_handle.clone(),
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
    let mut generation_status_override = None;
    let (mut surface, mut transcript_viewport_height) =
        render_tui_frame(terminal.terminal_mut(), &app, palette)?;
    let mut last_frame_at = Instant::now();
    let mut redraw_pending = false;
    while !app.quit {
        let next_frame_at = last_frame_at
            .checked_add(MIN_FRAME_INTERVAL)
            .unwrap_or(last_frame_at);
        tokio::select! {
            _ = tokio::time::sleep_until(next_frame_at.into()), if redraw_pending => {
                (surface, transcript_viewport_height) =
                    render_tui_frame(terminal.terminal_mut(), &app, palette)?;
                last_frame_at = Instant::now();
                redraw_pending = false;
                continue;
            }
            terminal_event = events.next() => {
                match terminal_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        let copied_selection = terminal.fullscreen
                            && handle_copy_shortcut(key, &mut app, &surface, &mut clipboard);
                        if !copied_selection {
                            app.screen_selection = None;
                            let mut ui = KeyUi {
                                clipboard: &mut clipboard,
                                transcript_viewport_height,
                            };
                            handle_key(
                                key,
                                &mut app,
                                &session,
                                &session_handle,
                                &project_trust,
                                &effect_sender,
                                &mut ui,
                            );
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) if terminal.fullscreen => {
                        handle_mouse_event(mouse, &mut app, &surface, &mut clipboard);
                    }
                    Some(Ok(Event::Paste(text)))
                        if app.trust_prompt.is_none()
                            && app.confirmation_prompt.is_none()
                            && app.selection_prompt.is_none()
                            && app.multi_selection_prompt.is_none() =>
                    {
                        app.screen_selection = None;
                        app.input.insert_str(text);
                        app.input_history.reset_navigation();
                        app.command_palette.get_mut().reset();
                    }
                    Some(Ok(Event::Resize(_, _))) => app.screen_selection = None,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.to_string()),
                    None => app.quit = true,
                }
            }
            session_event = subscription.events.recv() => match session_event {
                Ok(event) if event.revision > subscription.snapshot.revision => {
                    subscription.snapshot.revision = event.revision;
                    app.update(AppMessage::SessionEvent {
                        event: Box::new(event.event),
                        snapshot: Box::new(session.snapshot()),
                    });
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
                    let confirmation_prompt = app.confirmation_prompt.take();
                    let selection_prompt = app.selection_prompt.take();
                    let multi_selection_prompt = app.multi_selection_prompt.take();
                    let mut recovered = app_for_session(&session, &snapshot, &agent_dir);
                    recovered.input = input;
                    recovered.input_history = input_history;
                    recovered.epoch = epoch;
                    recovered.animation_frame = animation_frame;
                    recovered.tools_expanded = tools_expanded;
                    recovered.scroll_from_bottom = scroll_from_bottom;
                    recovered.trust_prompt = trust_prompt;
                    recovered.confirmation_prompt = confirmation_prompt;
                    recovered.selection_prompt = selection_prompt;
                    recovered.multi_selection_prompt = multi_selection_prompt;
                    recovered.status = "Caught up after UI lag".to_string();
                    app = recovered;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => app.update(AppMessage::Quit),
            },
            Some(done) = effect_receiver.recv() => {
                let current_epoch = done.epoch == app.epoch;
                let refresh_transcript = done.refresh_transcript;
                app.update(AppMessage::EffectCompleted(done));
                if current_epoch && refresh_transcript {
                    let status = app.status.clone();
                    let epoch = app.epoch;
                    let tools_expanded = app.tools_expanded;
                    let mut refreshed = app_for_session(&session, &session.snapshot(), &agent_dir);
                    refreshed.epoch = epoch;
                    refreshed.tools_expanded = tools_expanded;
                    refreshed.status = status;
                    app = refreshed;
                } else if current_epoch {
                    app.sync_snapshot(&session.snapshot());
                }
            }
            Some(request) = trust_requests.recv() => {
                app.update(AppMessage::TrustRequested(request));
            }
            Some(request) = confirmation_requests.recv() => {
                app.update(AppMessage::ConfirmationRequested(request));
            }
            Some(request) = selection_requests.recv() => {
                app.update(AppMessage::SelectionRequested(request));
            }
            Some(request) = multi_selection_requests.recv() => {
                app.update(AppMessage::MultiSelectionRequested(request));
            }
            _ = animation_tick.tick(), if app.has_active_animation() => {
                app.update(AppMessage::AnimationTick);
            }
            changed = session_changes.changed() => {
                if changed.is_err() {
                    app.quit = true;
                    continue;
                }
                session = session_handle.current();
                subscription = session.subscribe();
                let next_epoch = app.epoch.saturating_add(1);
                let tools_expanded = app.tools_expanded;
                app = app_for_session(&session, &subscription.snapshot, &agent_dir);
                app.epoch = next_epoch;
                app.tools_expanded = tools_expanded;
                app.status = generation_status_override
                    .take()
                    .unwrap_or_else(|| "Session generation replaced".to_string());
            }
        }
        if let Some(request) = app.pending_auth.take() {
            drop(events);
            let result = run_auth_request(terminal, &agent_dir, &request).await;
            events = EventStream::new();
            match result {
                Ok(status) => match session_handle.reload().await {
                    Ok(()) => {
                        let status = match app.refresh_auth_choices(&agent_dir) {
                            Ok(()) => status,
                            Err(error) => {
                                format!("Credential changed, but catalog refresh failed: {error}")
                            }
                        };
                        generation_status_override = Some(status.clone());
                        app.status = status;
                    }
                    Err(error) => {
                        app.status =
                            format!("Credential changed, but session reload failed: {error}");
                    }
                },
                Err(error) => app.status = format!("Authentication error: {error}"),
            }
        }
        redraw_pending = true;
    }
    session.abort();
    session.abort_shell();
    Ok(())
}

async fn run_auth_request(
    terminal: &mut TerminalSession,
    agent_dir: &Path,
    request: &AuthRequest,
) -> Result<String, String> {
    if let Err(error) = terminal.restore() {
        let _ = terminal.resume();
        return Err(format!("failed to suspend the TUI: {error}"));
    }
    let command = match request.operation {
        AuthOperation::Login => AuthCommand::Login {
            provider: Some(request.provider.clone()),
            api_key: false,
            oauth: false,
            oauth_token: false,
            token: None,
            refresh_token: None,
            expires: None,
        },
        AuthOperation::Logout => AuthCommand::Logout {
            provider: request.provider.clone(),
        },
    };
    let auth_result = auth::run(agent_dir, &command).await;
    let resume_result = terminal.resume().map_err(|error| error.to_string());
    match (auth_result, resume_result) {
        (Err(auth_error), Err(resume_error)) => Err(format!(
            "{auth_error}; additionally failed to restore the TUI: {resume_error}"
        )),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("failed to restore the TUI: {error}")),
        (Ok(()), Ok(())) => Ok(match request.operation {
            AuthOperation::Login => format!("Configured authentication for {}", request.provider),
            AuthOperation::Logout => format!(
                "Removed stored credential for {} · environment and models.json credentials are unchanged",
                request.provider
            ),
        }),
    }
}

// Input routing and effects live in `tui/controller.rs`.

// Layout and rendering live in `tui/view.rs`.

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
            "/login",
            "configure provider authentication",
            Some("[provider]"),
        ),
        ("/logout", "remove provider authentication", None),
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
        ("/compact", "compact context", Some("[instructions]")),
        ("/fork", "fork from a previous user message", None),
        ("/clone", "clone the session at its current position", None),
        ("/tree", "navigate the current session tree", None),
        ("/name", "set or show session name", Some("[name]")),
        ("/session", "show session info and stats", None),
        ("/copy", "copy last assistant message", None),
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

fn session_entry_choices(
    session: &AgentSession,
) -> (Vec<SessionEntryChoice>, Vec<SessionEntryChoice>) {
    let current = session.log().leaf_id();
    let records = session
        .log()
        .find_entries(&EntryQuery {
            order: EntryOrder::OldestFirst,
            ..EntryQuery::default()
        })
        .unwrap_or_default();
    let mut tree = Vec::new();
    let mut forks = Vec::new();
    for record in records {
        let label = match &record.entry {
            SessionEntry::Message(entry) => entry
                .message
                .as_standard()
                .map(message_choice_text)
                .unwrap_or_else(|| format!("{} message", entry.message.role())),
            SessionEntry::CustomMessage(message) => message.custom_type.clone(),
            SessionEntry::ModelChange(change) => {
                format!("Model: {}/{}", change.provider, change.model_id)
            }
            SessionEntry::ThinkingLevelChange(change) => {
                format!("Thinking: {}", change.thinking_level)
            }
            SessionEntry::ActiveToolsChange(_) => "Active tools changed".to_string(),
            SessionEntry::Compaction(_) => "Compaction summary".to_string(),
            SessionEntry::BranchSummary(_) => "Branch summary".to_string(),
            SessionEntry::Custom(change) => change.custom_type.clone(),
        };
        let id = record.id;
        let choice = SessionEntryChoice {
            current: current.as_deref() == Some(id.as_str()),
            id,
            label,
            description: format!("entry {}", record.seq),
        };
        if matches!(record.entry, SessionEntry::Message(ref entry) if entry.message.role() == "user")
        {
            forks.push(choice.clone());
        }
        tree.push(choice);
    }
    (tree, forks)
}

fn message_choice_text(message: &Message) -> String {
    match message {
        Message::User(message) => user_text(&message.content),
        Message::Assistant(_) => {
            assistant_text(message).unwrap_or_else(|| "Assistant message".to_string())
        }
        Message::ToolResult(message) => format!("Tool result: {}", message.tool_name),
        Message::Custom(message) => format!("{} message", message.custom_type),
    }
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

fn custom_content_text(content: &pi_core::CustomMessageContent) -> String {
    match content {
        pi_core::CustomMessageContent::Text(text) => text.clone(),
        pi_core::CustomMessageContent::Blocks(blocks) => user_text(blocks),
    }
}

fn push_history_entry(transcript: &mut Vec<TranscriptItem>, entry: &SessionEntry) {
    match entry {
        SessionEntry::Message(message) => push_history_message(transcript, &message.message),
        SessionEntry::CustomMessage(message) if message.display => {
            transcript.push(TranscriptItem::Notice(format!(
                "[{}]\n{}",
                message.custom_type,
                custom_content_text(&message.content)
            )));
        }
        _ => {}
    }
}

fn push_history_message(transcript: &mut Vec<TranscriptItem>, message: &pi_session::AgentMessage) {
    if let Some(standard) = message.as_standard() {
        match standard {
            Message::User(user) => {
                let display = message
                    .display_text()
                    .map(str::to_string)
                    .unwrap_or_else(|| user_text(&user.content));
                transcript.push(TranscriptItem::User(display));
            }
            Message::Assistant(assistant) => {
                let text = assistant_text(standard).unwrap_or_default();
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
            Message::Custom(message) => {
                if message.display {
                    transcript.push(TranscriptItem::Notice(format!(
                        "[{}]\n{}",
                        message.custom_type,
                        custom_content_text(&message.content)
                    )));
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
    use pi_session::{
        AgentSessionRuntimeRequest, AgentSessionRuntimeTarget, CompactionEntry,
        MultiSessionManager, SessionHeader, SessionLog, SessionRecord,
    };
    use ratatui::backend::TestBackend;
    use ratatui::{TerminalOptions, Viewport};

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

    struct FailingClipboard;

    fn billed_usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> pi_core::Usage {
        pi_core::Usage {
            input,
            output,
            cache_read,
            cache_write,
            total_tokens: input + output + cache_read + cache_write,
            ..pi_core::Usage::default()
        }
    }

    impl ClipboardWriter for FailingClipboard {
        fn set_text(&mut self, _text: &str) -> Result<(), String> {
            Err("clipboard denied".to_string())
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
            show_startup_header: false,
            transcript_layout_cache: RefCell::new(TranscriptLayoutCache::default()),
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
            registered_plugins: RegisteredPluginInventory::default(),
            session_choices: Vec::new(),
            tree_choices: Vec::new(),
            fork_choices: Vec::new(),
            login_providers: Vec::new(),
            logout_providers: Vec::new(),
            command_palette: RefCell::new(SelectionList::default()),
            dismissed_completion: None,
            view_stack: Vec::new(),
            view_selection: RefCell::new(SelectionList::default()),
            streaming_assistant: None,
            awaiting_assistant: false,
            working_started_at: None,
            bash_line: None,
            provider: "openai-compatible".to_string(),
            model: "gpt-5.6-sol".to_string(),
            thinking: "high".to_string(),
            cwd: "/workspace/project".to_string(),
            session_name: None,
            session_tokens: 12_450,
            context_tokens: Some(8_192),
            is_running: false,
            compacting: false,
            tools_expanded: false,
            animation_frame: 0,
            scroll_from_bottom: 0,
            scroll_input: ScrollInputNormalizer::with_events_per_tick(1),
            screen_selection: None,
            trust_prompt: None,
            confirmation_prompt: None,
            selection_prompt: None,
            multi_selection_prompt: None,
            pending_auth: None,
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
        let (service, _) = ProjectTrustService::new(
            &directory.path().join("agent"),
            None,
            true,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
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

    fn blank_surface(width: u16, height: u16) -> ScreenTextSurface {
        let area = Rect::new(0, 0, width, height);
        ScreenTextSurface::capture(&ratatui::buffer::Buffer::empty(area))
    }

    fn drag_screen_selection(
        app: &mut App,
        surface: &ScreenTextSurface,
        start: (u16, u16),
        end: (u16, u16),
    ) {
        for (kind, (column, row)) in [
            (MouseEventKind::Down(MouseButton::Left), start),
            (MouseEventKind::Drag(MouseButton::Left), end),
            (MouseEventKind::Up(MouseButton::Left), end),
        ] {
            handle_mouse(
                MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
                app,
                surface,
            );
        }
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
    fn multiline_command_results_are_added_to_the_transcript() {
        let mut app = demo_app();

        app.update(AppMessage::EffectCompleted(EffectDone {
            epoch: app.epoch,
            status: Ok("Session Info\n\nID: session-1".to_string()),
            refresh_transcript: false,
        }));

        assert_eq!(
            app.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Session Info\n\nID: session-1".to_string()
            ))
        );
        assert_eq!(app.status, "Ready");
    }

    #[test]
    fn common_session_commands_are_suggested_with_pi_compatible_arguments() {
        let plugin_commands = [
            CommandSpec {
                name: "export".to_string(),
                description: "Export session".to_string(),
                argument_hint: Some("[file]".to_string()),
            },
            CommandSpec {
                name: "import".to_string(),
                description: "Import session".to_string(),
                argument_hint: Some("<file.jsonl>".to_string()),
            },
            CommandSpec {
                name: "share".to_string(),
                description: "Share session".to_string(),
                argument_hint: None,
            },
        ];
        let suggestions = command_suggestions("/", &plugin_commands);

        let compact = suggestions
            .iter()
            .find(|suggestion| suggestion.invocation == "/compact")
            .unwrap();
        assert_eq!(compact.argument_hint.as_deref(), Some("[instructions]"));
        assert!(suggestions.iter().any(|item| item.invocation == "/name"));
        assert!(suggestions.iter().any(|item| item.invocation == "/session"));
        assert!(suggestions.iter().any(|item| item.invocation == "/copy"));
        assert!(suggestions.iter().any(|item| item.invocation == "/fork"));
        assert!(suggestions.iter().any(|item| item.invocation == "/clone"));
        assert!(suggestions.iter().any(|item| item.invocation == "/tree"));
        assert!(suggestions.iter().any(|item| item.invocation == "/export"));
        assert!(suggestions.iter().any(|item| item.invocation == "/import"));
        assert!(suggestions.iter().any(|item| item.invocation == "/share"));
    }

    #[test]
    fn help_text_is_a_readable_multiline_command_list() {
        let help = builtin_help_text(&[]);

        assert!(help.starts_with("Commands\n\n"));
        assert!(help.lines().any(|line| line.starts_with("- `/new [path]`")));
        assert!(help.lines().any(|line| line.starts_with("- `/help`")));
        assert!(help.lines().any(|line| line.starts_with("- `!cmd`")));
        assert!(help.lines().count() > 10);
    }

    #[test]
    fn generic_plugin_confirmation_renders_and_returns_the_decision() {
        let mut app = demo_app();
        let (response, decision) = tokio::sync::oneshot::channel();
        app.update(AppMessage::ConfirmationRequested(
            PluginConfirmationRequest {
                title: "Import session?".to_string(),
                message: "/tmp/session.jsonl\n\nThe current session will be replaced.".to_string(),
                response,
            },
        ));

        let confirmation = render_app(&app, 100, 20);
        assert!(confirmation.contains("Import session?"));
        assert!(confirmation.contains("/tmp/session.jsonl"));
        assert!(confirmation.contains("enter confirm · esc cancel"));
        assert!(handle_confirmation_prompt_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut app,
        ));
        assert!(app.confirmation_prompt.is_none());
        assert!(!decision.blocking_recv().unwrap());
    }

    #[test]
    fn generic_plugin_multi_selection_searches_filters_sorts_and_returns_an_action() {
        let mut app = demo_app();
        let (response, decision) = tokio::sync::oneshot::channel();
        app.update(AppMessage::MultiSelectionRequested(
            PluginMultiSelectionRequest {
                request: pi_core::UiMultiSelectRequest {
                    title: "Pi Hermes Memory — Procedural Skills".to_string(),
                    options: vec![
                        UiMultiSelectOption {
                            label: "[G] Alpha (~/.pi/skills/alpha/SKILL.md)".to_string(),
                            search_text: "Alpha global".to_string(),
                            category: Some("G".to_string()),
                            detail_lines: vec![
                                "Global alpha procedure".to_string(),
                                "global:alpha".to_string(),
                            ],
                            read_only: false,
                            sort_values: vec![
                                vec!["2026-01-01".to_string()],
                                vec!["2025-01-01".to_string()],
                                vec!["alpha".to_string()],
                            ],
                        },
                        UiMultiSelectOption {
                            label: "[P] Beta (.pi/skills/beta/SKILL.md)".to_string(),
                            search_text: "Beta project".to_string(),
                            category: Some("P".to_string()),
                            detail_lines: vec![
                                "Project beta procedure".to_string(),
                                "project:demo:beta".to_string(),
                            ],
                            read_only: false,
                            sort_values: vec![
                                vec!["2026-02-01".to_string()],
                                vec!["2025-02-01".to_string()],
                                vec!["beta".to_string()],
                            ],
                        },
                    ],
                    actions: vec![
                        UiMultiSelectAction {
                            id: "move-global".to_string(),
                            key: 'g',
                            label: "global".to_string(),
                            enabled: true,
                            confirmation: None,
                        },
                        UiMultiSelectAction {
                            id: "delete".to_string(),
                            key: 'd',
                            label: "delete".to_string(),
                            enabled: true,
                            confirmation: Some(
                                "Delete {count} selected skill{plural}? This cannot be undone. Press y to confirm or n to cancel.{read_only_note}"
                                    .to_string(),
                            ),
                        },
                    ],
                    categories: vec![
                        ("G".to_string(), "Global [G]".to_string()),
                        ("P".to_string(), "Project [P]".to_string()),
                    ],
                    sort_modes: vec![
                        ("Updated".to_string(), true),
                        ("Created".to_string(), true),
                        ("Name".to_string(), false),
                    ],
                    initially_selected: Vec::new(),
                    initial_query: String::new(),
                    initial_active_categories: Vec::new(),
                    initial_sort_mode: 0,
                    summary_lines: vec!["Select skills with space.".to_string()],
                },
                response,
            },
        ));

        let initial = render_app(&app, 110, 24);
        assert!(initial.contains("2 visible · 2 total · 0 selected · sort: Updated"));
        assert!(initial.contains("[P] Beta"));
        assert!(initial.contains("Project beta procedure"));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut app,
        ));
        let filtered = render_app(&app, 110, 24);
        assert!(filtered.contains("1 visible · 2 total · 0 selected"));
        assert!(filtered.contains("filters: [P]"));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            &mut app,
        ));
        let searched = render_app(&app, 110, 24);
        assert!(searched.contains("1 visible · 2 total · 1 selected"));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut app,
        ));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(render_app(&app, 110, 24).contains("Confirm delete: y yes"));
        assert!(handle_multi_selection_prompt_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            &mut app,
        ));
        assert!(app.multi_selection_prompt.is_none());
        assert_eq!(
            decision.blocking_recv().unwrap(),
            Some(UiMultiSelectResponse {
                selected: vec![1],
                action_id: "delete".to_string(),
                query: "bet".to_string(),
                active_categories: vec!["P".to_string()],
                sort_mode: 1,
            })
        );
    }

    #[tokio::test]
    async fn name_and_session_commands_use_the_active_session_log() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = MultiSessionManager::new(|request: AgentSessionRuntimeRequest| async move {
            let AgentSessionRuntimeTarget::Create { cwd, path, .. } = request.target else {
                unreachable!("this test does not replace the session")
            };
            let pi_runtime = pi_runtime::PiRuntime::builder()
                .provider_plugin(
                    pi_plugin_openai::OpenAiCompatiblePlugin::new(
                        pi_plugin_openai::OpenAiCompatibleConfig::without_api_key(
                            "https://example.invalid/v1",
                        ),
                    )
                    .unwrap(),
                )
                .agent_options(pi_agent::AgentOptions {
                    provider_id: ProviderId::new("openai-compatible"),
                    cwd,
                    ..pi_agent::AgentOptions::default()
                })
                .system_prompt(pi_runtime::SystemPrompt::Final("test".to_string()))
                .build()?;
            AgentSession::prepare_create(pi_runtime, path).await
        });
        let runtime = sessions
            .create_session(directory.path(), directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let session = runtime.current();

        let named = run_effect(
            &runtime,
            &session,
            "/name release polish".to_string(),
            EffectMode::Submit,
        )
        .await
        .unwrap();
        let info = run_effect(
            &runtime,
            &session,
            "/session".to_string(),
            EffectMode::Submit,
        )
        .await
        .unwrap();

        assert_eq!(named, "Session name set: release polish");
        assert_eq!(session.log().name().as_deref(), Some("release polish"));
        assert!(info.contains("Session Info"));
        assert!(info.contains("Name: release polish"));
        assert!(info.contains("Messages: 0"));
        sessions.shutdown().await.unwrap();
    }

    #[test]
    fn copy_command_copies_the_last_completed_assistant_message() {
        let mut app = demo_app();
        app.transcript.push(TranscriptItem::Assistant {
            text: "latest answer".to_string(),
            streaming: false,
            error: None,
        });
        let mut clipboard = RecordingClipboard::default();

        assert!(handle_copy_command(&mut app, &mut clipboard));

        assert_eq!(clipboard.text.as_deref(), Some("latest answer"));
        assert_eq!(app.status, "Copied last assistant message to clipboard");
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
        let mut app = demo_app();
        app.cwd = "/workspace/pi-rs".to_string();
        let expected_cwd = compact_path(&app.cwd);
        let screen = render_app(&app, 100, 32);
        if std::env::var_os("PI_PRINT_TUI_TEST").is_some() {
            println!("{screen}");
        }
        assert!(screen.contains('›'));
        assert!(screen.contains('•'));
        assert!(screen.contains("gpt-5.6-sol high"));
        assert!(screen.contains(&expected_cwd));
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

        let rendered = transcript_item_lines(&item, TerminalAppearance::Dark, 0, true, 0)
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
        assert_eq!(inline_code.style.bg, None);
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
            (242, 242, 244),
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
        let contrast = contrast_ratio(keyword.style.fg.expect("keyword foreground"), (31, 31, 33));

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
    fn working_status_shimmers_and_formats_elapsed_time_like_codex() {
        let first = working_status_line(TerminalAppearance::Light, 0, 65);
        let next = working_status_line(TerminalAppearance::Light, 1, 65);

        assert_eq!(first.to_string(), "• Working (1m 05s • esc to interrupt)");
        assert_eq!(format_elapsed_compact(0), "0s");
        assert_eq!(format_elapsed_compact(59), "59s");
        assert_eq!(format_elapsed_compact(3601), "1h 00m 01s");
        assert_eq!(first.spans[0].style.fg, first.spans[1].style.fg);
        assert_eq!(next.spans[0].style.fg, next.spans[1].style.fg);
        assert!(first.spans[0].style.add_modifier.contains(Modifier::BOLD));
        let first_colors = first.spans[1..=7]
            .iter()
            .map(|span| span.style.fg)
            .collect::<Vec<_>>();
        let next_colors = next.spans[1..=7]
            .iter()
            .map(|span| span.style.fg)
            .collect::<Vec<_>>();
        assert_ne!(first_colors, next_colors);
        assert!(
            first.spans[1..=7]
                .iter()
                .all(|span| { span.style.add_modifier.contains(Modifier::BOLD) })
        );
    }

    #[test]
    fn working_animation_reuses_the_finalized_markdown_layout() {
        let mut app = demo_app();
        app.awaiting_assistant = true;
        app.working_started_at = Some(Instant::now());
        let first = cached_transcript_layout(&app, 100, 2, TerminalAppearance::Dark);

        app.advance_animation();
        let next = cached_transcript_layout(&app, 100, 2, TerminalAppearance::Dark);

        assert!(Arc::ptr_eq(&first, &next));
    }

    #[test]
    fn local_working_status_renders_a_transcript_placeholder_before_session_events() {
        let mut app = demo_app();
        app.transcript.clear();
        app.status = "Working…".to_string();
        app.working_started_at = Instant::now().checked_sub(Duration::from_secs(5));
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
        assert!(render_app(&app, 80, 12).contains("Working (5s • esc to interrupt)"));
    }

    #[test]
    fn tool_execution_keeps_the_agent_working_indicator_active() {
        let mut app = demo_app();
        app.transcript.clear();
        app.apply_agent_event(AgentEvent::AgentStart);
        app.apply_agent_event(AgentEvent::ToolExecutionStart {
            tool_call_id: ToolCallId::new("call-web-search"),
            tool_name: "web_search".to_string(),
            args: serde_json::json!({ "query": "Beijing weather tomorrow" }),
        });

        assert!(
            app.has_active_animation(),
            "the working indicator should keep animating while a tool is running"
        );
        let screen = render_app(&app, 100, 12);
        assert!(
            screen.contains("web_search"),
            "tool status should remain visible:\n{screen}"
        );
        assert!(
            screen.contains("Working"),
            "agent status should remain visible:\n{screen}"
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
    fn legacy_skill_message_without_display_metadata_is_not_rewritten_by_the_tui() {
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
            vec![TranscriptItem::User(expanded.to_string())]
        );

        let mut restored = Vec::new();
        push_history_message(&mut restored, &pi_session::AgentMessage::from(message));
        assert_eq!(restored, vec![TranscriptItem::User(expanded.to_string())]);
    }

    #[test]
    fn extension_notice_is_visible_in_the_transcript() {
        let mut app = demo_app();
        app.transcript.clear();

        app.apply_session_event(AgentSessionEvent::PluginNotice {
            message: "/todos requires interactive mode".to_string(),
            level: pi_session::NoticeLevel::Error,
        });

        assert_eq!(
            app.transcript,
            vec![TranscriptItem::Notice(
                "/todos requires interactive mode".to_string()
            )]
        );
        assert_eq!(app.status, "/todos requires interactive mode");
    }

    #[test]
    fn transformed_user_message_displays_the_original_command_after_resume() {
        let message = pi_session::AgentMessage::with_display_text(
            Message::User(pi_core::UserMessage::text(
                "Run the private review prompt for accessibility",
                0,
            )),
            "/review accessibility",
        )
        .unwrap();
        let mut transcript = Vec::new();

        push_history_message(&mut transcript, &message);

        assert_eq!(
            transcript,
            vec![TranscriptItem::User("/review accessibility".to_string())]
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
        assert_eq!(dark.composer_background, Some(Color::Rgb(30, 30, 30)));
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
                let mut terminal = Terminal::with_options(
                    backend,
                    TerminalOptions {
                        viewport: Viewport::Fixed(Rect::new(0, 0, 100, 32)),
                    },
                )
                .unwrap();
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
    fn terminal_restore_attempts_every_cleanup_after_an_output_error() {
        struct FailingWriter;

        impl io::Write for FailingWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("terminal output failed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("terminal flush failed"))
            }
        }

        let raw_mode_disabled = std::cell::Cell::new(false);
        let result = restore_terminal_writer(&mut FailingWriter, true, || {
            raw_mode_disabled.set(true);
            Ok(())
        });

        assert!(result.is_err());
        assert!(raw_mode_disabled.get());
    }

    #[test]
    fn terminal_restore_leaves_fullscreen_and_shows_the_cursor() {
        let mut output = Vec::new();
        let raw_mode_disabled = std::cell::Cell::new(false);

        restore_terminal_writer(&mut output, true, || {
            raw_mode_disabled.set(true);
            Ok(())
        })
        .unwrap();

        assert!(raw_mode_disabled.get());
        assert!(contains_bytes(&output, b"\x1b[?1049l"));
        assert!(contains_bytes(&output, b"\x1b[?25h"));
    }

    #[test]
    fn mouse_wheel_scrolls_only_the_tui_transcript() {
        let mut app = demo_app();
        let surface = blank_surface(80, 24);
        assert_eq!(app.scroll_from_bottom, 0);

        handle_mouse(mouse_event(MouseEventKind::ScrollUp), &mut app, &surface);
        assert_eq!(app.scroll_from_bottom, MOUSE_SCROLL_LINES_PER_TICK);

        handle_mouse(mouse_event(MouseEventKind::Moved), &mut app, &surface);
        assert_eq!(app.scroll_from_bottom, MOUSE_SCROLL_LINES_PER_TICK);

        handle_mouse(mouse_event(MouseEventKind::ScrollDown), &mut app, &surface);
        assert_eq!(app.scroll_from_bottom, 0);
    }

    #[test]
    fn scroll_input_normalizes_raw_terminal_event_density() {
        let mut one_event = ScrollInputNormalizer::with_events_per_tick(1);
        assert_eq!(one_event.lines(ScrollDirection::Up), 3);

        let mut three_events = ScrollInputNormalizer::with_events_per_tick(3);
        assert_eq!(three_events.lines(ScrollDirection::Up), 1);
        assert_eq!(three_events.lines(ScrollDirection::Up), 1);
        assert_eq!(three_events.lines(ScrollDirection::Up), 1);

        let mut nine_events = ScrollInputNormalizer::with_events_per_tick(9);
        let lines = (0..9)
            .map(|_| nine_events.lines(ScrollDirection::Up))
            .sum::<usize>();
        assert_eq!(lines, 3);
        assert_eq!(nine_events.lines(ScrollDirection::Down), 0);
    }

    #[test]
    fn transcript_page_navigation_tracks_the_rendered_viewport_height() {
        let mut app = demo_app();
        app.scroll_from_bottom = 5;

        assert!(handle_transcript_navigation(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &mut app,
            10,
        ));
        assert_eq!(app.scroll_from_bottom, 11);

        assert!(handle_transcript_navigation(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &mut app,
            10,
        ));
        assert_eq!(app.scroll_from_bottom, 5);

        assert!(handle_transcript_navigation(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &mut app,
            3,
        ));
        assert_eq!(app.scroll_from_bottom, 6);

        assert!(handle_transcript_navigation(
            KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
            &mut app,
            10,
        ));
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
    fn mouse_drag_release_requests_copy_of_visible_assistant_text() {
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
        let surface = ScreenTextSurface::capture(buffer);
        let mut clipboard = RecordingClipboard::default();

        handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            &surface,
            &mut clipboard,
        );
        handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: x + 3,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            &surface,
            &mut clipboard,
        );
        handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: x + 3,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            &surface,
            &mut clipboard,
        );

        let selection = app.screen_selection.expect("finished selection");
        assert_eq!(surface.selected_text(selection).as_deref(), Some("copy"));
        assert_eq!(clipboard.text.as_deref(), Some("copy"));
        assert_eq!(app.status, "Copied 4 characters");
        let screen = render_app(&app, 140, 12);
        assert!(!screen.contains("Ctrl+Shift+C copy"));
        assert!(!screen.contains("drag to copy"));
    }

    #[test]
    fn mouse_drag_release_reports_clipboard_failures() {
        let mut app = demo_app();
        let area = Rect::new(0, 0, 8, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        buffer.set_string(0, 0, "copy me", Style::default());
        let surface = ScreenTextSurface::capture(&buffer);
        let mut clipboard = FailingClipboard;

        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), 0),
            (MouseEventKind::Drag(MouseButton::Left), 3),
            (MouseEventKind::Up(MouseButton::Left), 3),
        ] {
            handle_mouse_event(
                MouseEvent {
                    kind,
                    column,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
                &mut app,
                &surface,
                &mut clipboard,
            );
        }

        assert_eq!(app.status, "Copy failed: clipboard denied");
    }

    #[test]
    fn dragged_screen_selection_is_visibly_highlighted() {
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
        let surface = ScreenTextSurface::capture(buffer);
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
        let surface = ScreenTextSurface::capture(&buffer);
        app.screen_selection = Some(ScreenSelection::new(
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
    fn screen_selection_copies_composer_text_outside_the_transcript() {
        let mut app = demo_app();
        app.input.set_text("draft to copy");
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_buffer_position(buffer, 80, 16, "draft to copy").expect("composer text");
        let surface = ScreenTextSurface::capture(buffer);

        drag_screen_selection(&mut app, &surface, (x, y), (x + 4, y));

        let selection = app.screen_selection.expect("composer selection");
        assert_eq!(surface.selected_text(selection).as_deref(), Some("draft"));
    }

    #[test]
    fn screen_selection_highlight_is_painted_after_the_footer() {
        let mut app = demo_app();
        app.status = "footer copy target".to_string();
        let backend = TestBackend::new(140, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) =
            find_buffer_position(buffer, 140, 16, "footer copy target").expect("footer status");
        let surface = ScreenTextSurface::capture(buffer);
        drag_screen_selection(&mut app, &surface, (x, y), (x + 5, y));

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        for selected_x in x..=x + 5 {
            assert_eq!(
                terminal.backend().buffer()[(selected_x, y)].bg,
                Color::Rgb(10, 78, 152)
            );
        }
    }

    #[test]
    fn screen_selection_copies_bottom_view_text() {
        let mut app = demo_app();
        app.view_stack.push(BottomPaneView::Model);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_buffer_position(buffer, 100, 24, "Select Model and Effort")
            .expect("bottom view title");
        let surface = ScreenTextSurface::capture(buffer);

        drag_screen_selection(&mut app, &surface, (x, y), (x + 5, y));

        let selection = app.screen_selection.expect("bottom view selection");
        assert_eq!(surface.selected_text(selection).as_deref(), Some("Select"));
    }

    #[test]
    fn screen_selection_and_copy_work_on_the_trust_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join(".pi/skills")).unwrap();
        let (service, _) = ProjectTrustService::new(
            &directory.path().join("agent"),
            None,
            true,
            pi_settings::DefaultProjectTrust::Ask,
        )
        .unwrap();
        let mut app = demo_app();
        app.trust_prompt = Some(TrustPromptState {
            cwd: project.clone(),
            options: service.manual_options(&project).unwrap(),
            selected: 0,
            response: None,
        });
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(0, 0, 0)));
        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) =
            find_buffer_position(buffer, 100, 24, "Trust project folder?").expect("trust title");
        let surface = ScreenTextSurface::capture(buffer);
        drag_screen_selection(&mut app, &surface, (x, y), (x + 4, y));
        let mut clipboard = RecordingClipboard::default();

        assert!(handle_copy_shortcut(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER),
            &mut app,
            &surface,
            &mut clipboard,
        ));
        assert_eq!(clipboard.text.as_deref(), Some("Trust"));
        assert!(app.trust_prompt.is_some());
    }

    #[test]
    fn active_screen_selection_suppresses_animation_ticks() {
        let mut app = demo_app();
        app.awaiting_assistant = true;
        assert!(app.has_active_animation());
        app.screen_selection = Some(ScreenSelection::new(
            Position::new(0, 0),
            Position::new(1, 0),
        ));

        assert!(!app.has_active_animation());
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
        assert_eq!(app.command_palette.borrow().selected(), Some(1));
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
    fn ctrl_c_clears_a_draft_interrupts_work_and_only_quits_when_idle() {
        let mut app = demo_app();
        assert_eq!(ctrl_c_action(&app), CtrlCAction::ClearComposer);

        app.input.clear();
        app.is_running = true;
        assert_eq!(ctrl_c_action(&app), CtrlCAction::Interrupt);

        app.is_running = false;
        assert_eq!(ctrl_c_action(&app), CtrlCAction::Quit);
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
            assert_eq!(buffer[(0, y)].bg, Color::Rgb(30, 30, 30));
            assert_eq!(buffer[(79, y)].bg, Color::Rgb(30, 30, 30));
        }
        assert_eq!(
            buffer[(0, composer_y.saturating_sub(1))].bg,
            Color::Rgb(30, 30, 30)
        );
        assert_eq!(
            buffer[(79, composer_y.saturating_add(1))].bg,
            Color::Rgb(30, 30, 30)
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
            assert_eq!(buffer[(x, code_y)].bg, Color::Rgb(242, 242, 244));
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
        app.show_startup_header = true;
        app.registered_plugins = RegisteredPluginInventory {
            js_extensions: vec!["clipboard.ts".to_string(), "session-tools.ts".to_string()],
            rust_plugins: vec!["frontend-check".to_string(), "release-audit".to_string()],
        };
        app.input.clear();
        app.queue = QueueSnapshot::default();

        let screen = render_app(&app, 80, 20);
        if std::env::var_os("PI_PRINT_TUI_TEST").is_some() {
            println!("{screen}");
        }

        assert!(screen.contains(&format!(">_ pi (v{})", env!("CARGO_PKG_VERSION"))));
        assert!(screen.contains("model:"));
        assert!(screen.contains("directory:"));
        assert!(screen.contains("gpt-5.6-sol high  /model to change"));
        assert!(screen.contains("js extensions:  clipboard.ts, session-tools.ts"));
        assert!(screen.contains("rust plugins:   frontend-check, release-audit"));
        assert!(screen.contains("Ask pi to do anything"));
        assert!(screen.contains("Tip: type / for commands or ! for shell"));
        let card_top = screen.lines().find(|line| line.contains('╭')).unwrap();
        assert_eq!(UnicodeWidthStr::width(card_top.trim_start()), 54);
    }

    #[test]
    fn registered_plugin_inventory_uses_resolved_js_identities_and_configured_native_plugins() {
        let runtime_inventory = SessionRuntimeInventory::new(
            [
                "npm:@counterposition/pi-web-search".to_string(),
                "npm:@narumitw/pi-lsp@0.49.5".to_string(),
                "clipboard.ts".to_string(),
            ],
            [pi_core::PluginId::new("frontend-check")],
        );
        let inventory = RegisteredPluginInventory::from_runtime(&runtime_inventory);

        assert_eq!(
            inventory.js_extensions,
            [
                "npm:@counterposition/pi-web-search",
                "npm:@narumitw/pi-lsp@0.49.5",
                "clipboard.ts",
            ]
        );
        assert_eq!(inventory.rust_plugins, ["frontend-check"]);
    }

    #[test]
    fn startup_card_wraps_registration_lists_without_clipping_entries() {
        let mut app = demo_app();
        app.transcript.clear();
        app.show_startup_header = true;
        app.input.clear();
        app.queue = QueueSnapshot::default();
        app.registered_plugins = RegisteredPluginInventory {
            js_extensions: vec![
                "clipboard.ts".to_string(),
                "session-tools".to_string(),
                "review-workflow.ts".to_string(),
            ],
            rust_plugins: vec![
                "frontend-check".to_string(),
                "repository-policy".to_string(),
                "release-audit".to_string(),
                "last-native-plugin".to_string(),
            ],
        };

        let screen = render_app(&app, 60, 28);

        for registration in app
            .registered_plugins
            .js_extensions
            .iter()
            .chain(&app.registered_plugins.rust_plugins)
        {
            assert!(
                screen.contains(registration),
                "missing {registration}\n{screen}"
            );
        }
        assert!(screen.contains("Tip: type / for commands or ! for shell"));
        assert!(screen.contains("Ask pi to do anything"));
    }

    #[test]
    fn startup_header_remains_visible_after_the_first_exchange() {
        let mut app = demo_app();
        app.show_startup_header = true;
        app.transcript = vec![
            TranscriptItem::User("hi".to_string()),
            TranscriptItem::Assistant {
                text: "Hi! How can I help?".to_string(),
                streaming: false,
                error: None,
            },
        ];
        app.input.clear();
        app.queue = QueueSnapshot::default();

        let screen = render_app(&app, 80, 30);

        assert!(screen.contains(&format!(">_ pi (v{})", env!("CARGO_PKG_VERSION"))));
        assert!(screen.contains("Tip: type / for commands or ! for shell"));
        assert!(screen.contains("hi"));
        assert!(screen.contains("Hi! How can I help?"));
    }

    #[test]
    fn composer_follows_short_transcript_instead_of_sticking_to_the_bottom() {
        let mut app = demo_app();
        app.transcript = vec![TranscriptItem::Assistant {
            text: "Short answer".to_string(),
            streaming: false,
            error: None,
        }];
        app.input.clear();
        app.queue = QueueSnapshot::default();
        let root = Rect::new(0, 0, 80, 30);

        let areas = ui_areas(root, &app);

        assert_eq!(areas.composer.y, areas.transcript.bottom());
        assert!(areas.footer.bottom() < root.bottom());
    }

    #[test]
    fn scroll_redraw_stays_within_the_30_fps_frame_budget() {
        let mut app = demo_app();
        app.show_startup_header = true;
        app.input.clear();
        app.queue = QueueSnapshot::default();
        app.transcript = (0..240)
            .map(|index| TranscriptItem::Assistant {
                text: format!(
                    "## Response {index}\n\n- first item with `inline code`\n- second item\n\n```rust\nfn response_{index}() {{}}\n```"
                ),
                streaming: false,
                error: None,
            })
            .collect();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));
        let completed = terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
        std::hint::black_box(ScreenTextSurface::capture(completed.buffer));

        let started = std::time::Instant::now();
        for frame_index in 0..30 {
            app.scroll_from_bottom = frame_index * MOUSE_SCROLL_LINES_PER_TICK;
            let completed = terminal.draw(|frame| draw(frame, &app, palette)).unwrap();
            std::hint::black_box(ScreenTextSurface::capture(completed.buffer));
        }
        let average_frame = started.elapsed() / 30;

        assert!(
            average_frame < Duration::from_millis(33),
            "scroll redraw averaged {average_frame:?}, exceeding the 30 fps budget"
        );
    }

    #[test]
    fn narrow_layout_stays_renderable() {
        let mut app = demo_app();
        app.input.set_text("/");
        let screen = render_app(&app, 38, 12);
        assert!(screen.contains("/new"));
        assert!(!screen.contains("↑↓ select"));
    }

    #[test]
    fn completion_panel_sits_directly_below_the_composer() {
        let mut app = demo_app();
        app.input.set_text("/skill:");

        let areas = ui_areas(Rect::new(0, 0, 80, 20), &app);

        assert!(!areas.context.is_empty());
        assert_eq!(areas.context.y, areas.composer.bottom());
        assert_eq!(areas.footer.y, areas.context.bottom());
    }

    #[test]
    fn completion_labels_align_with_the_composer_text_column() {
        let mut app = demo_app();
        app.input.set_text("/");
        let areas = ui_areas(Rect::new(0, 0, 80, 20), &app);
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        let buffer = terminal.backend().buffer();
        let composer_text_y = areas.composer.y.saturating_add(1);
        let composer_slash_x = (areas.composer.x..areas.composer.right())
            .find(|x| buffer[(*x, composer_text_y)].symbol() == "/")
            .expect("composer slash");
        let (completion_slash_x, _) =
            find_buffer_position(buffer, 80, 20, "/new").expect("completion slash");
        assert_eq!(completion_slash_x, composer_slash_x);
    }

    #[test]
    fn dismissed_completion_restores_the_passive_footer_until_input_changes() {
        let mut app = demo_app();
        app.input.set_text("/");
        app.queue = QueueSnapshot::default();
        app.dismissed_completion = Some("/".to_string());

        let dismissed = ui_areas(Rect::new(0, 0, 80, 20), &app);
        assert!(dismissed.context.is_empty());
        assert!(!dismissed.footer.is_empty());
        assert!(suggestions_for_app(&app).is_empty());

        app.input.set_text("/m");
        let reopened = ui_areas(Rect::new(0, 0, 80, 20), &app);
        assert!(!reopened.context.is_empty());
        assert!(reopened.footer.is_empty());
    }

    #[test]
    fn bottom_pane_view_replaces_the_composer_without_losing_its_draft() {
        let mut app = demo_app();
        app.input.set_text("draft that must survive");
        app.model_specs = vec![ModelSpec::new("custom", "alpha", "Alpha", "test")];
        app.push_bottom_view(BottomPaneView::Model);

        let areas = ui_areas(Rect::new(0, 0, 80, 20), &app);
        let screen = render_app(&app, 80, 20);

        assert!(areas.context.is_empty());
        assert!(areas.footer.is_empty());
        assert!(screen.contains("Select Model and Effort"));
        assert!(!screen.contains("draft that must survive"));
        app.pop_bottom_view();
        assert_eq!(app.input.text(), "draft that must survive");
    }

    #[test]
    fn login_and_logout_commands_open_provider_specific_pickers() {
        let mut app = demo_app();
        app.login_providers = vec![
            AuthProviderInfo {
                id: "anthropic".to_string(),
                supports_oauth: true,
                stored_kind: Some("oauth"),
            },
            AuthProviderInfo {
                id: "private-gateway".to_string(),
                supports_oauth: false,
                stored_kind: None,
            },
        ];
        app.logout_providers = vec![app.login_providers[0].clone()];

        assert!(activate_bottom_view_for_command(&mut app, "/login"));
        assert_eq!(app.active_bottom_view(), Some(BottomPaneView::Login));
        let login = render_app(&app, 90, 20);
        assert!(login.contains("Select provider to configure"));
        assert!(login.contains("anthropic"));
        assert!(login.contains("OAuth or API key · oauth configured"));
        assert!(login.contains("private-gateway"));
        assert!(login.contains("API key · unconfigured"));

        app.pop_bottom_view();
        assert!(activate_bottom_view_for_command(&mut app, "/logout"));
        assert_eq!(app.active_bottom_view(), Some(BottomPaneView::Logout));
        let logout = render_app(&app, 90, 20);
        assert!(logout.contains("Select provider to log out"));
        assert!(logout.contains("stored oauth"));
    }

    #[test]
    fn auth_commands_only_queue_known_or_stored_providers() {
        let mut app = demo_app();
        app.login_providers = vec![AuthProviderInfo {
            id: "openai-codex".to_string(),
            supports_oauth: true,
            stored_kind: None,
        }];
        app.logout_providers = vec![AuthProviderInfo {
            id: "anthropic".to_string(),
            supports_oauth: true,
            stored_kind: Some("oauth"),
        }];

        assert_eq!(
            auth_request_for_input(&app, "/login OPENAI-CODEX").unwrap(),
            Some(AuthRequest {
                operation: AuthOperation::Login,
                provider: "openai-codex".to_string(),
            })
        );
        assert_eq!(
            auth_request_for_input(&app, "/logout anthropic").unwrap(),
            Some(AuthRequest {
                operation: AuthOperation::Logout,
                provider: "anthropic".to_string(),
            })
        );
        assert_eq!(
            auth_request_for_input(&app, "/logout openai-codex").unwrap_err(),
            "No stored credential for openai-codex"
        );
        assert_eq!(auth_request_for_input(&app, "/login").unwrap(), None);
    }

    #[test]
    fn auth_commands_are_discoverable_from_completion_and_help() {
        let suggestions = command_suggestions("/log", &[]);
        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.invocation.as_str())
                .collect::<Vec<_>>(),
            vec!["/login", "/logout"]
        );
        let help = builtin_help_text(&[]);
        assert!(help.contains("`/login [provider]`"));
        assert!(help.contains("`/logout`"));
    }

    #[test]
    fn selected_completion_row_uses_the_codex_accent_hierarchy() {
        let mut app = demo_app();
        app.input.set_text("/");
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let palette = UiPalette::from_background(Some(RgbColor::new(255, 255, 255)));

        terminal.draw(|frame| draw(frame, &app, palette)).unwrap();

        let buffer = terminal.backend().buffer();
        let (selected_x, selected_y) =
            find_buffer_position(buffer, 80, 20, "/new").expect("selected command");
        let (description_x, description_y) =
            find_buffer_position(buffer, 80, 20, "new session").expect("selected description");
        let (plain_x, plain_y) =
            find_buffer_position(buffer, 80, 20, "/resume").expect("plain command");

        assert_eq!(buffer[(selected_x, selected_y)].fg, Color::Rgb(0, 95, 135));
        assert_eq!(
            buffer[(description_x, description_y)].fg,
            Color::Rgb(0, 95, 135)
        );
        assert!(
            buffer[(selected_x, selected_y)]
                .modifier
                .contains(Modifier::BOLD)
        );
        assert_ne!(buffer[(plain_x, plain_y)].fg, Color::DarkGray);
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
        assert!(screen.contains("/skill:code-review"));
        assert!(screen.contains("Review changes against repository standards"));
        assert!(!screen.contains("↑↓ select"));
        complete_selected_command(&mut app);
        assert_eq!(app.input.text(), "/skill:code-review ");
    }

    #[test]
    fn command_selection_moves_and_wraps() {
        let mut app = demo_app();
        app.input.set_text("/");
        let suggestion_count = command_suggestions(&app.input.text(), &app.command_specs).len();

        move_command_selection(&mut app, -1);
        assert_eq!(
            app.command_palette.borrow().selected(),
            Some(suggestion_count - 1)
        );

        move_command_selection(&mut app, 1);
        assert_eq!(app.command_palette.borrow().selected(), Some(0));

        move_command_selection(&mut app, 1);
        assert_eq!(app.command_palette.borrow().selected(), Some(1));
    }

    #[test]
    fn completion_uses_the_selected_command() {
        let mut app = demo_app();
        app.input.set_text("/");
        let suggestions = suggestions_for_app(&app);
        let reload = suggestions
            .iter()
            .position(|suggestion| suggestion.invocation == "/reload")
            .unwrap();
        app.command_palette
            .get_mut()
            .reconcile_len(suggestions.len());
        app.command_palette.get_mut().select(reload);

        assert!(complete_selected_command(&mut app));
        assert_eq!(app.input.text(), "/reload");
        assert_eq!(app.command_palette.borrow().selected(), Some(0));
    }

    #[test]
    fn command_panel_scrolls_to_keep_the_selection_visible() {
        let mut app = demo_app();
        app.command_specs = (0..12)
            .map(|index| CommandSpec {
                name: format!("custom:command-{index}"),
                description: format!("Custom command number {index}"),
                argument_hint: None,
            })
            .collect();
        app.input.set_text("/custom:");
        app.command_palette.get_mut().reconcile_len(12);
        app.command_palette.get_mut().select(8);

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("/custom:command-8"));
        assert!(!screen.contains("/custom:command-0"));
        assert!(screen.contains("/custom:command-11"));
        assert!(!screen.contains("↑↓ select"));
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

    #[test]
    fn live_usage_counts_entries_and_detached_adjustments_once() {
        let mut app = demo_app();
        app.session_tokens = 0;
        let mut tool_usage = billed_usage(20, 5, 6, 7);
        tool_usage.total_tokens = 0;
        app.apply_session_event(AgentSessionEvent::EntryAppended {
            entry: SessionRecord {
                id: "tool-entry".to_string(),
                seq: 1,
                parent_id: None,
                timestamp_ms: 2,
                entry: SessionEntry::message(Message::tool_result(pi_core::ToolResultMessage {
                    tool_call_id: ToolCallId::new("metered-call"),
                    tool_name: "metered".to_string(),
                    content: vec![ContentBlock::Text(pi_core::TextContent::new("result"))],
                    details: None,
                    usage: Some(tool_usage),
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: 2,
                })),
            },
        });
        let compaction = CompactionEntry {
            summary: "summary".to_string(),
            retained_tail: Vec::new(),
            tokens_before: 1_000,
            details: None,
            usage: Some(billed_usage(30, 8, 9, 10)),
        };
        app.apply_session_event(AgentSessionEvent::EntryAppended {
            entry: SessionRecord {
                id: "compaction-entry".to_string(),
                seq: 2,
                parent_id: Some("tool-entry".to_string()),
                timestamp_ms: 3,
                entry: SessionEntry::Compaction(compaction.clone()),
            },
        });
        app.apply_session_event(AgentSessionEvent::CompactionEnd {
            reason: pi_session::CompactionReason::Manual,
            result: Some(SessionRecord {
                id: "compaction-entry".to_string(),
                seq: 2,
                parent_id: Some("tool-entry".to_string()),
                timestamp_ms: 3,
                entry: SessionEntry::Compaction(compaction),
            }),
            aborted: false,
            will_retry: false,
            error_message: None,
        });
        app.apply_session_event(AgentSessionEvent::UsageRecorded {
            usage: billed_usage(2, 3, 4, 5),
        });

        assert_eq!(app.session_tokens, 109);
        assert_eq!(app.context_tokens, None);
        assert!(usage_footer(&app).contains("context ?"));
    }

    #[tokio::test]
    async fn resumed_app_restores_all_entry_usage_and_unknown_compacted_context() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, SessionHeader::new("usage-resume", directory.path()))
            .unwrap();
        let assistant = Message::assistant(pi_core::AssistantMessage {
            content: vec![ContentBlock::Text(pi_core::TextContent::new("answer"))],
            api: "openai-completions".to_string(),
            provider: ProviderId::new("openai-compatible"),
            model: ModelId::new("default"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: billed_usage(10, 2, 3, 4),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 2,
        });
        log.append_message(Message::User(pi_core::UserMessage::text("hello", 1)))
            .unwrap();
        log.append_message(assistant.clone()).unwrap();
        log.append_message(Message::tool_result(pi_core::ToolResultMessage {
            tool_call_id: ToolCallId::new("metered-call"),
            tool_name: "metered".to_string(),
            content: vec![ContentBlock::Text(pi_core::TextContent::new("result"))],
            details: None,
            usage: Some(billed_usage(20, 5, 6, 7)),
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 3,
        }))
        .unwrap();
        log.append(SessionEntry::Compaction(CompactionEntry {
            summary: "summary".to_string(),
            retained_tail: vec![assistant.clone().into()],
            tokens_before: 10_000,
            details: None,
            usage: Some(billed_usage(30, 8, 9, 10)),
        }))
        .unwrap();
        drop(log);

        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(
                pi_plugin_openai::OpenAiCompatiblePlugin::new(
                    pi_plugin_openai::OpenAiCompatibleConfig::without_api_key(
                        "https://example.invalid/v1",
                    ),
                )
                .unwrap(),
            )
            .agent_options(pi_agent::AgentOptions {
                provider_id: ProviderId::new("openai-compatible"),
                cwd: directory.path().to_path_buf(),
                ..pi_agent::AgentOptions::default()
            })
            .system_prompt(pi_runtime::SystemPrompt::Final("test".to_string()))
            .build()
            .unwrap();
        let session = AgentSession::open(runtime, &path).await.unwrap();

        let mut app = App::new(&session, &session.snapshot());

        assert_eq!(app.session_tokens, 114);
        assert_eq!(app.context_tokens, None);
        let mut stale_snapshot = session.snapshot();
        stale_snapshot.agent.messages = vec![assistant];
        app.sync_snapshot(&stale_snapshot);
        assert_eq!(app.context_tokens, None);
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
                  "apiKey": "test-key",
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
        assert!(activate_bottom_view_for_command(&mut app, "/model"));

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("Select Model and Effort"));
        assert!(screen.contains("custom/alpha (current)"));
        assert!(screen.contains("Alpha Registered"));
        assert!(screen.contains("custom/beta"));
        assert!(screen.contains("Beta Registered"));
        assert!(screen.contains("Press enter to confirm or esc to go back"));
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
                cwd: PathBuf::from("/workspace/project"),
                name: Some("Resume polish".to_string()),
                first_message: "Improve the session picker".to_string(),
                message_count: 12,
                modified_at_ms: unix_time_ms(),
                current: false,
            },
            SessionChoice {
                path: PathBuf::from("/tmp/sessions/older.jsonl"),
                id: "older".to_string(),
                cwd: PathBuf::from("/workspace/project"),
                name: None,
                first_message: "Older conversation".to_string(),
                message_count: 4,
                modified_at_ms: 0,
                current: true,
            },
        ];
        app.input.set_text("/resume");
        assert!(activate_bottom_view_for_command(&mut app, "/resume"));

        let screen = render_app(&app, 100, 24);

        assert!(screen.contains("Resume polish"));
        assert!(screen.contains("resume-polish · now"));
        assert!(screen.contains("Older conversation (current)"));
        assert!(screen.contains("older ·"));
        assert!(!screen.contains("msgs"));
        assert!(screen.contains("Press enter to confirm or esc to go back"));

        app.pop_bottom_view();
        app.input.set_text("/resume polish");
        let suggestions = suggestions_for_app(&app);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].label.as_deref(), Some("Resume polish"));
        assert!(
            suggestions[0]
                .description
                .starts_with("resume-polish · now · ")
        );
        assert!(!suggestions[0].description.contains("msgs"));
        assert!(!complete_selected_command_for_enter(&mut app));
        assert_eq!(
            app.input.text(),
            "/resume /tmp/sessions/resume polish.jsonl"
        );
    }

    #[test]
    fn thinking_command_opens_an_option_picker_for_the_current_model() {
        let mut app = demo_app();
        let mut model = ModelSpec::new("openai-compatible", "gpt-5.6-sol", "GPT", "test");
        model.reasoning = true;
        model
            .thinking_level_map
            .insert("max".to_string(), Some("max".to_string()));
        app.model_specs = vec![model];
        app.input.set_text("/thinking");

        assert!(activate_bottom_view_for_command(&mut app, "/thinking"));
        assert_eq!(app.active_bottom_view(), Some(BottomPaneView::Thinking));
        assert_eq!(app.view_selection.borrow().selected(), Some(4));

        let screen = render_app(&app, 80, 20);
        assert!(screen.contains("Select Thinking Level"));
        assert!(screen.contains("No reasoning"));
        assert!(screen.contains("Deep reasoning (~16k tokens)"));
        assert!(screen.contains("Maximum reasoning"));
        assert!(!screen.contains("Extra-high reasoning"));
    }

    #[test]
    fn non_reasoning_model_only_offers_thinking_off() {
        let mut app = demo_app();
        app.model_specs = vec![ModelSpec::new(
            "openai-compatible",
            "gpt-5.6-sol",
            "GPT",
            "test",
        )];

        let choices = app.thinking_choices();

        assert_eq!(choices, vec![THINKING_CHOICES[0]]);
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
        let start = truncate_start("/workspace/project", 12);
        assert!(UnicodeWidthStr::width(end.as_str()) <= 7);
        assert!(UnicodeWidthStr::width(start.as_str()) <= 12);
        assert!(end.ends_with('…'));
        assert!(start.starts_with('…'));
    }
}
