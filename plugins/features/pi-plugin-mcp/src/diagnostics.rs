//! Private, best-effort server diagnostics. Never write to stdout/stderr or session journals.
#![allow(deprecated)] // Logging is part of the pinned stable MCP contract.

mod stderr;
#[cfg(test)]
mod tests;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use rmcp::model::LoggingMessageNotificationParam;
use time::OffsetDateTime;

use crate::{McpOAuthConfig, McpTransport};
pub(crate) use stderr::StderrCapture;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const REDACTED: &[u8] = b"[REDACTED]";
// Serialize local append/rotation transactions even across separate library instances.
static LOG_WRITE: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
pub(crate) struct Redactor(Arc<Vec<Vec<u8>>>);

impl Redactor {
    pub(crate) fn new(transport: &McpTransport, oauth: Option<&McpOAuthConfig>) -> Self {
        let mut values = Vec::new();
        match transport {
            McpTransport::Stdio { args, env, .. } => {
                values.extend(env.values().cloned());
                for arg in args {
                    values.push(arg.clone());
                    if let Some((_, value)) = arg.split_once('=') {
                        values.push(value.to_string());
                    }
                }
                // Inherited credentials can also be echoed by a failed child.
                values.extend(std::env::vars_os().filter_map(|(key, value)| {
                    let key = key.to_string_lossy().to_ascii_lowercase();
                    [
                        "token",
                        "secret",
                        "password",
                        "credential",
                        "cookie",
                        "auth",
                        "key",
                    ]
                    .iter()
                    .any(|part| key.contains(part))
                    .then(|| value.to_string_lossy().into_owned())
                }));
            }
            McpTransport::Http { url, headers } => {
                values.push(url.clone());
                for value in headers.values() {
                    values.push(value.clone());
                    if let Some((_, token)) = value.split_once(' ') {
                        values.push(token.to_string());
                    }
                }
            }
        }
        if let Some(secret) = oauth.and_then(|oauth| oauth.client_secret.as_ref()) {
            values.push(secret.clone());
        }
        Self::from_values(values)
    }

    fn from_values(values: Vec<String>) -> Self {
        let mut values = values
            .into_iter()
            .filter(|value| !value.is_empty())
            .map(String::into_bytes)
            .collect::<Vec<_>>();
        // Prefer complete, longer secrets over a prefix secret.
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        Self(Arc::new(values))
    }

    /// Consume only a safe prefix; a secret split across reads is kept pending.
    fn consume(&self, input: &[u8], eof: bool, mut output: impl FnMut(&[u8])) -> usize {
        let mut offset = 0;
        while offset < input.len() {
            let remaining = &input[offset..];
            let mut matched = false;
            for secret in self.0.iter() {
                if !eof && secret.starts_with(remaining) && remaining.len() < secret.len() {
                    return offset;
                }
                if remaining.starts_with(secret) {
                    output(REDACTED);
                    offset += secret.len();
                    matched = true;
                    break;
                }
            }
            if !matched {
                output(&input[offset..offset + 1]);
                offset += 1;
            }
        }
        offset
    }

    pub(crate) fn text(&self, input: &str) -> String {
        let mut output = Vec::new();
        self.consume(input.as_bytes(), true, |bytes| {
            output.extend_from_slice(bytes)
        });
        clean_text(&String::from_utf8_lossy(&output))
    }
}

fn clean_text(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
}

/// Optional file sink: transport-only / ACP clients never discover an agent directory.
#[derive(Clone)]
pub(crate) struct ServerLog {
    path: PathBuf,
    server: String,
    redactor: Redactor,
}

impl ServerLog {
    pub(crate) fn new(path: PathBuf, server: String, redactor: Redactor) -> Self {
        Self {
            path,
            server,
            redactor,
        }
    }

    pub(crate) fn write(&self, params: LoggingMessageNotificationParam) {
        let line = format_message(&self.server, &params, OffsetDateTime::now_utc());
        // Diagnostics do not propagate errors, including rotation/permission/lock failures.
        let _ = append(&self.path, self.redactor.text(&line).as_bytes());
    }
}

fn format_message(
    server: &str,
    params: &LoggingMessageNotificationParam,
    now: OffsetDateTime,
) -> String {
    let timestamp = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    );
    let level = serde_json::to_value(params.level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "info".into());
    let logger = params
        .logger
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| format!(" {value}:"))
        .unwrap_or_default();
    let text = params
        .data
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| params.data.to_string())
        .replace("\r\n", "\n")
        .replace('\n', "\n    ");
    format!("{timestamp} [{server}] {level}{logger} {text}\n")
}

fn private_file(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Never append to a pre-existing symlink, device or directory.
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return Err(std::io::Error::other("not a regular log file"));
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

struct LogLock(std::fs::File);
impl Drop for LogLock {
    fn drop(&mut self) {
        // A concurrent fork may inherit the descriptor until exec. Closing our copy alone
        // would leave the lock alive there and spuriously drop other local writers' records.
        let _ = FileExt::unlock(&self.0);
    }
}

fn append(path: &Path, line: &[u8]) -> std::io::Result<()> {
    let _local = LOG_WRITE
        .lock()
        .map_err(|_| std::io::Error::other("log lock poisoned"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // A sibling lock serializes rotation against other processes. Never wait for a stuck writer.
    let lock = private_file(&path.with_extension("log.lock"), false)?;
    lock.try_lock_exclusive()?;
    let _lock = LogLock(lock);
    // Tighten permissions of pre-existing output too, before it can become the rotated file.
    let file = private_file(path, true)?;
    if file.metadata()?.len() > MAX_LOG_BYTES {
        let rotated = path.with_extension("log.1");
        // Windows rename cannot replace a file. The transaction is protected by the lock.
        if rotated.exists() {
            fs::remove_file(&rotated)?;
        }
        fs::rename(path, rotated)?;
    }
    private_file(path, true)?.write_all(line)
}
