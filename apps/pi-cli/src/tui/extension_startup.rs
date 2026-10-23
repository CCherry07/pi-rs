//! Temporary extension frontend used while session-start hooks are still activating the session.

use crossterm::event::Event;
use pi_plugin::{UiSurfaceAction, UiSurfaceSnapshot};
use ratatui::layout::Rect;

use super::extension_surface::ExtensionSurface;
use super::input::InputStream;
use super::{SystemClipboard, TerminalSession, extension_operations};
use crate::plugin_ui::{SurfaceReceiver, SurfaceRequest};

pub(crate) struct StartupUi {
    pub(super) surface: ExtensionSurface,
    pub(super) input: InputStream,
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

async fn handle_request(
    request: SurfaceRequest,
    receiver: &mut SurfaceReceiver,
    surface: &mut ExtensionSurface,
    terminal: &mut TerminalSession,
    clipboard: &mut SystemClipboard,
    snapshot: &UiSurfaceSnapshot,
) -> Result<(), String> {
    match request {
        SurfaceRequest::Exchange(frame, response) => surface.exchange(frame, response, snapshot),
        SurfaceRequest::Action(owner, UiSurfaceAction::Close) => {
            terminal.clear_owner(&owner)?;
            surface.retire(owner);
            surface.refresh(snapshot);
        }
        SurfaceRequest::Action(owner, action) if surface.accepts(&owner) => match action {
            UiSurfaceAction::HostOperation { id, operation } => {
                let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
                let result = extension_operations::execute(
                    operation,
                    terminal,
                    clipboard,
                    &cwd,
                    receiver.external_editor.as_deref(),
                )
                .await;
                surface.graphics.borrow_mut().reset();
                surface.host_result(&owner, id, result, snapshot);
            }
            UiSurfaceAction::Wake => surface.wake(&owner, snapshot),
            UiSurfaceAction::EditorText { text, append } => {
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
            UiSurfaceAction::ToolsExpanded { expanded } => {
                receiver
                    .snapshot
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .tools_expanded = expanded;
            }
            UiSurfaceAction::Title { title } => extension_operations::set_title(&title)?,
            UiSurfaceAction::Focus { region_id, node_id } => {
                surface.focus(&owner, &region_id, &node_id);
            }
            UiSurfaceAction::Theme { name } => {
                if let Some(palette) = extension_operations::theme(&name) {
                    surface.set_palette(palette);
                }
                receiver.initial.push_back(SurfaceRequest::Action(
                    owner,
                    UiSurfaceAction::Theme { name },
                ));
            }
            UiSurfaceAction::Progress { active } => {
                terminal.set_progress(&owner, active)?;
                receiver.initial.push_back(SurfaceRequest::Action(
                    owner,
                    UiSurfaceAction::Progress { active },
                ));
            }
            UiSurfaceAction::ProgramStatus {
                state,
                text,
                progress,
            } => {
                terminal.set_program_status(&owner, state.clone(), text.clone(), progress)?;
                receiver.initial.push_back(SurfaceRequest::Action(
                    owner,
                    UiSurfaceAction::ProgramStatus {
                        state,
                        text,
                        progress,
                    },
                ));
            }
            // The full frontend applies editor commands and product actions after activation.
            other => receiver
                .initial
                .push_back(SurfaceRequest::Action(owner, other)),
        },
        SurfaceRequest::Action(_, _) => {}
    }
    Ok(())
}

fn handle_input(
    event: Option<Result<super::input::InputPacket, std::io::Error>>,
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

    surface.graphics.borrow_mut().begin();
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
    let paint = surface.graphics.borrow_mut().prepare(completed.buffer);
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
            .and_then(extension_operations::theme)
            .unwrap_or_else(|| {
                super::UiPalette::from_background(super::background_from_colorfgbg())
            }),
    );
    surface
        .graphics
        .borrow_mut()
        .set_enabled(receiver.show_images);
    let mut clipboard = SystemClipboard::default();

    let result = loop {
        tokio::select! {
            result = &mut startup => break result,
            request = receiver.requests.recv() => {
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
                handle_request(
                    request,
                    receiver,
                    &mut surface,
                    frontend,
                    &mut clipboard,
                    &current,
                ).await?;
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
