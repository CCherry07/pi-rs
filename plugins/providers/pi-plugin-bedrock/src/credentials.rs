use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_core::{AbortSignal, ProviderError};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::sigv4::AwsCredentials;

const CLI_TIMEOUT: Duration = Duration::from_secs(20);
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub struct AwsCredentialSettings {
    pub values: BTreeMap<String, String>,
    pub prefer_profile: bool,
}

impl AwsCredentialSettings {
    pub fn from_environment(overrides: BTreeMap<String, String>) -> Self {
        let prefer_profile = overrides
            .get("AWS_PROFILE")
            .is_some_and(|profile| !profile.trim().is_empty());
        let mut values = BTreeMap::new();
        for name in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
            "AWS_SHARED_CREDENTIALS_FILE",
            "AWS_CONFIG_FILE",
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_ROLE_ARN",
        ] {
            if let Ok(value) = std::env::var(name)
                && !value.trim().is_empty()
            {
                values.insert(name.to_string(), value);
            }
        }
        values.extend(
            overrides
                .into_iter()
                .filter(|(_, value)| !value.trim().is_empty()),
        );
        Self {
            values,
            prefer_profile,
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn has_source(&self) -> bool {
        (self.get("AWS_ACCESS_KEY_ID").is_some() && self.get("AWS_SECRET_ACCESS_KEY").is_some())
            || self.get("AWS_PROFILE").is_some()
            || self.get("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_some()
            || self.get("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_some()
            || self.get("AWS_WEB_IDENTITY_TOKEN_FILE").is_some()
            || self
                .shared_credentials_path()
                .is_some_and(|path| path.is_file())
    }

    pub fn profile_region(&self) -> Result<Option<String>, ProviderError> {
        let path = self
            .get("AWS_CONFIG_FILE")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".aws/config")));
        let Some(path) = path else {
            return Ok(None);
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ProviderError::Failure(format!(
                    "failed to read AWS config file {}: {error}",
                    path.display()
                )));
            }
        };
        let section = if self.profile() == "default" {
            "default".to_string()
        } else {
            format!("profile {}", self.profile())
        };
        Ok(ini_section(&content, &section).get("region").cloned())
    }

    fn profile(&self) -> &str {
        self.get("AWS_PROFILE").unwrap_or("default")
    }

    fn shared_credentials_path(&self) -> Option<PathBuf> {
        self.get("AWS_SHARED_CREDENTIALS_FILE")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".aws/credentials")))
    }
}

#[derive(Clone)]
pub struct AwsCredentialResolver {
    settings: AwsCredentialSettings,
    cache: Arc<Mutex<Option<(AwsCredentials, Instant)>>>,
}

impl AwsCredentialResolver {
    pub fn new(settings: AwsCredentialSettings) -> Self {
        Self {
            settings,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn has_source(&self) -> bool {
        self.settings.has_source()
    }

    pub async fn resolve(&self, signal: &AbortSignal) -> Result<AwsCredentials, ProviderError> {
        if self.settings.prefer_profile
            && let Some(credentials) = self.resolve_static_profile()?
        {
            return Ok(credentials);
        }
        if let (Some(access_key_id), Some(secret_access_key)) = (
            self.settings.get("AWS_ACCESS_KEY_ID"),
            self.settings.get("AWS_SECRET_ACCESS_KEY"),
        ) {
            let credentials = AwsCredentials {
                access_key_id: access_key_id.to_string(),
                secret_access_key: secret_access_key.to_string(),
                session_token: self.settings.get("AWS_SESSION_TOKEN").map(str::to_string),
            };
            credentials.validate().map_err(ProviderError::Failure)?;
            return Ok(credentials);
        }
        if !self.settings.prefer_profile
            && let Some(credentials) = self.resolve_static_profile()?
        {
            return Ok(credentials);
        }

        let mut cache = self.cache.lock().await;
        if let Some((credentials, created)) = cache.as_ref()
            && created.elapsed() < CACHE_TTL
        {
            return Ok(credentials.clone());
        }
        let credentials = self.resolve_with_aws_cli(signal).await?;
        *cache = Some((credentials.clone(), Instant::now()));
        Ok(credentials)
    }

    fn resolve_static_profile(&self) -> Result<Option<AwsCredentials>, ProviderError> {
        let Some(path) = self.settings.shared_credentials_path() else {
            return Ok(None);
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ProviderError::Failure(format!(
                    "failed to read AWS credentials file {}: {error}",
                    path.display()
                )));
            }
        };
        let values = ini_section(&content, self.settings.profile());
        let (Some(access_key_id), Some(secret_access_key)) = (
            values.get("aws_access_key_id"),
            values.get("aws_secret_access_key"),
        ) else {
            return Ok(None);
        };
        let credentials = AwsCredentials {
            access_key_id: access_key_id.clone(),
            secret_access_key: secret_access_key.clone(),
            session_token: values.get("aws_session_token").cloned(),
        };
        credentials.validate().map_err(ProviderError::Failure)?;
        Ok(Some(credentials))
    }

    async fn resolve_with_aws_cli(
        &self,
        signal: &AbortSignal,
    ) -> Result<AwsCredentials, ProviderError> {
        validate_profile(self.settings.profile())?;
        let mut command = tokio::process::Command::new("aws");
        command.args(["configure", "export-credentials", "--format", "process"]);
        if self.settings.get("AWS_PROFILE").is_some() {
            command.args(["--profile", self.settings.profile()]);
        }
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        for (name, value) in &self.settings.values {
            command.env(name, value);
        }
        let result = tokio::select! {
            _ = signal.wait() => return Err(ProviderError::Aborted),
            result = tokio::time::timeout(CLI_TIMEOUT, command.output()) => result,
        };
        let output = match result {
            Ok(Ok(output)) if output.status.success() => output,
            Ok(Ok(output)) => {
                let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(ProviderError::Failure(format!(
                    "AWS credential chain failed: {}",
                    if message.is_empty() {
                        output.status.to_string()
                    } else {
                        truncate(&message, 2_000)
                    }
                )));
            }
            Ok(Err(error)) => {
                return Err(ProviderError::Failure(format!(
                    "failed to run AWS credential chain (install AWS CLI or configure static credentials): {error}"
                )));
            }
            Err(_) => {
                return Err(ProviderError::Failure(
                    "timed out resolving the AWS credential chain".to_string(),
                ));
            }
        };
        if output.stdout.len() > 256 * 1_024 {
            return Err(ProviderError::Failure(
                "AWS credential response is unexpectedly large".to_string(),
            ));
        }
        let value: ProcessCredentials =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                ProviderError::Failure(format!("invalid AWS credential response: {error}"))
            })?;
        let credentials = AwsCredentials {
            access_key_id: value.access_key_id,
            secret_access_key: value.secret_access_key,
            session_token: value.session_token,
        };
        credentials.validate().map_err(ProviderError::Failure)?;
        Ok(credentials)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProcessCredentials {
    access_key_id: String,
    secret_access_key: String,
    #[serde(default)]
    session_token: Option<String>,
}

fn ini_section(content: &str, target: &str) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    let mut selected = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            selected = line[1..line.len() - 1].trim() == target;
            continue;
        }
        if !selected || line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            output.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    output
}

fn validate_profile(profile: &str) -> Result<(), ProviderError> {
    if profile.is_empty() || profile.len() > 256 || profile.contains(['\r', '\n', '\0']) {
        return Err(ProviderError::Failure("invalid AWS profile".to_string()));
    }
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| Path::new(path).is_absolute())
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_requested_static_profile() {
        let values = ini_section(
            "[default]\naws_access_key_id = default\n[work]\naws_access_key_id = work\naws_secret_access_key = secret\naws_session_token = session\n",
            "work",
        );
        assert_eq!(values["aws_access_key_id"], "work");
        assert_eq!(values["aws_secret_access_key"], "secret");
        assert_eq!(values["aws_session_token"], "session");
    }

    #[test]
    fn stored_profile_is_marked_to_win_over_ambient_static_keys() {
        let settings = AwsCredentialSettings::from_environment(BTreeMap::from([(
            "AWS_PROFILE".to_string(),
            "work".to_string(),
        )]));
        assert!(settings.prefer_profile);
        assert_eq!(settings.get("AWS_PROFILE"), Some("work"));
    }
}
