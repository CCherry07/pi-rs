//! Local shell ownership and cancellation, including inherited output pipes.

#[cfg(windows)]
use std::process::Stdio;

/// Guard only unfinished invocations. A completed shell may intentionally have
/// launched a daemon; like Pi, successful completion does not kill that daemon.
pub(super) struct ProcessTree(Option<u32>);

impl ProcessTree {
    pub(super) fn new(pid: Option<u32>) -> Self {
        Self(pid)
    }

    pub(super) fn terminate(&self) {
        let Some(pid) = self.0 else { return };
        terminate(pid);
    }

    pub(super) fn release(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnixSignalTarget {
    Group(i32),
    Process(i32),
}

#[cfg(unix)]
fn terminate_unix_with(
    pid: i32,
    stop_group_first: bool,
    mut send: impl FnMut(UnixSignalTarget, nix::sys::signal::Signal) -> Result<(), ()>,
) {
    use nix::sys::signal::Signal;

    // macOS can wake a waiting shell between signals to group members. Stop
    // the group first so cancellation cannot advance `command; next_command`.
    if stop_group_first {
        let _ = send(UnixSignalTarget::Group(pid), Signal::SIGSTOP);
    }
    if send(UnixSignalTarget::Group(pid), Signal::SIGKILL).is_err() {
        let _ = send(UnixSignalTarget::Process(pid), Signal::SIGKILL);
    }
}

#[cfg(unix)]
fn terminate(pid: u32) {
    use nix::sys::signal::{kill, killpg};
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else { return };
    terminate_unix_with(pid, cfg!(target_os = "macos"), |target, signal| {
        let result = match target {
            UnixSignalTarget::Group(pid) => killpg(Pid::from_raw(pid), signal),
            UnixSignalTarget::Process(pid) => kill(Pid::from_raw(pid), signal),
        };
        result.map_err(|_| ())
    });
}

#[cfg(windows)]
fn terminate(pid: u32) {
    use std::os::windows::process::CommandExt;

    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    // Wait for taskkill to finish. If this guard is being dropped during
    // shutdown, returning first lets the OS job close the shell before
    // taskkill can discover its descendants.
    let _ = std::process::Command::new(std::path::Path::new(&root).join("System32/taskkill.exe"))
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .status();
}

#[cfg(not(any(unix, windows)))]
fn terminate(_pid: u32) {}

/// The I/O tasks must not outlive a cancelled/dropped shell future.
pub(super) struct PipeReaders {
    pub(super) readers: [tokio::task::JoinHandle<std::io::Result<()>>; 2],
    pub(super) stdin: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Drop for PipeReaders {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        if let Some(stdin) = &self.stdin {
            stdin.abort();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;

    #[test]
    fn macos_stops_the_group_before_killing_it() {
        let mut calls = Vec::new();
        terminate_unix_with(1234, true, |target, signal| {
            calls.push((target, signal));
            Ok(())
        });
        assert_eq!(
            calls,
            [
                (UnixSignalTarget::Group(1234), Signal::SIGSTOP),
                (UnixSignalTarget::Group(1234), Signal::SIGKILL),
            ]
        );
    }

    #[test]
    fn macos_still_kills_the_group_when_stopping_fails() {
        let mut calls = Vec::new();
        terminate_unix_with(1234, true, |target, signal| {
            calls.push((target, signal));
            if signal == Signal::SIGSTOP {
                Err(())
            } else {
                Ok(())
            }
        });
        assert_eq!(
            calls,
            [
                (UnixSignalTarget::Group(1234), Signal::SIGSTOP),
                (UnixSignalTarget::Group(1234), Signal::SIGKILL),
            ]
        );
    }

    #[test]
    fn failed_group_kill_falls_back_to_the_shell() {
        let mut calls = Vec::new();
        terminate_unix_with(1234, true, |target, signal| {
            calls.push((target, signal));
            if target == UnixSignalTarget::Group(1234) && signal == Signal::SIGKILL {
                Err(())
            } else {
                Ok(())
            }
        });
        assert_eq!(
            calls,
            [
                (UnixSignalTarget::Group(1234), Signal::SIGSTOP),
                (UnixSignalTarget::Group(1234), Signal::SIGKILL),
                (UnixSignalTarget::Process(1234), Signal::SIGKILL),
            ]
        );
    }

    #[test]
    fn failures_of_every_signal_are_tolerated() {
        let mut calls = Vec::new();
        terminate_unix_with(1234, true, |target, signal| {
            calls.push((target, signal));
            Err(())
        });
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn linux_kills_the_group_without_a_stop() {
        let mut calls = Vec::new();
        terminate_unix_with(1234, false, |target, signal| {
            calls.push((target, signal));
            Ok(())
        });
        assert_eq!(calls, [(UnixSignalTarget::Group(1234), Signal::SIGKILL)]);
    }
}
