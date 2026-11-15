use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredCredential {
    ApiKey {
        #[serde(default)]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    Oauth {
        access: String,
        refresh: String,
        expires: f64,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

impl StoredCredential {
    pub fn secret(&self) -> Option<&str> {
        match self {
            Self::ApiKey { key, .. } => key.as_deref(),
            Self::Oauth { access, .. } => Some(access.as_str()),
        }
        .map(str::trim)
        .filter(|value| !value.is_empty())
    }

    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::Oauth { .. })
    }

    pub fn extra_string(&self, name: &str) -> Option<&str> {
        let extra = match self {
            Self::ApiKey { extra, .. } | Self::Oauth { extra, .. } => extra,
        };
        extra.get(name).and_then(serde_json::Value::as_str)
    }

    pub fn extra_strings(&self, name: &str) -> Option<Vec<String>> {
        let extra = match self {
            Self::ApiKey { extra, .. } | Self::Oauth { extra, .. } => extra,
        };
        extra
            .get(name)?
            .as_array()?
            .iter()
            .map(|value| value.as_str().map(str::to_string))
            .collect()
    }

    pub fn environment(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::ApiKey { env, .. } => Some(env),
            Self::Oauth { .. } => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        if self.is_oauth() { "oauth" } else { "api_key" }
    }
}

pub fn read_stored_credential(
    agent_dir: &Path,
    provider: &str,
) -> Result<Option<StoredCredential>, String> {
    Ok(read_credentials(&agent_dir.join("auth.json"))?.remove(provider))
}

pub fn read_credentials(path: &Path) -> Result<BTreeMap<String, StoredCredential>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    serde_json::from_str(&content).map_err(|error| format!("invalid {}: {error}", path.display()))
}

/// Shared atomic credential store used by login/logout and request-time refresh.
/// The same sibling lock spans read, network refresh, and durable replacement.
pub fn modify_credentials<T>(
    agent_dir: &Path,
    update: impl FnOnce(&mut BTreeMap<String, StoredCredential>) -> T,
) -> Result<T, String> {
    use fs2::FileExt;
    let lock = open_lock(agent_dir)?;
    lock.lock_exclusive()
        .map_err(|error| format!("failed to lock credentials: {error}"))?;
    let mut credentials = read_credentials(&agent_dir.join("auth.json"))?;
    let result = update(&mut credentials);
    save_credentials(agent_dir, &credentials)?;
    Ok(result)
}

fn open_lock(agent_dir: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(agent_dir)
        .map_err(|error| format!("failed to create credential directory: {error}"))?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(agent_dir.join("auth.json.lock"))
        .map_err(|error| format!("failed to open credential lock: {error}"))
}

fn save_credentials(
    agent_dir: &Path,
    credentials: &BTreeMap<String, StoredCredential>,
) -> Result<(), String> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(agent_dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    serde_json::to_writer_pretty(&mut file, credentials).map_err(|error| error.to_string())?;
    file.write_all(b"\n")
        .and_then(|_| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    file.persist(agent_dir.join("auth.json"))
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct StoredCredentialSource {
    agent_dir: std::path::PathBuf,
    provider: String,
    explicit: Option<String>,
    environment: Vec<(String, pi_provider::CredentialKind)>,
}

impl StoredCredentialSource {
    pub(crate) fn new(
        agent_dir: &Path,
        provider: &str,
        explicit: Option<String>,
        environment: Vec<(String, pi_provider::CredentialKind)>,
    ) -> Self {
        Self {
            agent_dir: agent_dir.to_owned(),
            provider: provider.into(),
            explicit,
            environment,
        }
    }

    pub(crate) fn builtin(
        agent_dir: &Path,
        provider: &str,
        explicit: Option<String>,
    ) -> std::sync::Arc<dyn pi_provider::CredentialSource> {
        use pi_provider::CredentialKind::{ApiKey, Bearer, OAuth};
        let environment = match provider {
            "anthropic" => vec![
                ("ANTHROPIC_AUTH_TOKEN", Bearer),
                ("ANTHROPIC_OAUTH_TOKEN", OAuth),
                ("ANTHROPIC_API_KEY", ApiKey),
            ],
            "xai" => vec![("XAI_API_KEY", ApiKey)],
            "openai" => vec![("OPENAI_API_KEY", ApiKey)],
            "github-copilot" => vec![("COPILOT_GITHUB_TOKEN", ApiKey)],
            "kimi-coding" => vec![("KIMI_API_KEY", ApiKey)],
            _ => Vec::new(),
        }
        .into_iter()
        .map(|(key, kind)| (key.to_owned(), kind))
        .collect();
        std::sync::Arc::new(Self::new(agent_dir, provider, explicit, environment))
    }
}

#[async_trait::async_trait]
impl pi_provider::CredentialSource for StoredCredentialSource {
    async fn resolve(
        &self,
        signal: &pi_core::AbortSignal,
    ) -> Result<Option<pi_provider::ProviderCredential>, pi_plugin::ProviderError> {
        use pi_provider::{CredentialKind, ProviderCredential};
        // Cloudflare merges each stored field before falling back to ambient env.
        let cloudflare = if self.provider.starts_with("cloudflare-") {
            read_stored_credential(&self.agent_dir, &self.provider)
                .map_err(pi_plugin::ProviderError::Failure)?
        } else {
            None
        };
        let configured = self
            .explicit
            .clone()
            .map(|key| (key, CredentialKind::ApiKey))
            .or_else(|| {
                cloudflare
                    .as_ref()?
                    .secret()
                    .map(|key| (key.to_owned(), CredentialKind::ApiKey))
            })
            .or_else(|| {
                self.environment.iter().find_map(|(key, kind)| {
                    std::env::var(key)
                        .ok()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| (value, kind.clone()))
                })
            });
        if let Some((secret, kind)) = configured {
            let mut metadata = BTreeMap::new();
            if self.provider == "github-copilot" && self.explicit.is_none() {
                let stored = read_stored_credential(&self.agent_dir, &self.provider)
                    .map_err(pi_plugin::ProviderError::Failure)?;
                let domain = std::env::var("GITHUB_COPILOT_ENTERPRISE_DOMAIN")
                    .ok()
                    .or_else(|| {
                        stored
                            .as_ref()?
                            .extra_string("enterpriseUrl")
                            .map(str::to_owned)
                    });
                if let Some(domain) = domain {
                    metadata.insert("enterpriseUrl".into(), serde_json::json!(domain));
                }
            }
            if let Some(StoredCredential::ApiKey { env, .. }) = cloudflare {
                metadata.insert("env".into(), serde_json::json!(env));
            }
            return Ok(Some(ProviderCredential {
                secret,
                kind,
                metadata,
            }));
        }
        let source = self.clone();
        let lock_signal = signal.clone();
        // A detached refresh must finish persisting a rotated token even when
        // the caller aborts. Cancellation still interrupts waiting for the lock.
        let task = tokio::spawn(async move { source.resolve_stored(&lock_signal).await });
        let credential = tokio::select! {
            _ = signal.wait() => return Err(pi_plugin::ProviderError::Aborted),
            result = task => result.map_err(|error| pi_plugin::ProviderError::Failure(error.to_string()))?.map_err(pi_plugin::ProviderError::Failure)?,
        };
        Ok(credential.and_then(|value| {
            let secret = value.secret()?.to_owned();
            let kind = if value.is_oauth() {
                CredentialKind::OAuth
            } else {
                CredentialKind::ApiKey
            };
            let mut metadata = match value {
                StoredCredential::ApiKey { env, mut extra, .. } => {
                    extra.insert("env".into(), serde_json::json!(env));
                    extra
                }
                StoredCredential::Oauth { extra, .. } => extra,
            };
            if self.provider == "github-copilot"
                && let Ok(domain) = std::env::var("GITHUB_COPILOT_ENTERPRISE_DOMAIN")
            {
                metadata.insert("enterpriseUrl".into(), serde_json::json!(domain));
            }
            Some(ProviderCredential {
                secret,
                kind,
                metadata,
            })
        }))
    }
}

impl StoredCredentialSource {
    async fn resolve_stored(
        &self,
        signal: &pi_core::AbortSignal,
    ) -> Result<Option<StoredCredential>, String> {
        let provider = self.provider.clone();
        self.resolve_stored_with(signal, move |credential| async move {
            refresh_credential(&provider, credential).await
        })
        .await
    }

    async fn resolve_stored_with<F, Fut>(
        &self,
        signal: &pi_core::AbortSignal,
        refresh: F,
    ) -> Result<Option<StoredCredential>, String>
    where
        F: FnOnce(StoredCredential) -> Fut,
        Fut: std::future::Future<Output = Result<StoredCredential, String>>,
    {
        use fs2::FileExt;
        let credential = read_stored_credential(&self.agent_dir, &self.provider)?;
        if !needs_refresh(credential.as_ref()) {
            return Ok(credential);
        }
        let lock = open_lock(&self.agent_dir)?;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::select! {
                        _ = signal.wait() => return Err("credential lock wait aborted".into()),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
                    }
                }
                Err(error) => return Err(format!("failed to lock credentials: {error}")),
            }
        }
        if signal.is_aborted() {
            return Err("credential lock wait aborted".into());
        }
        let mut credentials = read_credentials(&self.agent_dir.join("auth.json"))?;
        let current = credentials.get(&self.provider).cloned();
        if !needs_refresh(current.as_ref()) {
            return Ok(current);
        }
        let current = current.expect("refresh requires a stored OAuth credential");
        let refreshed = tokio::time::timeout(std::time::Duration::from_secs(60), refresh(current))
            .await
            .map_err(|_| format!("{} OAuth refresh timed out", self.provider))??;
        credentials.insert(self.provider.clone(), refreshed.clone());
        save_credentials(&self.agent_dir, &credentials)?;
        Ok(Some(refreshed))
    }
}

fn needs_refresh(credential: Option<&StoredCredential>) -> bool {
    matches!(credential, Some(StoredCredential::Oauth { expires, .. }) if *expires <= pi_utils::time::unix_timestamp_ms_u64() as f64 + 300_000.0)
}

async fn refresh_credential(
    provider: &str,
    credential: StoredCredential,
) -> Result<StoredCredential, String> {
    let StoredCredential::Oauth {
        refresh, mut extra, ..
    } = credential
    else {
        return Ok(credential);
    };
    if refresh.trim().is_empty() {
        return Err(format!(
            "stored {provider} OAuth token expired; run `pi auth login {provider} --oauth`"
        ));
    }
    let (access, refresh, expires) = match provider {
        "openai" => {
            let value = pi_plugin_openai::chatgpt_oauth::refresh(
                &refresh,
                extra
                    .get("clientId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            )
            .await?;
            extra.extend(value.metadata);
            (value.access, value.refresh, value.expires)
        }
        "kimi-coding" => {
            let value = pi_plugin_gateways::kimi_oauth::refresh(&refresh).await?;
            (value.access, value.refresh, value.expires)
        }
        "anthropic" => {
            let value = pi_plugin_anthropic::refresh(&refresh).await?;
            (value.access, value.refresh, value.expires)
        }
        "xai" => {
            let value = pi_plugin_xai::refresh(&refresh).await?;
            (value.access, value.refresh, value.expires)
        }
        "openai-codex" => {
            let value = pi_plugin_openai::refresh_openai_oauth(&refresh).await?;
            (value.access, value.refresh, value.expires)
        }
        "github-copilot" => {
            let value = pi_plugin_copilot::refresh_github_copilot_oauth(
                &refresh,
                extra
                    .get("enterpriseUrl")
                    .and_then(serde_json::Value::as_str),
            )
            .await?;
            extra.insert(
                "availableModelIds".into(),
                serde_json::json!(value.available_model_ids),
            );
            (value.access, value.refresh, value.expires)
        }
        _ => {
            return Err(format!(
                "provider {provider} does not support OAuth refresh"
            ));
        }
    };
    Ok(StoredCredential::Oauth {
        access,
        refresh,
        expires: expires as f64,
        extra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn oauth(access: &str, expires: f64) -> StoredCredential {
        StoredCredential::Oauth {
            access: access.into(),
            refresh: "rotating-refresh".into(),
            expires,
            extra: BTreeMap::from([("custom".into(), serde_json::json!(true))]),
        }
    }

    #[tokio::test]
    async fn cloudflare_prefers_saved_fields_and_explicit_key_over_ambient_env() {
        use pi_provider::CredentialSource;
        let directory = tempfile::tempdir().unwrap();
        modify_credentials(directory.path(), |credentials| {
            credentials.insert(
                "cloudflare-ai-gateway".into(),
                StoredCredential::ApiKey {
                    key: Some("saved-key".into()),
                    env: BTreeMap::from([("CLOUDFLARE_ACCOUNT_ID".into(), "saved-account".into())]),
                    extra: BTreeMap::new(),
                },
            );
        })
        .unwrap();
        // An existing non-secret environment variable exercises precedence without
        // changing process-wide environment in parallel tests.
        assert!(std::env::var("PATH").is_ok());
        let (_, signal) = pi_core::AbortHandle::new();
        for (explicit, expected) in [(None, "saved-key"), (Some("explicit".into()), "explicit")] {
            let source = StoredCredentialSource::new(
                directory.path(),
                "cloudflare-ai-gateway",
                explicit,
                vec![("PATH".into(), pi_provider::CredentialKind::ApiKey)],
            );
            let credential = source.resolve(&signal).await.unwrap().unwrap();
            assert_eq!(credential.secret, expected);
            assert_eq!(
                credential.metadata["env"]["CLOUDFLARE_ACCOUNT_ID"],
                "saved-account"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_requests_refresh_once_and_persist_before_unlocking() {
        let directory = tempfile::tempdir().unwrap();
        modify_credentials(directory.path(), |credentials| {
            credentials.insert("test".into(), oauth("old", 0.0));
        })
        .unwrap();
        let source = StoredCredentialSource::new(directory.path(), "test", None, vec![]);
        let calls = Arc::new(AtomicUsize::new(0));
        let (_, signal) = pi_core::AbortHandle::new();
        let resolve = || {
            let source = source.clone();
            let calls = calls.clone();
            let signal = signal.clone();
            tokio::spawn(async move {
                source
                    .resolve_stored_with(&signal, move |_| async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                        Ok(oauth("new", f64::MAX))
                    })
                    .await
                    .unwrap()
                    .unwrap()
            })
        };
        let (one, two) = tokio::join!(resolve(), resolve());
        assert_eq!(one.unwrap().secret(), Some("new"));
        assert_eq!(two.unwrap().secret(), Some("new"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            read_stored_credential(directory.path(), "test")
                .unwrap()
                .unwrap()
                .secret(),
            Some("new")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(directory.path().join("auth.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn cancellation_after_refresh_starts_preserves_rotated_credentials() {
        let directory = tempfile::tempdir().unwrap();
        modify_credentials(directory.path(), |credentials| {
            credentials.insert("test".into(), oauth("old", 0.0));
        })
        .unwrap();
        let source = StoredCredentialSource::new(directory.path(), "test", None, vec![]);
        let (abort, signal) = pi_core::AbortHandle::new();
        let result = source
            .resolve_stored_with(&signal, |_| async {
                abort.abort();
                Ok(oauth("rotated", f64::MAX))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.secret(), Some("rotated"));
        assert_eq!(
            read_stored_credential(directory.path(), "test")
                .unwrap()
                .unwrap()
                .secret(),
            Some("rotated")
        );
    }

    #[tokio::test]
    async fn failed_refresh_preserves_previous_credential() {
        let directory = tempfile::tempdir().unwrap();
        modify_credentials(directory.path(), |credentials| {
            credentials.insert("test".into(), oauth("old", 0.0));
        })
        .unwrap();
        let source = StoredCredentialSource::new(directory.path(), "test", None, vec![]);
        let (_, signal) = pi_core::AbortHandle::new();
        assert!(
            source
                .resolve_stored_with(&signal, |_| async { Err("unavailable".into()) })
                .await
                .is_err()
        );
        assert_eq!(
            read_stored_credential(directory.path(), "test")
                .unwrap()
                .unwrap()
                .secret(),
            Some("old")
        );
    }
}
