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
fn terminate(pid: u32) {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else { return };
    let pid = Pid::from_raw(pid);
    // macOS can wake a waiting shell between signals to group members. Stop
    // the group first so cancellation cannot advance `command; next_command`.
    #[cfg(target_os = "macos")]
    let _ = killpg(pid, Signal::SIGSTOP);
    if killpg(pid, Signal::SIGKILL).is_err() {
        let _ = kill(pid, Signal::SIGKILL);
    }
}

#[cfg(windows)]
fn terminate(pid: u32) {
    use std::os::windows::process::CommandExt;

    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let _ = std::process::Command::new(std::path::Path::new(&root).join("System32/taskkill.exe"))
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn();
}

#[cfg(not(any(unix, windows)))]
fn terminate(_pid: u32) {}

/// The read tasks must not outlive a cancelled/dropped shell future.
pub(super) struct PipeReaders(pub(super) [tokio::task::JoinHandle<std::io::Result<()>>; 2]);

impl Drop for PipeReaders {
    fn drop(&mut self) {
        for reader in &self.0 {
            reader.abort();
        }
    }
}
