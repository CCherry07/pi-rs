#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use pi_core::AbortSignal;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub timed_out: bool,
    pub truncated: bool,
    pub truncation: Option<ShellTruncation>,
    pub full_output_path: Option<PathBuf>,
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
    let mut capture = OutputCapture::default();
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
                        record_chunk(&mut capture, request.on_chunk.as_ref(), stream, &bytes).await?;
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
        record_chunk(&mut capture, request.on_chunk.as_ref(), stream, &bytes).await?;
    }
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    capture.finish().await?;
    let truncation = capture.truncation();
    let output = truncation
        .as_ref()
        .map_or_else(|| capture.tail_text(), |value| value.content.clone());
    let truncated = truncation.is_some();
    Ok(ShellResult {
        output,
        exit_code: status.code(),
        cancelled,
        timed_out,
        truncated,
        truncation,
        full_output_path: capture.full_output_path,
    })
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
            .prefix("pi-bash-")
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
