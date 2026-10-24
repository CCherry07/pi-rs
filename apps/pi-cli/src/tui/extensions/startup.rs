//! Temporary frontend while session-start hooks are still activating the session.

use crossterm::event::Event;
use pi_plugin::{UiCommand, UiProductCommand, UiSurfaceSnapshot};
use ratatui::layout::Rect;

use super::super::input::InputStream;
use super::super::{SystemClipboard, TerminalSession};
use super::bridge::{SurfaceReceiver, SurfaceRequest};
use super::host;
use super::surface::ExtensionSurface;

pub(in crate::tui) struct StartupUi {
    pub(in crate::tui) surface: ExtensionSurface,
    pub(in crate::tui) input: InputStream,
}

fn snapshot(receiver: &SurfaceReceiver) -> UiSurfaceSnapshot {
    receiver
        .snapshot
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn store_snapshot(receiver: &SurfaceReceiver, snapshot: UiSurfaceSnapshot) {
    *receiver
        .snapshot
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
}

async fn apply_effect(
    effect: super::surface::Effect,
    receiver: &mut SurfaceReceiver,
    surface: &mut ExtensionSurface,
    terminal: &mut TerminalSession,
    clipboard: &mut SystemClipboard,
    snapshot: &UiSurfaceSnapshot,
) -> Result<(), String> {
    use super::surface::Effect;
    match effect {
        Effect::ClearOwner { owner } => terminal.clear_owner(&owner)?,
        Effect::HostOperation {
            owner,
            id,
            operation,
        } => {
            let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
            let result = host::execute(
                operation,
                terminal,
                clipboard,
                &cwd,
                receiver.external_editor.as_deref(),
            )
            .await;
            surface.finish_host_operation(&owner, id, result, snapshot);
        }
        Effect::Product { owner, command } => match command {
            UiProductCommand::EditorText { text, append } => {
                let mut shared = receiver
                    .snapshot
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if append {
                    shared.editor_text.push_str(&text);
                } else {
                    shared.editor_text = text;
                }
                surface.refresh(&shared);
            }
            UiProductCommand::ToolsExpanded { expanded } => {
                receiver
                    .snapshot
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .tools_expanded = expanded;
            }
            UiProductCommand::Title { title } => host::set_title(&title)?,
            command => {
                match &command {
                    UiProductCommand::Theme { name } => {
                        if let Some(palette) = host::theme(name) {
                            surface.set_palette(palette);
                        }
                    }
                    UiProductCommand::Progress { active } => {
                        terminal.set_progress(&owner, *active)?
                    }
                    UiProductCommand::ProgramStatus {
                        state,
                        text,
                        progress,
                    } => {
                        terminal.set_program_status(
                            &owner,
                            state.clone(),
                            text.clone(),
                            *progress,
                        )?;
                    }
                    _ => {}
                }
                // Re-enter owner admission when the active frontend takes over: an owner may
                // retire after issuing a startup command and before session activation.
                receiver.defer(SurfaceRequest::Command(owner, UiCommand::Product(command)));
            }
        },
    }
    Ok(())
}

fn handle_input(
    event: Option<Result<super::super::input::InputPacket, std::io::Error>>,
    receiver: &SurfaceReceiver,
    surface: &mut ExtensionSurface,
) -> Result<(), String> {
    let Some(event) = event else {
        return Err("terminal closed during startup".into());
    };
    let packet = event.map_err(|error| error.to_string())?;
    let mut current = snapshot(receiver);
    if let Some(Event::Resize(columns, rows)) = packet.event {
        current.columns = columns;
        current.rows = rows;
        store_snapshot(receiver, current.clone());
        surface.refresh(&current);
    } else {
        surface.input_packet(packet.event, packet.data.as_deref(), &current);
    }
    Ok(())
}

fn render(
    receiver: &SurfaceReceiver,
    surface: &mut ExtensionSurface,
    terminal: &mut TerminalSession,
) -> Result<(), String> {
    let size = terminal
        .terminal_mut()
        .size()
        .map_err(|error| error.to_string())?;
    let mut current = snapshot(receiver);
    let areas = surface.startup_areas(size.width, size.height);
    if current.areas != areas || current.columns != size.width || current.rows != size.height {
        current.areas = areas;
        current.columns = size.width;
        current.rows = size.height;
        store_snapshot(receiver, current.clone());
        surface.refresh(&current);
    }

    surface.begin_frame();
    let completed = terminal
        .terminal_mut()
        .draw(|frame| {
            for area in &current.areas {
                surface.draw_regions(
                    frame,
                    area.placement,
                    Rect::new(area.column, area.row, area.width, area.height),
                );
            }
            surface.draw_overlays(frame);
        })
        .map_err(|error| error.to_string())?;
    let paint = surface.prepare_frame(completed.buffer);
    paint
        .emit(terminal.terminal_mut())
        .map_err(|error| error.to_string())
}

/// Session-start hooks can await UI before a PiSession exists. Keep the same
/// component channel live during activation, then hand its state to the full frontend.
pub(crate) async fn during_startup<T>(
    startup: impl std::future::Future<Output = Result<T, String>>,
    receiver: &mut SurfaceReceiver,
    fullscreen: bool,
) -> Result<T, String> {
    tokio::pin!(startup);
    let mut terminal: Option<TerminalSession> = None;
    let mut input = InputStream::new();
    let mut surface = ExtensionSurface::default();
    surface.set_palette(
        receiver
            .theme
            .as_deref()
            .and_then(host::theme)
            .unwrap_or_else(|| {
                super::super::UiPalette::from_background(super::super::background_from_colorfgbg())
            }),
    );
    surface.set_images_enabled(receiver.show_images);
    let mut clipboard = SystemClipboard::default();

    let result = loop {
        tokio::select! {
            result = &mut startup => break result,
            request = receiver.recv_live() => {
                let Some(request) = request else {
                    break Err("extension UI closed during startup".into());
                };
                if terminal.is_none() {
                    terminal = Some(TerminalSession::new(fullscreen).map_err(|error| error.to_string())?);
                }
                let frontend = terminal.as_mut().expect("startup terminal initialized above");
                let size = frontend.terminal_mut().size().map_err(|error| error.to_string())?;
                let current = UiSurfaceSnapshot {
                    columns: size.width,
                    rows: size.height,
                    ..snapshot(receiver)
                };
                store_snapshot(receiver, current.clone());
                if let Some(effect) = surface.handle_request(request, &current) {
                    apply_effect(
                        effect,
                        receiver,
                        &mut surface,
                        frontend,
                        &mut clipboard,
                        &current,
                    )
                    .await?;
                }
            }
            event = input.next(), if terminal.is_some() && surface.can_accept_input() => {
                handle_input(event, receiver, &mut surface)?;
            }
        }
        if let Some(frontend) = terminal.as_mut() {
            render(receiver, &mut surface, frontend)?;
        }
    };

    if result.is_ok() {
        receiver.startup = Some(StartupUi { surface, input });
    } else {
        surface.close();
    }
    if let Some(terminal) = terminal {
        terminal.finish().map_err(|error| error.to_string())?;
    }
    result
}
