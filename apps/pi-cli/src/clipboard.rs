use std::io::{self, Write as _};
use std::process::{Command, Stdio};

use base64::Engine as _;

const OSC52_MAX_RAW_BYTES: usize = 100_000;

pub(crate) trait ClipboardWriter {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

#[derive(Default)]
pub(crate) struct SystemClipboard {
    native: Option<arboard::Clipboard>,
}

impl ClipboardWriter for SystemClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        let environment = ClipboardEnvironment::detect();
        copy_text_with(
            text,
            environment,
            |text| self.set_native_text(text),
            wsl_clipboard_copy,
            tmux_clipboard_copy,
            osc52_copy,
        )
    }
}

impl SystemClipboard {
    fn set_native_text(&mut self, text: &str) -> Result<(), String> {
        if self.native.is_none() {
            self.native = Some(
                arboard::Clipboard::new()
                    .map_err(|error| format!("clipboard unavailable: {error}"))?,
            );
        }
        self.native
            .as_mut()
            .expect("clipboard initialized")
            .set_text(text.to_string())
            .map_err(|error| format!("failed to set clipboard text: {error}"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClipboardEnvironment {
    ssh_session: bool,
    tmux_session: bool,
    wsl_session: bool,
}

impl ClipboardEnvironment {
    fn detect() -> Self {
        Self {
            ssh_session: std::env::var_os("SSH_TTY").is_some()
                || std::env::var_os("SSH_CONNECTION").is_some(),
            tmux_session: std::env::var_os("TMUX").is_some()
                || std::env::var_os("TMUX_PANE").is_some(),
            wsl_session: is_wsl_session(),
        }
    }
}

fn copy_text_with(
    text: &str,
    environment: ClipboardEnvironment,
    mut native_copy: impl FnMut(&str) -> Result<(), String>,
    wsl_copy: impl Fn(&str) -> Result<(), String>,
    tmux_copy: impl Fn(&str) -> Result<(), String>,
    osc52_copy: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if environment.ssh_session {
        return terminal_copy_with(text, environment.tmux_session, &tmux_copy, &osc52_copy)
            .map_err(|error| format!("terminal clipboard failed over SSH: {error}"));
    }

    match native_copy(text) {
        Ok(()) => Ok(()),
        Err(native_error) if environment.wsl_session => match wsl_copy(text) {
            Ok(()) => Ok(()),
            Err(wsl_error) => terminal_copy_with(
                text,
                environment.tmux_session,
                &tmux_copy,
                &osc52_copy,
            )
            .map_err(|terminal_error| {
                format!(
                    "native clipboard: {native_error}; WSL clipboard: {wsl_error}; terminal clipboard: {terminal_error}"
                )
            }),
        },
        Err(native_error) => terminal_copy_with(
            text,
            environment.tmux_session,
            &tmux_copy,
            &osc52_copy,
        )
        .map_err(|terminal_error| {
            format!("native clipboard: {native_error}; terminal clipboard: {terminal_error}")
        }),
    }
}

fn terminal_copy_with(
    text: &str,
    tmux_session: bool,
    tmux_copy: &impl Fn(&str) -> Result<(), String>,
    osc52_copy: &impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if tmux_session {
        match tmux_copy(text) {
            Ok(()) => return Ok(()),
            Err(tmux_error) => {
                return osc52_copy(text).map_err(|osc52_error| {
                    format!("tmux clipboard: {tmux_error}; OSC 52: {osc52_error}")
                });
            }
        }
    }
    osc52_copy(text).map_err(|error| format!("OSC 52: {error}"))
}

#[cfg(target_os = "linux")]
fn is_wsl_session() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    ["/proc/sys/kernel/osrelease", "/proc/version"]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|contents| contents.to_ascii_lowercase().contains("microsoft"))
}

#[cfg(not(target_os = "linux"))]
fn is_wsl_session() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn wsl_clipboard_copy(text: &str) -> Result<(), String> {
    run_copy_command(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "[Console]::InputEncoding = [System.Text.Encoding]::UTF8; $ErrorActionPreference = 'Stop'; $text = [Console]::In.ReadToEnd(); Set-Clipboard -Value $text",
        ],
        text,
    )
}

#[cfg(not(target_os = "linux"))]
fn wsl_clipboard_copy(_text: &str) -> Result<(), String> {
    Err("WSL clipboard unavailable on this platform".to_string())
}

fn tmux_clipboard_copy(text: &str) -> Result<(), String> {
    let set_clipboard = command_output("tmux", &["show-options", "-gv", "set-clipboard"])?;
    if set_clipboard.trim() == "off" {
        return Err("clipboard forwarding is disabled".to_string());
    }
    let terminal_info = command_output("tmux", &["info"])?;
    if terminal_info
        .lines()
        .any(|line| line.contains("Ms: [missing]"))
    {
        return Err("clipboard forwarding lacks the Ms capability".to_string());
    }
    run_copy_command("tmux", &["load-buffer", "-w", "-"], text)
}

fn command_output(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("failed to spawn {program}: {error}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("{program} output was not UTF-8: {error}"))
    } else {
        Err(command_failure(program, output.status, &output.stderr))
    }
}

fn run_copy_command(program: &str, arguments: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn {program}: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to open {program} stdin"));
    };
    if let Err(error) = stdin.write_all(text.as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to write to {program}: {error}"));
    }
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for {program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(program, output.status, &output.stderr))
    }
}

fn command_failure(program: &str, status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        format!("{program} exited with status {status}")
    } else {
        format!("{program} failed: {stderr}")
    }
}

fn osc52_copy(text: &str) -> Result<(), String> {
    let tmux_session =
        std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some();
    let sequence = osc52_sequence(text, tmux_session)?;
    #[cfg(unix)]
    if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty")
        && write_osc52(tty, &sequence).is_ok()
    {
        return Ok(());
    }
    write_osc52(io::stdout().lock(), &sequence)
}

fn osc52_sequence(text: &str, tmux_session: bool) -> Result<String, String> {
    if text.len() > OSC52_MAX_RAW_BYTES {
        return Err(format!(
            "payload too large ({} bytes; max {OSC52_MAX_RAW_BYTES})",
            text.len()
        ));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if tmux_session {
        Ok(format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\"))
    } else {
        Ok(format!("\x1b]52;c;{encoded}\x07"))
    }
}

fn write_osc52(mut writer: impl io::Write, sequence: &str) -> Result<(), String> {
    writer
        .write_all(sequence.as_bytes())
        .map_err(|error| format!("failed to write OSC 52: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("failed to flush OSC 52: {error}"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn osc52_sequence_encodes_unicode_for_the_host_terminal() {
        assert_eq!(
            osc52_sequence("复制", false).unwrap(),
            "\x1b]52;c;5aSN5Yi2\x07"
        );
    }

    #[test]
    fn osc52_sequence_wraps_tmux_and_rejects_large_payloads() {
        assert_eq!(
            osc52_sequence("copy", true).unwrap(),
            "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\"
        );
        let error = osc52_sequence(&"x".repeat(OSC52_MAX_RAW_BYTES + 1), false).unwrap_err();
        assert!(error.contains("payload too large"));
    }

    #[test]
    fn local_copy_prefers_the_native_clipboard() {
        let native_called = Cell::new(false);
        let terminal_called = Cell::new(false);
        copy_text_with(
            "copy",
            ClipboardEnvironment {
                ssh_session: false,
                tmux_session: false,
                wsl_session: false,
            },
            |_| {
                native_called.set(true);
                Ok(())
            },
            |_| panic!("WSL should not be called"),
            |_| panic!("tmux should not be called"),
            |_| {
                terminal_called.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert!(native_called.get());
        assert!(!terminal_called.get());
    }

    #[test]
    fn ssh_copy_uses_tmux_then_osc52_without_touching_native_clipboard() {
        let tmux_called = Cell::new(false);
        let osc52_called = Cell::new(false);
        copy_text_with(
            "copy",
            ClipboardEnvironment {
                ssh_session: true,
                tmux_session: true,
                wsl_session: false,
            },
            |_| panic!("native clipboard should not be called over SSH"),
            |_| panic!("WSL should not be called"),
            |_| {
                tmux_called.set(true);
                Err("unavailable".to_string())
            },
            |_| {
                osc52_called.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert!(tmux_called.get());
        assert!(osc52_called.get());
    }

    #[test]
    fn wsl_copy_falls_back_through_powershell_before_terminal_copy() {
        let wsl_called = Cell::new(false);
        let terminal_called = Cell::new(false);
        copy_text_with(
            "copy",
            ClipboardEnvironment {
                ssh_session: false,
                tmux_session: false,
                wsl_session: true,
            },
            |_| Err("native unavailable".to_string()),
            |_| {
                wsl_called.set(true);
                Ok(())
            },
            |_| panic!("tmux should not be called"),
            |_| {
                terminal_called.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert!(wsl_called.get());
        assert!(!terminal_called.get());
    }
}
