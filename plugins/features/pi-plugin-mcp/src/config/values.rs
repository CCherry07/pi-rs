//! Local configuration values. Syntax is pure; shell execution requires explicit library opt-in.
use std::path::{Path, PathBuf};
use std::time::Duration;

use pi_core::AbortSignal;
use pi_shell::{ShellCaptureRequest, ShellCaptureStatus};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_BYTES: usize = 1024 * 1024;

/// Private capability attached only to trust-filtered local-library connections.
#[derive(Clone)]
pub(crate) struct LocalValues {
    pub cwd: PathBuf,
    pub allow_commands: bool,
}

impl LocalValues {
    pub async fn oauth(
        &self,
        mut settings: crate::McpOAuthConfig,
        signal: AbortSignal,
    ) -> Result<crate::McpOAuthConfig, String> {
        if let Some(secret) = settings.client_secret.as_mut() {
            *secret = resolve(secret, &self.cwd, self.allow_commands, signal).await?;
        }
        Ok(settings)
    }
}

fn name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn name_byte(byte: u8) -> bool {
    name_start(byte) || byte.is_ascii_digit()
}

/// Pi templates plus the existing Rust `${env:NAME}` spelling. Invalid references are literal.
/// Leading command markers are opaque here: parsing/transport-only callers never execute them.
pub(super) fn expand(
    value: &str,
    lookup: impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    if value.starts_with('!') {
        return Ok(value.into());
    }
    let mut result = String::new();
    let mut rest = value;
    while let Some((before, after)) = rest.split_once('$') {
        result.push_str(before);
        if let Some(after) = after.strip_prefix('$') {
            result.push('$');
            rest = after;
            continue;
        }
        if let Some(after) = after.strip_prefix('!') {
            result.push('!');
            rest = after;
            continue;
        }
        let reference = if let Some(braced) = after.strip_prefix('{') {
            braced.split_once('}').and_then(|(raw, tail)| {
                let key = raw.strip_prefix("env:").unwrap_or(raw);
                let bytes = key.as_bytes();
                (!bytes.is_empty() && name_start(bytes[0]) && bytes.iter().copied().all(name_byte))
                    .then_some((key, tail))
            })
        } else if after.as_bytes().first().copied().is_some_and(name_start) {
            let length = after.bytes().take_while(|byte| name_byte(*byte)).count();
            Some((&after[..length], &after[length..]))
        } else {
            None
        };
        if let Some((key, tail)) = reference {
            let expanded = lookup(key).map_err(|_| {
                "MCP configuration environment value is missing or not UTF-8".to_string()
            })?;
            if expanded.is_empty() {
                return Err("MCP configuration environment value is empty".into());
            }
            result.push_str(&expanded);
            rest = tail;
        } else {
            result.push('$');
            // Consume an invalid braced reference as one literal, including any dollar inside it.
            if let Some((invalid, tail)) = after
                .strip_prefix('{')
                .and_then(|value| value.split_once('}'))
            {
                result.push('{');
                result.push_str(invalid);
                result.push('}');
                rest = tail;
            } else {
                rest = after;
            }
        }
    }
    result.push_str(rest);
    Ok(result)
}

pub(super) async fn resolve(
    value: &str,
    cwd: &Path,
    allow_commands: bool,
    signal: AbortSignal,
) -> Result<String, String> {
    if signal.is_aborted() {
        return Err("MCP configuration value resolution cancelled".into());
    }
    let Some(command) = value.strip_prefix('!') else {
        return expand(value, |key| std::env::var(key).map_err(|_| String::new()));
    };
    if !allow_commands {
        return Err(
            "Command-backed MCP configuration requires explicit local-library opt-in".into(),
        );
    }
    let result = pi_shell::capture(ShellCaptureRequest {
        command: command.into(),
        cwd: cwd.into(),
        // Pi uses the default POSIX shell for config helpers; Windows uses its shell adapter.
        shell_path: if cfg!(unix) {
            Some("/bin/sh".into())
        } else {
            None
        },
        timeout: COMMAND_TIMEOUT,
        signal,
        stdout_limit: COMMAND_BYTES,
        stderr_limit: COMMAND_BYTES,
    })
    .await
    .map_err(|_| "MCP configuration command could not run".to_string())?;
    match result.status {
        ShellCaptureStatus::Cancelled => return Err("MCP configuration command cancelled".into()),
        ShellCaptureStatus::TimedOut => return Err("MCP configuration command timed out".into()),
        ShellCaptureStatus::OutputLimit => {
            return Err("MCP configuration command output exceeded its byte limit".into());
        }
        ShellCaptureStatus::Exited => {}
    }
    if result.exit_code != Some(0) {
        return Err("MCP configuration command failed".into());
    }
    let text = String::from_utf8(result.stdout)
        .map_err(|_| "MCP configuration command output is not UTF-8".to_string())?;
    let text = text.trim();
    if text.is_empty() {
        return Err("MCP configuration command output is empty".into());
    }
    // Do not rescan either shell output or substituted environment values for references/commands.
    Ok(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_syntax_matches_pi_and_preserves_the_rust_env_spelling() {
        for (input, expected) in [
            ("literal", "literal"),
            ("$TOKEN", "value"),
            ("before:$TOKEN/end", "before:value/end"),
            ("${TOKEN}-${env:TOKEN}", "value-value"),
            ("$$TOKEN", "$TOKEN"),
            ("$!echo $TOKEN", "!echo value"),
            ("${1INVALID}", "${1INVALID}"),
            ("${bad-$TOKEN}", "${bad-$TOKEN}"),
            ("${UNCLOSED", "${UNCLOSED"),
            ("$(do-not-execute)", "$(do-not-execute)"),
            ("!echo $TOKEN", "!echo $TOKEN"),
            ("東京🙂$TOKEN尾", "東京🙂value尾"),
            ("$0 $$ $", "$0 $ $"),
        ] {
            assert_eq!(
                expand(input, |key| {
                    assert_eq!(key, "TOKEN");
                    Ok("value".into())
                })
                .unwrap(),
                expected,
                "{input}"
            );
        }
        assert_eq!(
            expand("$TOKEN", |_| Ok("!echo $OTHER".into())).unwrap(),
            "!echo $OTHER"
        );
        for result in [Ok(String::new()), Err("private lookup failure".into())] {
            let error = expand("prefix $TOKEN", |_| result.clone()).unwrap_err();
            assert!(!error.contains("private"));
        }
    }
}
