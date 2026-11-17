use std::time::Duration;

use pi_core::AbortSignal;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveError {
    #[error("request aborted while resolving configuration")]
    Aborted,
    #[error("{0}")]
    Failed(String),
}

/// Request-time resolution. Commands may return rotating credentials, so neither
/// successful output nor failures can be cached across calls (matching current Pi).
#[derive(Default)]
pub(crate) struct ConfigValueResolver {}

impl ConfigValueResolver {
    pub async fn resolve(
        &self,
        configured: &str,
        description: &str,
        signal: &AbortSignal,
    ) -> Result<String, ResolveError> {
        if let Some(command) = configured.strip_prefix('!') {
            return self.resolve_command(command, description, signal).await;
        }
        resolve_template(configured).map_err(|name| {
            ResolveError::Failed(format!(
                "failed to resolve {description} from environment variable {name}"
            ))
        })
    }

    async fn resolve_command(
        &self,
        command: &str,
        description: &str,
        signal: &AbortSignal,
    ) -> Result<String, ResolveError> {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut process = tokio::process::Command::new(shell);
        process
            .arg("-c")
            .arg(command)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let result = tokio::select! {
            _ = signal.wait() => return Err(ResolveError::Aborted),
            result = tokio::time::timeout(COMMAND_TIMEOUT, process.output()) => result,
        };
        let resolved = match result {
            Ok(Ok(output)) if output.status.success() => {
                let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (!value.is_empty()).then_some(value)
            }
            _ => None,
        };
        resolved.ok_or_else(|| {
            // Commands can embed credentials, and helper output can contain secrets.
            // Identify the configuration field without echoing any of those values.
            ResolveError::Failed(format!(
                "failed to resolve {description} from shell command"
            ))
        })
    }
}

fn resolve_template(configured: &str) -> Result<String, String> {
    let characters = configured.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '$' {
            output.push(characters[index]);
            index += 1;
            continue;
        }
        let Some(next) = characters.get(index + 1).copied() else {
            output.push('$');
            break;
        };
        if matches!(next, '$' | '!') {
            output.push(next);
            index += 2;
            continue;
        }
        if next == '{' {
            let Some(relative_end) = characters[index + 2..]
                .iter()
                .position(|character| *character == '}')
            else {
                output.push('$');
                index += 1;
                continue;
            };
            let end = index + 2 + relative_end;
            let name = characters[index + 2..end].iter().collect::<String>();
            if valid_env_name(&name) {
                output.push_str(&environment_value(&name)?);
            } else {
                output.extend(characters[index..=end].iter());
            }
            index = end + 1;
            continue;
        }
        if next == '_' || next.is_ascii_alphabetic() {
            let mut end = index + 2;
            while end < characters.len()
                && (characters[end] == '_' || characters[end].is_ascii_alphanumeric())
            {
                end += 1;
            }
            let name = characters[index + 1..end].iter().collect::<String>();
            output.push_str(&environment_value(&name)?);
            index = end;
            continue;
        }
        output.push('$');
        index += 1;
    }
    Ok(output)
}

fn valid_env_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn environment_value(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn command_failure_does_not_disclose_command_or_output() {
        let resolver = ConfigValueResolver::default();
        let (_, signal) = pi_core::AbortHandle::new();
        let error = resolver
            .resolve(
                "!printf 'stdout-secret'; printf 'stderr-secret' >&2; exit 7",
                "API key for provider fixture",
                &signal,
            )
            .await
            .unwrap_err();
        let displayed = format!("{error} {error:?}");
        assert!(displayed.contains("API key for provider fixture"));
        assert!(!displayed.contains("stdout-secret"));
        assert!(!displayed.contains("stderr-secret"));
        assert!(!displayed.contains("printf"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_empty_output_is_rejected() {
        let resolver = ConfigValueResolver::default();
        let (_, signal) = pi_core::AbortHandle::new();
        assert!(matches!(
            resolver
                .resolve("!printf '  \\n'", "API key", &signal)
                .await,
            Err(ResolveError::Failed(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_resolution_remains_abortable() {
        let resolver = ConfigValueResolver::default();
        let (handle, signal) = pi_core::AbortHandle::new();
        let resolution = resolver.resolve("!exec sleep 30", "API key", &signal);
        let abort = async {
            tokio::task::yield_now().await;
            handle.abort();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(resolution, abort)
        })
        .await
        .expect("cancellation must not wait for the command timeout");
        assert!(matches!(result, Err(ResolveError::Aborted)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_resolution_retains_the_timeout() {
        let resolver = ConfigValueResolver::default();
        let (_, signal) = pi_core::AbortHandle::new();
        let result = tokio::time::timeout(
            COMMAND_TIMEOUT + Duration::from_secs(5),
            resolver.resolve("!exec sleep 30", "API key", &signal),
        )
        .await
        .expect("credential helpers must have a bounded runtime");
        assert!(matches!(result, Err(ResolveError::Failed(_))));
    }

    #[test]
    fn templates_support_pi_escapes_without_requiring_environment_mutation() {
        assert_eq!(
            resolve_template("price=$$5 and $!literal"),
            Ok("price=$5 and !literal".into())
        );
        assert_eq!(
            resolve_template(concat!("$", "{not-valid}")),
            Ok(concat!("$", "{not-valid}").into())
        );
    }
}
