#![forbid(unsafe_code)]

mod operations;
mod process_tree;
pub use operations::{ShellOperations, ShellOperationsRequest, execute_with_operations};

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use pi_core::AbortSignal;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_OUTPUT_BYTES: usize = 50 * 1024;
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

/// A shell adapter's executable and arguments. Process capture and
/// cancellation remain shared across shell implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfiguration {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub output_file_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullShellOutput {
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub timed_out: bool,
    /// On timeout, when the shell exited before the deadline but descendants
    /// were still writing to its output pipes, retain the real exit timing.
    pub shell_exit_after: Option<Duration>,
    pub truncated: bool,
    pub truncation: Option<ShellTruncation>,
    pub full_output_path: Option<PathBuf>,
}

impl ShellResult {
    /// Reads the complete captured output for programmatic callers. Large
    /// results retain both ends and link to the complete persisted file.
    pub async fn read_full_output(&self, max_bytes: usize) -> Result<FullShellOutput, ShellError> {
        let Some(path) = &self.full_output_path else {
            return Ok(FullShellOutput {
                content: self.output.clone(),
                truncated: false,
            });
        };
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|error| ShellError::Process(error.to_string()))?;
        let size = file
            .metadata()
            .await
            .map_err(|error| ShellError::Process(error.to_string()))?
            .len();
        if size <= max_bytes as u64 {
            let mut bytes = Vec::with_capacity(size as usize);
            file.read_to_end(&mut bytes)
                .await
                .map_err(|error| ShellError::Process(error.to_string()))?;
            return Ok(FullShellOutput {
                content: String::from_utf8_lossy(&bytes).into_owned(),
                truncated: false,
            });
        }
        let head_bytes = max_bytes / 2;
        let tail_bytes = max_bytes - head_bytes;
        let mut head = vec![0; head_bytes];
        let mut tail = vec![0; tail_bytes];
        file.read_exact(&mut head)
            .await
            .map_err(|error| ShellError::Process(error.to_string()))?;
        file.seek(std::io::SeekFrom::End(-(tail_bytes as i64)))
            .await
            .map_err(|error| ShellError::Process(error.to_string()))?;
        file.read_exact(&mut tail)
            .await
            .map_err(|error| ShellError::Process(error.to_string()))?;
        let mut offset = 0;
        let head_end = loop {
            match std::str::from_utf8(&head[offset..]) {
                Ok(_) => break head.len(),
                Err(error) => match error.error_len() {
                    Some(length) => offset += error.valid_up_to() + length,
                    None => break offset + error.valid_up_to(),
                },
            }
        };
        let tail_start = tail
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(tail.len());
        let content = format!(
            "{}\n\n[... {} bytes omitted ...]\n\n{}",
            String::from_utf8_lossy(&head[..head_end]),
            size - max_bytes as u64,
            String::from_utf8_lossy(&tail[tail_start..])
        );
        Ok(FullShellOutput {
            content,
            truncated: true,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTruncation {
    pub content: String,
    pub truncated_by: TruncatedBy,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub last_line_bytes: usize,
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
    execute_with_environment(request, BTreeMap::new()).await
}

/// Executes with per-child environment overrides. `None` removes an inherited
/// variable; values never modify the host process environment.
pub async fn execute_with_environment(
    request: ShellRequest,
    environment: BTreeMap<OsString, Option<OsString>>,
) -> Result<ShellResult, ShellError> {
    let (shell, flag) = resolve_shell(request.shell_path.clone());
    execute_configured(
        request,
        environment,
        ShellConfiguration {
            executable: shell,
            arguments: vec![flag.into()],
            output_file_prefix: "pi-bash-".into(),
        },
    )
    .await
}

pub async fn execute_configured(
    request: ShellRequest,
    environment: BTreeMap<OsString, Option<OsString>>,
    configuration: ShellConfiguration,
) -> Result<ShellResult, ShellError> {
    if request.abort_signal.is_aborted() {
        return Ok(ShellResult {
            output: String::new(),
            exit_code: None,
            cancelled: true,
            timed_out: false,
            shell_exit_after: None,
            truncated: false,
            truncation: None,
            full_output_path: None,
        });
    }
    let cwd = std::fs::canonicalize(&request.cwd)
        .map_err(|error| ShellError::InvalidCwd(error.to_string()))?;
    let mut command = Command::new(configuration.executable);
    command.args(configuration.arguments);
    command.arg(&request.command);
    for (key, value) in environment {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    // Pi starts Unix shells in their own process group so cancellation also
    // reaches descendants after the shell itself has already exited.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ShellError::Spawn(error.to_string()))?;
    let started_at = std::time::Instant::now();
    let mut process_tree = process_tree::ProcessTree::new(child.id());
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
    let readers = process_tree::PipeReaders([stdout_task, stderr_task]);
    drop(sender);

    let deadline = request
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout);
    let mut capture = OutputCapture {
        output_file_prefix: configuration.output_file_prefix,
        ..Default::default()
    };
    let mut cancelled = false;
    let mut timed_out = false;
    let mut chunks_open = true;
    let mut status = None;
    let mut shell_exit_after = None;
    let mut idle_deadline = None;
    loop {
        if status.is_some() && !chunks_open {
            break;
        }
        tokio::select! {
            biased;
            () = request.abort_signal.wait(), if !cancelled && !timed_out => {
                cancelled = true;
                process_tree.terminate();
                let _ = child.start_kill();
            }
            () = wait_deadline(deadline), if deadline.is_some() && !cancelled && !timed_out => {
                timed_out = true;
                process_tree.terminate();
                let _ = child.start_kill();
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|error| ShellError::Process(error.to_string()))?);
                if !cancelled && !timed_out {
                    shell_exit_after = Some(started_at.elapsed());
                }
                idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(100));
            }
            chunk = receiver.recv(), if chunks_open => {
                match chunk {
                    Some((stream, bytes)) => {
                        record_chunk(&mut capture, request.on_chunk.as_ref(), stream, &bytes).await?;
                        if status.is_some() {
                            idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(100));
                        }
                    }
                    None => chunks_open = false,
                }
            }
            () = wait_deadline(idle_deadline), if idle_deadline.is_some() => break,
        }
    }
    // A quiet inherited pipe must not keep an invocation open forever. Active
    // writers rearm the grace period on every chunk, up to timeout/abort.
    drop(readers);
    process_tree.release();
    let status = status.expect("the pipe grace timer starts only after shell exit");
    cancelled |= request.abort_signal.is_aborted();
    capture.finish().await?;
    let truncation = capture.truncation();
    let output = truncation
        .as_ref()
        .map_or_else(|| capture.tail_text(), |value| value.content.clone());
    let truncated = truncation.is_some();
    Ok(ShellResult {
        output,
        exit_code: shell_exit_code(status),
        cancelled,
        timed_out,
        shell_exit_after: shell_exit_after.filter(|_| timed_out),
        truncated,
        truncation,
        full_output_path: capture.full_output_path,
    })
}

fn shell_exit_code(status: std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .or_else(|| status.signal().map(|signal| 128 + signal))
    }
    #[cfg(not(unix))]
    {
        status.code()
    }
}

fn resolve_shell(shell_path: Option<PathBuf>) -> (PathBuf, &'static str) {
    if let Some(shell) = shell_path {
        return (shell, if cfg!(windows) { "/C" } else { "-c" });
    }
    if cfg!(windows) {
        (PathBuf::from("cmd"), "/C")
    } else if std::path::Path::new("/bin/bash").exists() {
        (PathBuf::from("/bin/bash"), "-c")
    } else if let Some(bash) = executable_on_path("bash") {
        (bash, "-c")
    } else {
        (PathBuf::from("sh"), "-c")
    }
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
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

async fn record_chunk(
    capture: &mut OutputCapture,
    sink: Option<&ShellChunkSink>,
    stream: ShellStream,
    bytes: &[u8],
) -> Result<(), ShellError> {
    if let Some(sink) = sink {
        sink(ShellChunk {
            stream,
            text: String::from_utf8_lossy(bytes).into_owned(),
        });
    }
    capture.push(bytes).await
}

#[derive(Default)]
struct OutputCapture {
    tail: TailCapture,
    buffered: Vec<u8>,
    full_output: Option<tokio::fs::File>,
    full_output_path: Option<PathBuf>,
    total_bytes: usize,
    newline_count: usize,
    last_byte: Option<u8>,
    last_line_bytes: usize,
    output_file_prefix: String,
}

impl OutputCapture {
    async fn push(&mut self, bytes: &[u8]) -> Result<(), ShellError> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.tail.push(bytes);
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.newline_count = self
            .newline_count
            .saturating_add(bytes.iter().filter(|byte| **byte == b'\n').count());
        self.last_byte = bytes.last().copied();
        if let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') {
            self.last_line_bytes = bytes.len() - last_newline - 1;
        } else {
            self.last_line_bytes = self.last_line_bytes.saturating_add(bytes.len());
        }

        if let Some(output) = self.full_output.as_mut() {
            output
                .write_all(bytes)
                .await
                .map_err(|error| ShellError::Process(error.to_string()))?;
        } else {
            self.buffered.extend_from_slice(bytes);
            if self.should_persist() {
                self.start_persisting().await?;
            }
        }
        Ok(())
    }

    async fn finish(&mut self) -> Result<(), ShellError> {
        if let Some(output) = self.full_output.as_mut() {
            output
                .flush()
                .await
                .map_err(|error| ShellError::Process(error.to_string()))?;
        }
        Ok(())
    }

    fn total_lines(&self) -> usize {
        self.newline_count + usize::from(self.total_bytes > 0 && self.last_byte != Some(b'\n'))
    }

    fn should_persist(&self) -> bool {
        self.total_bytes > MAX_OUTPUT_BYTES || self.total_lines() > MAX_OUTPUT_LINES
    }

    async fn start_persisting(&mut self) -> Result<(), ShellError> {
        let temporary = tempfile::Builder::new()
            .prefix(if self.output_file_prefix.is_empty() {
                "pi-bash-"
            } else {
                &self.output_file_prefix
            })
            .suffix(".log")
            .tempfile()
            .map_err(|error| ShellError::Process(error.to_string()))?;
        let (output, path) = temporary
            .keep()
            .map_err(|error| ShellError::Process(error.error.to_string()))?;
        let mut output = tokio::fs::File::from_std(output);
        output
            .write_all(&self.buffered)
            .await
            .map_err(|error| ShellError::Process(error.to_string()))?;
        self.buffered.clear();
        self.full_output = Some(output);
        self.full_output_path = Some(path);
        Ok(())
    }

    fn tail_text(&self) -> String {
        String::from_utf8_lossy(&self.tail.bytes).into_owned()
    }

    fn truncation(&self) -> Option<ShellTruncation> {
        if !self.should_persist() {
            return None;
        }
        Some(truncate_tail(
            &self.tail_text(),
            self.total_lines(),
            self.total_bytes,
            self.last_line_bytes,
        ))
    }
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

fn truncate_tail(
    text: &str,
    total_lines: usize,
    total_bytes: usize,
    last_line_bytes: usize,
) -> ShellTruncation {
    let lines = lines_for_counting(text);
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;
    for line in lines.iter().rev().take(MAX_OUTPUT_LINES) {
        let line_bytes = line.len() + usize::from(!kept.is_empty());
        if bytes.saturating_add(line_bytes) > MAX_OUTPUT_BYTES {
            truncated_by = TruncatedBy::Bytes;
            if kept.is_empty() {
                let partial = truncate_utf8_from_end(line, MAX_OUTPUT_BYTES);
                bytes = partial.len();
                kept.push(partial);
                last_line_partial = true;
            }
            break;
        }
        bytes += line_bytes;
        kept.push((*line).to_string());
    }
    if kept.len() >= MAX_OUTPUT_LINES && bytes <= MAX_OUTPUT_BYTES {
        truncated_by = TruncatedBy::Lines;
    }
    kept.reverse();
    let content = kept.join("\n");
    ShellTruncation {
        output_lines: kept.len(),
        output_bytes: content.len(),
        content,
        truncated_by,
        total_lines,
        total_bytes,
        last_line_partial,
        last_line_bytes,
    }
}

fn lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn truncate_utf8_from_end(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && bytes[start] & 0xc0 == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_request_does_not_spawn_a_process() {
        let directory = tempfile::tempdir().unwrap();
        let (abort, signal) = AbortHandle::new();
        abort.abort();
        let result = execute(ShellRequest {
            command: "touch side-effect".into(),
            cwd: directory.path().into(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        })
        .await
        .unwrap();
        assert!(result.cancelled);
        assert!(!directory.path().join("side-effect").exists());
    }

    // Oracle: legacy/pi/packages/coding-agent/src/utils/child-process.ts.
    #[cfg(unix)]
    #[tokio::test]
    async fn quiet_inherited_pipes_release_after_the_shell_exits() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            execute(ShellRequest {
                command: "sleep 2 & printf ready".into(),
                cwd: directory.path().into(),
                timeout: None,
                shell_path: Some("/bin/sh".into()),
                abort_signal: signal,
                on_chunk: None,
            }),
        )
        .await
        .expect("quiet descendants must not hold the shell invocation open")
        .unwrap();
        assert_eq!(result.output, "ready");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn programmatic_output_retains_both_ends_with_utf8_boundaries() {
        let text = format!("head{}tail", "🙂".repeat(300_000));
        let mut capture = OutputCapture::default();
        capture.push(text.as_bytes()).await.unwrap();
        capture.finish().await.unwrap();
        let result = ShellResult {
            output: capture.tail_text(),
            exit_code: Some(0),
            cancelled: false,
            timed_out: false,
            shell_exit_after: None,
            truncated: true,
            truncation: capture.truncation(),
            full_output_path: capture.full_output_path.clone(),
        };
        let limited = result.read_full_output(1024 * 1024 + 2).await.unwrap();
        assert!(limited.truncated);
        assert!(limited.content.starts_with("head🙂"));
        assert!(limited.content.ends_with("🙂tail"));
        assert!(limited.content.contains("bytes omitted"));
        assert!(!limited.content.contains('\u{fffd}'));
        let complete = result.read_full_output(text.len()).await.unwrap();
        assert!(!complete.truncated);
        assert_eq!(complete.content, text);
        std::fs::remove_file(result.full_output_path.unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_shell_uses_its_arguments_and_output_file_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let result = execute_configured(
            ShellRequest {
                command: "seq 3000".into(),
                cwd: directory.path().into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            },
            BTreeMap::new(),
            ShellConfiguration {
                executable: "/bin/sh".into(),
                arguments: vec!["-c".into()],
                output_file_prefix: "pi-configured-shell-".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(
            result
                .full_output_path
                .as_ref()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pi-configured-shell-")
        );
        let complete = result.read_full_output(1024 * 1024).await.unwrap();
        assert!(!complete.truncated);
        assert!(complete.content.starts_with("1\n2\n"));
        std::fs::remove_file(result.full_output_path.unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn environment_overrides_set_and_remove_only_child_values() {
        let directory = tempfile::tempdir().unwrap();
        let original_home = std::env::var_os("HOME");
        let (_, signal) = AbortHandle::new();
        let result = execute_with_environment(
            ShellRequest {
                command: "printf '%s|%s' \"${HOME-unset}\" \"$PI_TEST_VALUE\"".into(),
                cwd: directory.path().to_path_buf(),
                timeout: Some(DEFAULT_TIMEOUT),
                shell_path: Some("/bin/sh".into()),
                abort_signal: signal,
                on_chunk: None,
            },
            BTreeMap::from([
                (OsString::from("HOME"), None),
                (
                    OsString::from("PI_TEST_VALUE"),
                    Some(OsString::from("quoted ' value $()")),
                ),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(result.output, "unset|quoted ' value $()");
        assert_eq!(std::env::var_os("HOME"), original_home);
    }

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

    #[tokio::test]
    async fn persists_full_output_and_reports_exact_line_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let result = execute(ShellRequest {
            command: "seq 3000".to_string(),
            cwd: directory.path().to_path_buf(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        })
        .await
        .unwrap();
        let truncation = result.truncation.as_ref().unwrap();
        assert_eq!(truncation.truncated_by, TruncatedBy::Lines);
        assert_eq!(truncation.total_lines, 3_000);
        assert_eq!(truncation.output_lines, 2_000);
        assert!(result.output.starts_with("1001\n"));
        assert!(result.output.ends_with("3000"));
        let full_output_path = result.full_output_path.as_ref().unwrap();
        let full_output = std::fs::read_to_string(full_output_path).unwrap();
        assert!(full_output.starts_with("1\n2\n3\n"));
        assert!(full_output.ends_with("2999\n3000\n"));
        std::fs::remove_file(full_output_path).unwrap();
    }

    #[tokio::test]
    async fn keeps_a_byte_limited_tail_of_an_oversized_last_line() {
        let mut capture = OutputCapture::default();
        capture.push(&vec![b'x'; 60 * 1024]).await.unwrap();
        capture.finish().await.unwrap();
        let truncation = capture.truncation().unwrap();
        assert_eq!(truncation.truncated_by, TruncatedBy::Bytes);
        assert!(truncation.last_line_partial);
        assert_eq!(truncation.last_line_bytes, 60 * 1024);
        assert_eq!(truncation.output_bytes, MAX_OUTPUT_BYTES);
        let path = capture.full_output_path.as_ref().unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), 60 * 1024);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn utf8_tail_matches_byte_slicing_without_splitting_code_points() {
        for input in ["", "ascii", "aé🙂b", "中文🙂tail", "👩‍💻"] {
            for max_bytes in 0..=input.len() + 2 {
                let expected = if input.len() <= max_bytes {
                    input
                } else {
                    let mut start = input.len() - max_bytes;
                    while start < input.len() && !input.is_char_boundary(start) {
                        start += 1;
                    }
                    &input[start..]
                };
                let actual = truncate_utf8_from_end(input, max_bytes);
                assert_eq!(actual, expected, "input={input:?}, max_bytes={max_bytes}");
                assert!(actual.len() <= max_bytes);
            }
        }
    }

    #[test]
    fn line_count_ignores_one_trailing_newline() {
        assert_eq!(lines_for_counting("one\ntwo\n"), vec!["one", "two"]);
        assert_eq!(lines_for_counting("one\ntwo"), vec!["one", "two"]);
        assert!(lines_for_counting("").is_empty());
    }

    #[test]
    fn oversized_unicode_last_line_keeps_a_valid_byte_limited_tail() {
        let input = format!("prefix{}", "🙂".repeat(20_000));
        let truncated = truncate_tail(&input, 1, input.len(), input.len());

        assert_eq!(truncated.truncated_by, TruncatedBy::Bytes);
        assert!(truncated.last_line_partial);
        assert!(truncated.output_bytes <= MAX_OUTPUT_BYTES);
        assert!(truncated.content.ends_with('🙂'));
        assert_eq!(
            truncated.content,
            truncate_utf8_from_end(&input, MAX_OUTPUT_BYTES)
        );
    }
}
