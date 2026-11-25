//! Bounded, in-memory capture for programmatic callers. Never creates an output file.
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use pi_core::AbortSignal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::{
    CommandTransport, ShellError, process_tree::ProcessTree, resolve_shell, shell_exit_code,
};

/// Explicit limits for a non-streaming invocation. Commands and captured bytes are never logged.
pub struct ShellCaptureRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub shell_path: Option<PathBuf>,
    pub timeout: Duration,
    pub signal: AbortSignal,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCaptureStatus {
    Exited,
    Cancelled,
    TimedOut,
    OutputLimit,
}

/// Raw streams remain separate. Decoding, trimming and success policy belong to the caller.
/// Debug reports lengths, never captured data. No output/transcript file is written.
pub struct ShellCapture {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub status: ShellCaptureStatus,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl fmt::Debug for ShellCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellCapture")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("exit_code", &self.exit_code)
            .field("status", &self.status)
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr_truncated", &self.stderr_truncated)
            .finish()
    }
}

/// Captures only bounded memory. Timeout, cancellation, overflow, I/O errors and dropping this
/// future terminate the invocation using the same process-tree guard as ordinary shell execution.
/// Limits apply separately; reaching exactly the cap is allowed, the next byte stops the tree.
pub async fn capture(request: ShellCaptureRequest) -> Result<ShellCapture, ShellError> {
    let mut output = ShellCapture {
        stdout: Vec::new(),
        stderr: Vec::new(),
        exit_code: None,
        status: ShellCaptureStatus::Exited,
        stdout_truncated: false,
        stderr_truncated: false,
    };
    if request.signal.is_aborted() {
        output.status = ShellCaptureStatus::Cancelled;
        return Ok(output);
    }
    if request.timeout.is_zero() {
        output.status = ShellCaptureStatus::TimedOut;
        return Ok(output);
    }
    let deadline = tokio::time::Instant::now()
        .checked_add(request.timeout)
        .ok_or_else(|| ShellError::Process("Capture timeout is not representable".into()))?;
    let launch = resolve_shell(request.shell_path)?;
    let cwd = std::fs::canonicalize(request.cwd)
        .map_err(|_| ShellError::InvalidCwd("Capture working directory is unavailable".into()))?;
    let mut command = Command::new(launch.configuration.executable);
    command.args(launch.configuration.arguments);
    if launch.command_transport == CommandTransport::Argument {
        command.arg(&request.command);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .current_dir(cwd)
        .stdin(if launch.command_transport == CommandTransport::Stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ShellError::Spawn(error.to_string()))?;
    let mut tree = ProcessTree::new(child.id());
    let stdin = child.stdin.take();
    let mut input = std::pin::pin!(async move {
        if let Some(mut stdin) = stdin {
            // A parser error may close the input early; the exit status remains authoritative.
            let _ = stdin.write_all(request.command.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
    });
    let mut stdin_done = false;
    let mut stdout = child.stdout.take().expect("requested stdout pipe");
    let mut stderr = child.stderr.take().expect("requested stderr pipe");
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut exit = None;
    let mut out_buffer = [0u8; 8192];
    let mut err_buffer = [0u8; 8192];
    while exit.is_none() || !stdout_done || !stderr_done {
        tokio::select! {
            biased;
            () = request.signal.wait() => {
                output.status = ShellCaptureStatus::Cancelled;
                break;
            }
            () = tokio::time::sleep_until(deadline) => {
                output.status = ShellCaptureStatus::TimedOut;
                break;
            }
            result = child.wait(), if exit.is_none() => {
                exit = Some(result.map_err(|error| ShellError::Process(error.to_string()))?);
            }
            () = &mut input, if !stdin_done => stdin_done = true,
            result = stdout.read(&mut out_buffer), if !stdout_done => {
                let count = result.map_err(|error| ShellError::Process(error.to_string()))?;
                stdout_done = count == 0;
                output.stdout_truncated = append(&mut output.stdout, &out_buffer[..count], request.stdout_limit);
                if output.stdout_truncated {
                    output.status = ShellCaptureStatus::OutputLimit;
                    break;
                }
            }
            result = stderr.read(&mut err_buffer), if !stderr_done => {
                let count = result.map_err(|error| ShellError::Process(error.to_string()))?;
                stderr_done = count == 0;
                output.stderr_truncated = append(&mut output.stderr, &err_buffer[..count], request.stderr_limit);
                if output.stderr_truncated {
                    output.status = ShellCaptureStatus::OutputLimit;
                    break;
                }
            }
        }
    }
    if output.status != ShellCaptureStatus::Exited {
        tree.terminate();
        let _ = child.start_kill();
        if exit.is_none() {
            exit = Some(
                child
                    .wait()
                    .await
                    .map_err(|error| ShellError::Process(error.to_string()))?,
            );
        }
    }
    tree.release();
    output.exit_code = exit.and_then(shell_exit_code);
    Ok(output)
}

fn append(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> bool {
    let keep = bytes.len().min(limit.saturating_sub(output.len()));
    output.extend_from_slice(&bytes[..keep]);
    keep < bytes.len()
}

#[cfg(all(test, unix))]
mod tests;
