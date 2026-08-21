#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use pi_core::AbortSignal;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_OUTPUT_BYTES: usize = 1_000_000;
pub const MAX_OUTPUT_LINES: usize = 2_000;
const CAPTURE_TAIL_BYTES: usize = 2 * MAX_OUTPUT_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellChunk {
    pub stream: ShellStream,
    pub text: String,
}

pub type ShellChunkSink = Arc<dyn Fn(ShellChunk) + Send + Sync>;

pub struct ShellRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Option<Duration>,
    pub shell_path: Option<PathBuf>,
    pub abort_signal: AbortSignal,
    pub on_chunk: Option<ShellChunkSink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub timed_out: bool,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("invalid shell working directory: {0}")]
    InvalidCwd(String),
    #[error("failed to spawn shell: {0}")]
    Spawn(String),
    #[error("shell process failed: {0}")]
    Process(String),
}

pub async fn execute(request: ShellRequest) -> Result<ShellResult, ShellError> {
    let cwd = std::fs::canonicalize(&request.cwd)
        .map_err(|error| ShellError::InvalidCwd(error.to_string()))?;
    let (shell, flag) = resolve_shell(request.shell_path);
    let mut child = Command::new(shell)
        .arg(flag)
        .arg(&request.command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ShellError::Spawn(error.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ShellError::Process("missing stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ShellError::Process("missing stderr pipe".to_string()))?;
    let (sender, mut receiver) = mpsc::channel::<(ShellStream, Vec<u8>)>(64);
    let stdout_task = tokio::spawn(read_chunks(stdout, ShellStream::Stdout, sender.clone()));
    let stderr_task = tokio::spawn(read_chunks(stderr, ShellStream::Stderr, sender.clone()));
    drop(sender);

    let deadline = request
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout);
    let mut capture = TailCapture::default();
    let mut cancelled = false;
    let mut timed_out = false;
    let mut chunks_open = true;
    let status = loop {
        tokio::select! {
            biased;
            () = request.abort_signal.wait() => {
                cancelled = true;
                let _ = child.kill().await;
                break child.wait().await.map_err(|error| ShellError::Process(error.to_string()))?;
            }
            () = wait_deadline(deadline), if deadline.is_some() => {
                timed_out = true;
                let _ = child.kill().await;
                break child.wait().await.map_err(|error| ShellError::Process(error.to_string()))?;
            }
            chunk = receiver.recv(), if chunks_open => {
                match chunk {
                    Some((stream, bytes)) => {
                        record_chunk(&mut capture, request.on_chunk.as_ref(), stream, &bytes);
                    }
                    None => chunks_open = false,
                }
            }
            status = child.wait() => {
                break status.map_err(|error| ShellError::Process(error.to_string()))?;
            }
        }
    };

    while let Some((stream, bytes)) = receiver.recv().await {
        record_chunk(&mut capture, request.on_chunk.as_ref(), stream, &bytes);
    }
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    let (output, truncated) = truncate_tail(&String::from_utf8_lossy(&capture.bytes));
    Ok(ShellResult {
        output,
        exit_code: status.code(),
        cancelled,
        timed_out,
        truncated: truncated || capture.dropped,
    })
}

fn resolve_shell(shell_path: Option<PathBuf>) -> (PathBuf, &'static str) {
    if let Some(shell) = shell_path {
        return (shell, if cfg!(windows) { "/C" } else { "-c" });
    }
    if cfg!(windows) {
        (PathBuf::from("cmd"), "/C")
    } else {
        (PathBuf::from("/bin/sh"), "-c")
    }
}

async fn wait_deadline(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn read_chunks(
    mut reader: impl AsyncRead + Unpin,
    stream: ShellStream,
    sender: mpsc::Sender<(ShellStream, Vec<u8>)>,
) -> std::io::Result<()> {
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        if sender
            .send((stream, buffer[..read].to_vec()))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

fn record_chunk(
    capture: &mut TailCapture,
    sink: Option<&ShellChunkSink>,
    stream: ShellStream,
    bytes: &[u8],
) {
    if let Some(sink) = sink {
        sink(ShellChunk {
            stream,
            text: String::from_utf8_lossy(bytes).into_owned(),
        });
    }
    capture.push(bytes);
}

#[derive(Default)]
struct TailCapture {
    bytes: Vec<u8>,
    dropped: bool,
}

impl TailCapture {
    fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= CAPTURE_TAIL_BYTES {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&bytes[bytes.len() - CAPTURE_TAIL_BYTES..]);
            self.dropped = true;
            return;
        }
        let overflow = self
            .bytes
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(CAPTURE_TAIL_BYTES);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.dropped = true;
        }
        self.bytes.extend_from_slice(bytes);
    }
}

fn truncate_tail(text: &str) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES && text.lines().count() <= MAX_OUTPUT_LINES {
        return (text.to_string(), false);
    }
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for line in text.lines().rev() {
        if kept.len() >= MAX_OUTPUT_LINES || bytes.saturating_add(line.len() + 1) > MAX_OUTPUT_BYTES
        {
            break;
        }
        bytes += line.len() + 1;
        kept.push(line);
    }
    kept.reverse();
    (kept.join("\n"), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    #[tokio::test]
    async fn captures_combined_output_and_status() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let result = execute(ShellRequest {
            command: "printf hello; printf error >&2; exit 7".to_string(),
            cwd: directory.path().to_path_buf(),
            timeout: Some(DEFAULT_TIMEOUT),
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        })
        .await
        .unwrap();
        assert!(result.output.contains("hello"));
        assert!(result.output.contains("error"));
        assert_eq!(result.exit_code, Some(7));
    }

    #[tokio::test]
    async fn cancellation_kills_the_child() {
        let directory = tempfile::tempdir().unwrap();
        let (abort, signal) = AbortHandle::new();
        let running = tokio::spawn(execute(ShellRequest {
            command: "sleep 30".to_string(),
            cwd: directory.path().to_path_buf(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        }));
        tokio::task::yield_now().await;
        abort.abort();
        assert!(running.await.unwrap().unwrap().cancelled);
    }
}
