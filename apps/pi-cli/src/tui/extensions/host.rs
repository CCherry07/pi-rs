//! Physical terminal and operating-system operations requested through semantic UI effects.
use super::super::{ClipboardWriter, TerminalSession};
use pi_plugin::UiHostOperation;
use std::{path::Path, process::Stdio};

pub(in crate::tui) fn set_title(title: &str) -> Result<(), String> {
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::SetTitle(title.replace(['\x1b', '\x07'], ""))
    )
    .map_err(|error| error.to_string())
}

pub(in crate::tui) fn set_progress(active: bool) -> Result<(), String> {
    crossterm::execute!(
        std::io::stdout(),
        crossterm::style::Print(if active {
            "\x1b]9;4;3;0\x07"
        } else {
            "\x1b]9;4;0;0\x07"
        })
    )
    .map_err(|error| error.to_string())
}

pub(in crate::tui) fn set_program_status(
    state: &str,
    text: Option<&str>,
    progress: Option<u8>,
) -> Result<(), String> {
    if !matches!(state, "working" | "error" | "success" | "clear") {
        return Err(format!("Unknown program status: {state}"));
    }
    let mut sequence = format!("\x1b]7501;state={state}");
    if state != "clear" {
        sequence.push_str(":app=pi");
        if let Some(text) = text {
            let text: String = text
                .chars()
                .filter(|c| !c.is_control() && !matches!(c, ':' | ';' | '='))
                .collect();
            sequence.push_str(&format!(":text={text}"));
        }
        if let Some(progress) = progress {
            sequence.push_str(&format!(":progress={}", progress.min(100)));
        }
    }
    sequence.push_str("\x1b\\");
    crossterm::execute!(std::io::stdout(), crossterm::style::Print(sequence))
        .map_err(|error| error.to_string())
}

pub(in crate::tui) fn theme(name: &str) -> Option<super::super::UiPalette> {
    use super::super::{RgbColor, UiPalette};
    match name {
        "light" => Some(UiPalette::from_background(Some(RgbColor::new(
            255, 255, 255,
        )))),
        "dark" => Some(UiPalette::from_background(Some(RgbColor::new(0, 0, 0)))),
        "system" => Some(UiPalette::from_background(
            super::super::background_from_colorfgbg(),
        )),
        _ => None,
    }
}

pub(in crate::tui) async fn execute(
    operation: UiHostOperation,
    terminal: &mut TerminalSession,
    clipboard: &mut impl ClipboardWriter,
    cwd: &Path,
    default_editor: Option<&str>,
) -> Result<Option<String>, String> {
    if let UiHostOperation::Clipboard { text } = &operation {
        clipboard.set_text(text)?;
        return Ok(None);
    }
    terminal.restore().map_err(|error| error.to_string())?;
    let result = match operation {
        UiHostOperation::ExternalEditor { text, command } => edit(
            text,
            command.or_else(|| default_editor.map(str::to_owned)),
            cwd,
        )
        .await
        .map(Some),
        UiHostOperation::Suspend => suspend().await.map(|()| None),
        UiHostOperation::Clipboard { .. } => unreachable!(),
    };
    // Always reacquire terminal modes, including failed launches/nonzero editor exits.
    let resumed = terminal.resume().map_err(|error| error.to_string());
    match (result, resumed) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(resume)) => Err(format!("{error}; terminal restore failed: {resume}")),
    }
}

async fn edit(text: String, command: Option<String>, cwd: &Path) -> Result<String, String> {
    let command = command
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "nano".into()
            }
        });
    let directory = tempfile::Builder::new()
        .prefix("pi-editor-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let path = directory.path().join("prompt.md");
    tokio::fs::write(&path, text)
        .await
        .map_err(|e| e.to_string())?;
    // Match Pi's executable + arguments convention; prompt text is never shell code.
    let mut parts = command.split_whitespace();
    let program = parts.next().ok_or("External editor command is empty")?;
    let status = tokio::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|e| format!("Cannot launch editor: {e}"))?;
    if !status.success() {
        return Err(format!("External editor exited with {status}"));
    }
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(text
        .trim_start_matches('\u{feff}')
        .strip_suffix('\n')
        .unwrap_or(text.trim_start_matches('\u{feff}'))
        .to_owned())
}

#[cfg(unix)]
async fn suspend() -> Result<(), String> {
    // rustix provides the target-specific SIGCONT number on every Unix platform.
    let mut continued = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(
        rustix::process::Signal::CONT.as_raw(),
    ))
    .map_err(|e| e.to_string())?;
    rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TSTP)
        .map_err(|e| e.to_string())?;
    continued
        .recv()
        .await
        .ok_or_else(|| "Suspend signal stream closed".to_owned())
}
#[cfg(not(unix))]
async fn suspend() -> Result<(), String> {
    Err("Suspend is unavailable on this platform".into())
}
