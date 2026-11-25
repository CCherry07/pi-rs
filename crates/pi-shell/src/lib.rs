#![forbid(unsafe_code)]

mod capture;
mod operations;
mod process_tree;
pub use capture::{ShellCapture, ShellCaptureRequest, ShellCaptureStatus, capture};
mod user_output;
pub use operations::{ShellOperations, ShellOperationsRequest, execute_with_operations};

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandTransport {
    Argument,
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellLaunch {
    configuration: ShellConfiguration,
    command_transport: CommandTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellPlatform {
    Unix,
    Windows,
}

#[derive(Clone, Copy, Default)]
enum CapturePolicy {
    #[default]
    Raw,
    UserShell,
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

/// Executes user shell shorthand with Pi's display/storage sanitization.
/// Streaming chunks, the returned tail and any full-output file are sanitized;
/// built-in tools continue to use the raw `execute`/`execute_configured` paths.
pub async fn execute_user_shell(request: ShellRequest) -> Result<ShellResult, ShellError> {
    let launch = resolve_shell(request.shell_path.clone())?;
    execute_launch(request, BTreeMap::new(), launch, CapturePolicy::UserShell).await
}

/// Executes with per-child environment overrides. `None` removes an inherited
/// variable; values never modify the host process environment.
pub async fn execute_with_environment(
    request: ShellRequest,
    environment: BTreeMap<OsString, Option<OsString>>,
) -> Result<ShellResult, ShellError> {
    let launch = resolve_shell(request.shell_path.clone())?;
    execute_launch(request, environment, launch, CapturePolicy::Raw).await
}

pub async fn execute_configured(
    request: ShellRequest,
    environment: BTreeMap<OsString, Option<OsString>>,
    configuration: ShellConfiguration,
) -> Result<ShellResult, ShellError> {
    execute_launch(
        request,
        environment,
        ShellLaunch {
            configuration,
            command_transport: CommandTransport::Argument,
        },
        CapturePolicy::Raw,
    )
    .await
}

async fn execute_launch(
    request: ShellRequest,
    environment: BTreeMap<OsString, Option<OsString>>,
    launch: ShellLaunch,
    capture_policy: CapturePolicy,
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
    let cwd = std::fs::canonicalize(&request.cwd).map_err(|_| {
        ShellError::InvalidCwd(format!(
            "Working directory does not exist: {}\nCannot execute shell commands.",
            request.cwd.display()
        ))
    })?;
    let mut command = Command::new(&launch.configuration.executable);
    command.args(&launch.configuration.arguments);
    if launch.command_transport == CommandTransport::Argument {
        command.arg(&request.command);
    }
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
        .stdin(if launch.command_transport == CommandTransport::Stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ShellError::Spawn(format_io_error(&error)))?;
    let started_at = std::time::Instant::now();
    let mut process_tree = process_tree::ProcessTree::new(child.id());
    let stdin_task = if launch.command_transport == CommandTransport::Stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ShellError::Process("missing stdin pipe".to_string()))?;
        let input = request.command.clone().into_bytes();
        Some(tokio::spawn(async move {
            // A parser error can close stdin before the complete command is
            // written. Like Pi's Node backend, that broken pipe is not a host
            // execution failure; the shell's status remains authoritative.
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
            Ok(())
        }))
    } else {
        None
    };
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
    let readers = process_tree::PipeReaders {
        readers: [stdout_task, stderr_task],
        stdin: stdin_task,
    };
    drop(sender);

    let deadline = request
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout);
    let mut capture = OutputCapture::new(capture_policy, launch.configuration.output_file_prefix);
    let mut chunk_decoders = [
        ChunkDecoder::new(capture_policy),
        ChunkDecoder::new(capture_policy),
    ];
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
                        record_chunk(
                            &mut capture,
                            &mut chunk_decoders,
                            request.on_chunk.as_ref(),
                            stream,
                            &bytes,
                        )
                        .await?;
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
    finish_chunk_stream(&mut capture, &mut chunk_decoders, request.on_chunk.as_ref()).await?;
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
        exit_code: if cancelled && matches!(capture_policy, CapturePolicy::UserShell) {
            None
        } else {
            shell_exit_code(status)
        },
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

fn resolve_shell(shell_path: Option<PathBuf>) -> Result<ShellLaunch, ShellError> {
    let platform = if cfg!(windows) {
        ShellPlatform::Windows
    } else {
        ShellPlatform::Unix
    };
    resolve_shell_with(
        shell_path,
        platform,
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
        std::env::var_os("SystemRoot"),
        shell_path_exists,
        executable_on_path,
    )
}

fn resolve_shell_with(
    shell_path: Option<PathBuf>,
    platform: ShellPlatform,
    program_files: Option<OsString>,
    program_files_x86: Option<OsString>,
    system_root: Option<OsString>,
    mut path_exists: impl FnMut(&Path) -> bool,
    mut find_on_path: impl FnMut(&str) -> Option<PathBuf>,
) -> Result<ShellLaunch, ShellError> {
    if let Some(shell) = shell_path {
        if !path_exists(&shell) {
            return Err(ShellError::Spawn(format!(
                "Custom shell path not found: {}",
                shell.display()
            )));
        }
        return Ok(bash_launch(shell, platform, system_root.as_deref()));
    }

    if platform == ShellPlatform::Windows {
        let git_bash_paths = [program_files, program_files_x86]
            .into_iter()
            .flatten()
            .map(|root| PathBuf::from(root).join("Git").join("bin").join("bash.exe"))
            .collect::<Vec<_>>();
        for candidate in &git_bash_paths {
            if path_exists(candidate) {
                return Ok(bash_launch(
                    candidate.clone(),
                    platform,
                    system_root.as_deref(),
                ));
            }
        }
        if let Some(shell) = find_on_path("bash.exe")
            && path_exists(&shell)
        {
            return Ok(bash_launch(shell, platform, system_root.as_deref()));
        }
        let searched = git_bash_paths
            .iter()
            .map(|path| format!("  {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ShellError::Spawn(format!(
            "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{searched}"
        )));
    }

    let standard = PathBuf::from("/bin/bash");
    if path_exists(&standard) {
        return Ok(bash_launch(standard, platform, None));
    }
    Ok(bash_launch(
        find_on_path("bash").unwrap_or_else(|| PathBuf::from("sh")),
        platform,
        None,
    ))
}

fn bash_launch(
    shell: PathBuf,
    platform: ShellPlatform,
    system_root: Option<&OsStr>,
) -> ShellLaunch {
    let command_transport =
        if platform == ShellPlatform::Windows && is_legacy_wsl_bash_path(&shell, system_root) {
            CommandTransport::Stdin
        } else {
            CommandTransport::Argument
        };
    ShellLaunch {
        configuration: ShellConfiguration {
            executable: shell,
            arguments: vec![match command_transport {
                CommandTransport::Argument => "-c".into(),
                CommandTransport::Stdin => "-s".into(),
            }],
            output_file_prefix: "pi-bash-".into(),
        },
        command_transport,
    }
}

fn is_legacy_wsl_bash_path(shell: &Path, system_root: Option<&OsStr>) -> bool {
    let shell = normalize_windows_path(&shell.to_string_lossy());
    if let Some(system_root) = system_root {
        let root = normalize_windows_path(&system_root.to_string_lossy());
        if !is_absolute_windows_path(&root) {
            return false;
        }
        return ["system32", "sysnative"]
            .into_iter()
            .any(|directory| shell == format!("{root}\\{directory}\\bash.exe"));
    }
    let components = shell.split('\\').collect::<Vec<_>>();
    components.len() == 4
        && components[0].ends_with(':')
        && components[1] == "windows"
        && matches!(components[2], "system32" | "sysnative")
        && components[3] == "bash.exe"
}

fn normalize_windows_path(path: &str) -> String {
    let replaced = path.replace('/', "\\").to_lowercase();
    let mut components = Vec::new();
    for component in replaced.split('\\') {
        match component {
            "" | "." => {}
            ".." if components.len() > 1 => {
                components.pop();
            }
            _ => components.push(component),
        }
    }
    components.join("\\")
}

fn is_absolute_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && path.as_bytes().get(2) == Some(&b'\\')
}

fn shell_path_exists(path: &Path) -> bool {
    if cfg!(windows) {
        // App Execution Aliases are runnable reparse points. Checking the
        // entry itself avoids the extra regular-file stat that rejects them.
        std::fs::symlink_metadata(path).is_ok()
    } else {
        // Follow symlinks on Unix, matching access(F_OK), including rejection
        // of a dangling configured-shell symlink.
        std::fs::metadata(path).is_ok()
    }
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        let where_executable = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .map(|root| root.join("System32").join("where.exe"))
            .filter(|path| shell_path_exists(path))
            .unwrap_or_else(|| PathBuf::from("where"));
        let output = std::process::Command::new(where_executable)
            .arg(name)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let decoded = String::from_utf8_lossy(&output.stdout);
        let first = decoded
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let candidate = PathBuf::from(first);
        return shell_path_exists(&candidate).then_some(candidate);
    }

    let output = std::process::Command::new("which")
        .arg(name)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let decoded = String::from_utf8_lossy(&output.stdout);
    decoded
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

fn format_io_error(error: &std::io::Error) -> String {
    let label = match error.kind() {
        std::io::ErrorKind::NotFound => Some("ENOENT"),
        std::io::ErrorKind::PermissionDenied => Some("EACCES"),
        _ => None,
    };
    label.map_or_else(|| error.to_string(), |label| format!("{label}: {error}"))
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
    decoders: &mut [ChunkDecoder; 2],
    sink: Option<&ShellChunkSink>,
    stream: ShellStream,
    bytes: &[u8],
) -> Result<(), ShellError> {
    capture.received_bytes = capture.received_bytes.saturating_add(bytes.len());
    let decoder = match stream {
        ShellStream::Stdout => &mut decoders[0],
        ShellStream::Stderr => &mut decoders[1],
    };
    let sanitized = decoder.sanitizer.is_some();
    if !sanitized {
        capture.push(bytes).await?;
    }
    if sanitized || sink.is_some() {
        let text = decoder.push(bytes);
        append_decoded_chunk(capture, sanitized, sink, stream, text).await?;
    }
    Ok(())
}

async fn append_decoded_chunk(
    capture: &mut OutputCapture,
    sanitized: bool,
    sink: Option<&ShellChunkSink>,
    stream: ShellStream,
    text: String,
) -> Result<(), ShellError> {
    if text.is_empty() {
        return Ok(());
    }
    if sanitized {
        // executeBash uses received bytes to decide when to spill, but stores
        // only sanitized text. A spill alone does not imply display truncation.
        if capture.received_bytes > MAX_OUTPUT_BYTES && capture.full_output.is_none() {
            capture.start_persisting().await?;
        }
        capture.push(text.as_bytes()).await?;
    }
    if let Some(sink) = sink {
        sink(ShellChunk { stream, text });
    }
    Ok(())
}

async fn finish_chunk_stream(
    capture: &mut OutputCapture,
    decoders: &mut [ChunkDecoder; 2],
    sink: Option<&ShellChunkSink>,
) -> Result<(), ShellError> {
    for (stream, decoder) in [ShellStream::Stdout, ShellStream::Stderr]
        .into_iter()
        .zip(decoders)
    {
        let text = decoder.finish();
        append_decoded_chunk(capture, decoder.sanitizer.is_some(), sink, stream, text).await?;
    }
    Ok(())
}

struct ChunkDecoder {
    utf8: Utf8ChunkDecoder,
    sanitizer: Option<user_output::UserOutputSanitizer>,
}

impl ChunkDecoder {
    fn new(policy: CapturePolicy) -> Self {
        Self {
            utf8: Utf8ChunkDecoder::default(),
            sanitizer: match policy {
                CapturePolicy::Raw => None,
                CapturePolicy::UserShell => Some(user_output::UserOutputSanitizer::default()),
            },
        }
    }

    fn push(&mut self, bytes: &[u8]) -> String {
        let text = self.utf8.push(bytes);
        match self.sanitizer.as_mut() {
            Some(sanitizer) => sanitizer.push(text),
            None => text,
        }
    }

    fn finish(&mut self) -> String {
        let text = self.utf8.finish();
        match self.sanitizer.as_mut() {
            Some(sanitizer) => sanitizer.finish(text),
            None => text,
        }
    }
}

#[derive(Default)]
struct Utf8ChunkDecoder {
    pending: Vec<u8>,
}

impl Utf8ChunkDecoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        let mut consumed = 0;
        loop {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    output.push_str(text);
                    consumed = self.pending.len();
                    break;
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    // SAFETY: `valid_up_to` identifies a validated UTF-8 prefix.
                    output.push_str(
                        std::str::from_utf8(&self.pending[consumed..valid_end])
                            .expect("validated UTF-8 prefix"),
                    );
                    consumed = valid_end;
                    let Some(invalid_length) = error.error_len() else {
                        break;
                    };
                    output.push('\u{fffd}');
                    consumed += invalid_length;
                }
            }
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        output
    }

    fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned()
    }
}

#[derive(Default)]
struct OutputCapture {
    tail: TailCapture,
    buffered: Vec<u8>,
    full_output: Option<tokio::fs::File>,
    full_output_path: Option<PathBuf>,
    received_bytes: usize,
    total_bytes: usize,
    newline_count: usize,
    last_byte: Option<u8>,
    last_line_bytes: usize,
    output_file_prefix: String,
}

impl OutputCapture {
    fn new(policy: CapturePolicy, output_file_prefix: String) -> Self {
        Self {
            tail: TailCapture {
                policy,
                ..Default::default()
            },
            output_file_prefix,
            ..Default::default()
        }
    }

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
        let bytes = self.tail.bytes();
        let start = bytes
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[start..]).into_owned()
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

struct TailChunk {
    bytes: Vec<u8>,
    units: usize,
}

#[derive(Default)]
struct TailCapture {
    chunks: VecDeque<TailChunk>,
    bytes: usize,
    units: usize,
    policy: CapturePolicy,
}

impl TailCapture {
    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let kept = &bytes[bytes.len().saturating_sub(CAPTURE_TAIL_BYTES)..];
        let units = match self.policy {
            CapturePolicy::Raw => {
                // Preserve built-in tools' byte-bounded rolling capture.
                if bytes.len() >= CAPTURE_TAIL_BYTES {
                    self.chunks.clear();
                    self.bytes = 0;
                    self.units = 0;
                }
                kept.len()
            }
            CapturePolicy::UserShell => {
                // executeBash's rolling buffer uses string.length (UTF-16),
                // while the final model-facing truncation is byte-bounded.
                std::str::from_utf8(bytes)
                    .expect("decoded user-shell text")
                    .encode_utf16()
                    .count()
            }
        };
        self.chunks.push_back(TailChunk {
            bytes: kept.to_vec(),
            units,
        });
        self.bytes += kept.len();
        self.units += units;
        while self.units > CAPTURE_TAIL_BYTES && self.chunks.len() > 1 {
            if let Some(removed) = self.chunks.pop_front() {
                self.bytes -= removed.bytes.len();
                self.units -= removed.units;
            }
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.bytes);
        for chunk in &self.chunks {
            bytes.extend_from_slice(&chunk.bytes);
        }
        bytes
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
        let separator_bytes = usize::from(!kept.is_empty());
        let line_bytes = line.len() + separator_bytes;
        if bytes.saturating_add(line_bytes) > MAX_OUTPUT_BYTES {
            truncated_by = TruncatedBy::Bytes;
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(bytes + separator_bytes);
            if kept.is_empty()
                || (!line.trim().is_empty()
                    && kept.iter().all(|line: &String| line.trim().is_empty())
                    && remaining > 0)
            {
                let partial = truncate_utf8_from_end(line, remaining);
                bytes += partial.len() + separator_bytes;
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

    // The result shape differs from core/exec.ts, but these observable process
    // behaviors are shared by both executors.
    #[tokio::test]
    async fn silent_exit_one_has_no_synthetic_output_or_failure_flags() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_chunks = Arc::clone(&chunks);
        let result = execute(ShellRequest {
            command: "exit 1".into(),
            cwd: directory.path().into(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: Some(Arc::new(move |chunk| {
                callback_chunks.lock().unwrap().push(chunk);
            })),
        })
        .await
        .unwrap();

        assert_eq!(result.output, "");
        assert_eq!(result.exit_code, Some(1));
        assert!(!result.cancelled);
        assert!(!result.timed_out);
        assert!(!result.truncated);
        assert!(result.truncation.is_none());
        assert!(result.full_output_path.is_none());
        assert!(chunks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn normal_eof_flushes_incomplete_utf8_independently_on_both_streams() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_chunks = Arc::clone(&chunks);
        let result = execute(ShellRequest {
            command: "printf '\\342'; printf '\\360' >&2".into(),
            cwd: directory.path().into(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: Some(Arc::new(move |chunk| {
                callback_chunks.lock().unwrap().push(chunk);
            })),
        })
        .await
        .unwrap();

        assert_eq!(result.output, "��");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert!(!result.timed_out);
        assert!(!result.truncated);
        assert!(result.full_output_path.is_none());
        let chunks = chunks.lock().unwrap();
        for stream in [ShellStream::Stdout, ShellStream::Stderr] {
            let text = chunks
                .iter()
                .filter(|chunk| chunk.stream == stream)
                .map(|chunk| chunk.text.as_str())
                .collect::<String>();
            assert_eq!(text, "�", "{stream:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn external_sigkill_reports_137_without_cancellation_or_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let result = execute(ShellRequest {
            // The command, not the executor's abort/timeout path, sends SIGKILL.
            command: "kill -KILL $$".into(),
            cwd: directory.path().into(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        })
        .await
        .unwrap();

        assert_eq!(result.exit_code, Some(137));
        assert_eq!(result.output, "");
        assert!(!result.cancelled);
        assert!(!result.timed_out);
        assert!(!result.truncated);
        assert!(result.truncation.is_none());
        assert!(result.full_output_path.is_none());
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

    #[cfg(windows)]
    fn bash_single_quoted_path(path: &Path) -> String {
        format!(
            "'{}'",
            path.to_string_lossy()
                .replace('\\', "/")
                .replace('\'', "'\"'\"'")
        )
    }

    #[cfg(windows)]
    fn cleanup_windows_process(pid_file: &Path) {
        use std::os::windows::process::CommandExt;

        let Ok(pid) = std::fs::read_to_string(pid_file) else {
            return;
        };
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let _ = std::process::Command::new(Path::new(&root).join("System32").join("taskkill.exe"))
            .args(["/F", "/T", "/PID", pid.trim()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .status();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_quiet_inherited_pipes_release_after_the_shell_exits() {
        let Some(node) = executable_on_path("node.exe") else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("spawn-inherited-child.js");
        let pid_file = directory.path().join("descendant.pid");
        std::fs::write(
            &script,
            r#"const fs = require("fs");
const { spawn } = require("child_process");
const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)"], { stdio: "inherit", detached: true });
fs.writeFileSync(process.argv[2], String(child.pid));
child.unref();
console.log("child-exiting");
"#,
        )
        .unwrap();
        let command = format!(
            "{} {} {}",
            bash_single_quoted_path(&node),
            bash_single_quoted_path(&script),
            bash_single_quoted_path(&pid_file)
        );
        let (_, signal) = AbortHandle::new();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            execute_user_shell(ShellRequest {
                command,
                cwd: directory.path().into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            }),
        )
        .await;
        cleanup_windows_process(&pid_file);
        let result = result
            .expect("quiet inherited handles must not keep execution open")
            .unwrap();
        assert!(result.output.contains("child-exiting"));
        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(windows)]
    fn windows_process_exists(pid: u32) -> bool {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let output =
            std::process::Command::new(Path::new(&root).join("System32").join("tasklist.exe"))
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_abort_kills_descendant_processes() {
        let Some(node) = executable_on_path("node.exe") else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("wait-with-child.js");
        let pid_file = directory.path().join("descendant.pid");
        std::fs::write(
            &script,
            r#"const fs = require("fs");
const { spawn } = require("child_process");
const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)"], { stdio: "ignore" });
fs.writeFileSync(process.argv[2], String(child.pid));
setInterval(() => {}, 60000);
"#,
        )
        .unwrap();
        let command = format!(
            "{} {} {}",
            bash_single_quoted_path(&node),
            bash_single_quoted_path(&script),
            bash_single_quoted_path(&pid_file)
        );
        let (abort, signal) = AbortHandle::new();
        let running = tokio::spawn(execute(ShellRequest {
            command,
            cwd: directory.path().into(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        }));
        for _ in 0..300 {
            if pid_file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(&pid_file)
            .expect("fixture must write descendant pid")
            .parse::<u32>()
            .unwrap();
        abort.abort();
        assert!(running.await.unwrap().unwrap().cancelled);
        for _ in 0..100 {
            if !windows_process_exists(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cleanup_windows_process(&pid_file);
        panic!("descendant process {pid} survived shell cancellation");
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

    #[test]
    fn overlong_line_before_blank_lines_keeps_its_tail() {
        for ending in ["\n", "\r\n"] {
            let long_line = format!("{}END", "y".repeat(219_000));
            let input = format!("first line{ending}{long_line}{ending}{ending}");
            let truncated = truncate_tail(&input, lines_for_counting(&input).len(), input.len(), 0);

            assert!(truncated.last_line_partial);
            assert!(truncated.content.starts_with('y'));
            assert!(truncated.content.contains(&format!("END{ending}")));
            assert_eq!(truncated.output_bytes, MAX_OUTPUT_BYTES);
        }
    }

    #[test]
    fn streaming_decoder_joins_split_utf8_and_flushes_incomplete_eof() {
        let mut decoder = Utf8ChunkDecoder::default();
        let euro = "€".as_bytes();
        assert_eq!(decoder.push(&euro[..1]), "");
        assert_eq!(decoder.push(&euro[1..]), "€");
        assert_eq!(decoder.finish(), "");

        assert_eq!(decoder.push(&[0xe2]), "");
        assert_eq!(decoder.finish(), "�");
    }

    #[tokio::test]
    async fn streaming_decoders_keep_interleaved_stdout_and_stderr_independent() {
        let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_chunks = chunks.clone();
        let sink: ShellChunkSink = Arc::new(move |chunk| {
            callback_chunks
                .lock()
                .unwrap()
                .push((chunk.stream, chunk.text));
        });
        let mut capture = OutputCapture::default();
        let mut decoders = [
            ChunkDecoder::new(CapturePolicy::Raw),
            ChunkDecoder::new(CapturePolicy::Raw),
        ];
        let euro = "€".as_bytes();
        record_chunk(
            &mut capture,
            &mut decoders,
            Some(&sink),
            ShellStream::Stdout,
            &euro[..1],
        )
        .await
        .unwrap();
        record_chunk(
            &mut capture,
            &mut decoders,
            Some(&sink),
            ShellStream::Stderr,
            b"err",
        )
        .await
        .unwrap();
        record_chunk(
            &mut capture,
            &mut decoders,
            Some(&sink),
            ShellStream::Stdout,
            &euro[1..],
        )
        .await
        .unwrap();
        record_chunk(
            &mut capture,
            &mut decoders,
            Some(&sink),
            ShellStream::Stderr,
            &[0xf0, 0x9f],
        )
        .await
        .unwrap();
        finish_chunk_stream(&mut capture, &mut decoders, Some(&sink))
            .await
            .unwrap();

        assert_eq!(
            *chunks.lock().unwrap(),
            vec![
                (ShellStream::Stderr, "err".to_string()),
                (ShellStream::Stdout, "€".to_string()),
                (ShellStream::Stderr, "�".to_string()),
            ]
        );
    }

    #[test]
    fn unix_shell_resolution_prefers_bin_then_path_and_rejects_missing_custom_path() {
        let standard = resolve_shell_with(
            None,
            ShellPlatform::Unix,
            None,
            None,
            None,
            |path| path == Path::new("/bin/bash"),
            |_| Some("/usr/local/bin/bash".into()),
        )
        .unwrap();
        assert_eq!(standard.configuration.executable, Path::new("/bin/bash"));

        let fallback = resolve_shell_with(
            None,
            ShellPlatform::Unix,
            None,
            None,
            None,
            |_| false,
            |name| (name == "bash").then(|| "/usr/local/bin/bash".into()),
        )
        .unwrap();
        assert_eq!(
            fallback.configuration.executable,
            Path::new("/usr/local/bin/bash")
        );

        let error = resolve_shell_with(
            Some("/missing/bash".into()),
            ShellPlatform::Unix,
            None,
            None,
            None,
            |_| false,
            |_| None,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to spawn shell: Custom shell path not found: /missing/bash"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_existence_rejects_a_dangling_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let alias = directory.path().join("bash");
        std::os::unix::fs::symlink(directory.path().join("missing"), &alias).unwrap();
        assert!(!shell_path_exists(&alias));
    }

    #[test]
    fn windows_shell_resolution_prefers_git_and_accepts_path_aliases() {
        let git = PathBuf::from(r"D:\Applications")
            .join("Git")
            .join("bin")
            .join("bash.exe");
        let launch = resolve_shell_with(
            None,
            ShellPlatform::Windows,
            Some(r"D:\Applications".into()),
            Some(r"D:\Applications (x86)".into()),
            Some(r"D:\WinNT".into()),
            |path| path == git,
            |_| Some(r"D:\Users\test\WindowsApps\bash.exe".into()),
        )
        .unwrap();
        assert_eq!(launch.configuration.executable, git);
        assert_eq!(launch.command_transport, CommandTransport::Argument);

        let alias = PathBuf::from(r"D:\Users\test\WindowsApps\bash.exe");
        let launch = resolve_shell_with(
            None,
            ShellPlatform::Windows,
            None,
            None,
            Some(r"D:\WinNT".into()),
            |path| path == alias,
            |name| (name == "bash.exe").then(|| alias.clone()),
        )
        .unwrap();
        assert_eq!(launch.configuration.executable, alias);

        let explicit = resolve_shell_with(
            Some(alias.clone()),
            ShellPlatform::Windows,
            None,
            None,
            Some(r"D:\WinNT".into()),
            |path| path == alias,
            |_| None,
        )
        .unwrap();
        assert_eq!(explicit.configuration.executable, alias);

        let rejected = resolve_shell_with(
            None,
            ShellPlatform::Windows,
            None,
            None,
            Some(r"D:\WinNT".into()),
            |_| false,
            |name| (name == "bash.exe").then(|| PathBuf::from("missing-bash.exe")),
        )
        .unwrap_err();
        assert!(rejected.to_string().contains("No bash shell found"));
    }

    #[test]
    fn legacy_wsl_shell_uses_stdin_transport_for_actual_system_root_only() {
        for (system_root, shell, expected) in [
            (Some(r"D:\WinNT\"), r"d:/WINNT/System32/BASH.EXE", true),
            (Some(r"D:\WinNT"), r"D:\WinNT\Sysnative\bash.exe", true),
            (None, r"E:\Windows\System32\bash.exe", true),
            (Some(r"D:\WinNT"), r"D:\Git\bin\bash.exe", false),
            (Some(r"D:\WinNT"), r"E:\Windows\System32\bash.exe", false),
            (Some("WinNT"), r"C:\Windows\System32\bash.exe", false),
        ] {
            let actual = is_legacy_wsl_bash_path(Path::new(shell), system_root.map(OsStr::new));
            assert_eq!(actual, expected, "{system_root:?} / {shell}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_transport_writes_the_complete_command() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let result = execute_launch(
            ShellRequest {
                command: "value=prefix\nprintf '%s' \"$value-command\"".into(),
                cwd: directory.path().into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            },
            BTreeMap::new(),
            ShellLaunch {
                configuration: ShellConfiguration {
                    executable: "/bin/sh".into(),
                    arguments: vec!["-s".into()],
                    output_file_prefix: "pi-stdin-shell-".into(),
                },
                command_transport: CommandTransport::Stdin,
            },
            CapturePolicy::Raw,
        )
        .await
        .unwrap();

        assert_eq!(result.output, "prefix-command");
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn missing_working_directory_reports_actionable_failure() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let (_, signal) = AbortHandle::new();
        let error = execute(ShellRequest {
            command: "echo unreachable".into(),
            cwd: missing.clone(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: None,
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains(&format!(
            "Working directory does not exist: {}",
            missing.display()
        )));
        assert!(error.to_string().contains("Cannot execute shell commands."));
    }

    #[tokio::test]
    async fn configured_spawn_error_includes_errno_name() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let error = execute_configured(
            ShellRequest {
                command: "echo unreachable".into(),
                cwd: directory.path().into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            },
            BTreeMap::new(),
            ShellConfiguration {
                executable: directory.path().join("missing-shell"),
                arguments: vec!["-c".into()],
                output_file_prefix: "pi-missing-".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("ENOENT"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn abort_kills_the_shell_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("descendant.pid");
        let (abort, signal) = AbortHandle::new();
        let command = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; wait",
            pid_file.display()
        );
        let running = tokio::spawn(execute(ShellRequest {
            command,
            cwd: directory.path().into(),
            timeout: None,
            shell_path: Some("/bin/sh".into()),
            abort_signal: signal,
            on_chunk: None,
        }));
        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        abort.abort();
        assert!(running.await.unwrap().unwrap().cancelled);
        for _ in 0..100 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("descendant process {pid} survived shell cancellation");
    }
}
