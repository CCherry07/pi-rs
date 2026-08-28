use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::Path;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config::AuthCommand;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum StoredCredential {
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
    pub(crate) fn secret(&self) -> Option<&str> {
        match self {
            Self::ApiKey { key, .. } => key.as_deref(),
            Self::Oauth { access, .. } => Some(access.as_str()),
        }
        .map(str::trim)
        .filter(|value| !value.is_empty())
    }

    pub(crate) fn is_oauth(&self) -> bool {
        matches!(self, Self::Oauth { .. })
    }

    fn kind(&self) -> &'static str {
        if self.is_oauth() { "oauth" } else { "api_key" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthProviderInfo {
    pub(crate) id: String,
    pub(crate) supports_oauth: bool,
    pub(crate) stored_kind: Option<&'static str>,
}

pub(crate) fn read_stored_credential(
    agent_dir: &Path,
    provider: &str,
) -> Result<Option<StoredCredential>, String> {
    Ok(read_credentials(&agent_dir.join("auth.json"))?.remove(provider))
}

pub(crate) fn login_provider_catalog(agent_dir: &Path) -> Result<Vec<AuthProviderInfo>, String> {
    let credentials = read_credentials(&agent_dir.join("auth.json"))?;
    Ok(provider_catalog(agent_dir)?
        .into_iter()
        .map(|id| AuthProviderInfo {
            supports_oauth: oauth_supported(&id),
            stored_kind: credentials.get(&id).map(StoredCredential::kind),
            id,
        })
        .collect())
}

pub(crate) fn logout_provider_catalog(agent_dir: &Path) -> Result<Vec<AuthProviderInfo>, String> {
    Ok(read_credentials(&agent_dir.join("auth.json"))?
        .into_iter()
        .map(|(id, credential)| AuthProviderInfo {
            supports_oauth: oauth_supported(&id),
            stored_kind: Some(credential.kind()),
            id,
        })
        .collect())
}

pub(crate) async fn refresh_oauth_if_needed(agent_dir: &Path) -> Result<(), String> {
    for provider in ["xai", "anthropic", "openai-codex"] {
        refresh_provider_if_needed(agent_dir, provider).await?;
    }
    Ok(())
}

async fn refresh_provider_if_needed(agent_dir: &Path, provider: &str) -> Result<(), String> {
    let path = agent_dir.join("auth.json");
    let Some(StoredCredential::Oauth {
        refresh, expires, ..
    }) = read_credentials(&path)?.get(provider).cloned()
    else {
        return Ok(());
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_millis() as f64);
    if expires > now_ms + 5.0 * 60.0 * 1_000.0 {
        return Ok(());
    }
    if refresh.trim().is_empty() {
        return Err(format!(
            "stored {provider} OAuth token is expired and has no refresh token; run `pi auth login {provider} --oauth` again"
        ));
    }
    let (access, refresh, expires) = match provider {
        "xai" => {
            let credential = pi_plugin_xai::refresh(&refresh).await?;
            (credential.access, credential.refresh, credential.expires)
        }
        "anthropic" => {
            let credential = pi_plugin_anthropic::refresh(&refresh).await?;
            (credential.access, credential.refresh, credential.expires)
        }
        "openai-codex" => {
            let credential = pi_plugin_openai::refresh_openai_oauth(&refresh).await?;
            (credential.access, credential.refresh, credential.expires)
        }
        _ => return Ok(()),
    };
    modify_credentials(agent_dir, |credentials| {
        credentials.insert(
            provider.to_string(),
            StoredCredential::Oauth {
                access,
                refresh,
                expires: expires as f64,
                extra: BTreeMap::new(),
            },
        );
    })?;
    Ok(())
}

pub(crate) async fn run(agent_dir: &Path, command: &AuthCommand) -> Result<(), String> {
    match command {
        AuthCommand::Login {
            provider,
            api_key,
            oauth,
            oauth_token,
            token,
            refresh_token,
            expires,
        } => {
            let provider = select_provider(agent_dir, provider.as_deref())?;
            if *oauth {
                let credential = match provider.as_str() {
                    "xai" => run_xai_oauth().await?,
                    "anthropic" => run_anthropic_oauth().await?,
                    "openai-codex" => run_openai_oauth().await?,
                    _ => {
                        return Err(format!(
                            "provider {provider:?} does not support browser/device OAuth"
                        ));
                    }
                };
                modify_credentials(agent_dir, |credentials| {
                    credentials.insert(provider.clone(), credential);
                })?;
                println!("OAuth login successful for {provider}.");
                return Ok(());
            }
            if *oauth_token && !matches!(provider.as_str(), "anthropic" | "xai" | "openai-codex") {
                return Err(format!(
                    "provider {provider:?} does not support stored OAuth tokens"
                ));
            }
            let use_oauth = if !*api_key && !*oauth_token && token.is_none() {
                select_auth_type(&provider)?
            } else {
                false
            };
            if use_oauth {
                let credential = match provider.as_str() {
                    "xai" => run_xai_oauth().await?,
                    "anthropic" => run_anthropic_oauth().await?,
                    "openai-codex" => run_openai_oauth().await?,
                    _ => unreachable!("OAuth support checked by select_auth_type"),
                };
                modify_credentials(agent_dir, |credentials| {
                    credentials.insert(provider.clone(), credential);
                })?;
                println!("OAuth login successful for {provider}.");
                return Ok(());
            }
            let secret = match token {
                Some(token) => token.trim().to_string(),
                None if std::io::stdin().is_terminal() => rpassword::prompt_password(format!(
                    "{} for {provider}: ",
                    if *oauth_token {
                        "OAuth access token"
                    } else {
                        "API key"
                    }
                ))
                .map_err(|error| format!("failed to read secret: {error}"))?
                .trim()
                .to_string(),
                None => {
                    return Err(
                        "auth login requires a terminal prompt or the hidden --token option"
                            .to_string(),
                    );
                }
            };
            if secret.is_empty() {
                return Err("credential cannot be empty".to_string());
            }
            let credential = if *oauth_token {
                StoredCredential::Oauth {
                    access: secret,
                    refresh: refresh_token.clone().unwrap_or_default(),
                    expires: expires.unwrap_or(0.0),
                    extra: BTreeMap::new(),
                }
            } else {
                let _ = api_key;
                StoredCredential::ApiKey {
                    key: Some(secret),
                    env: BTreeMap::new(),
                    extra: BTreeMap::new(),
                }
            };
            modify_credentials(agent_dir, |credentials| {
                credentials.insert(provider.clone(), credential);
            })?;
            println!(
                "Stored {} credential for {provider}.",
                if *oauth_token { "OAuth" } else { "API key" }
            );
        }
        AuthCommand::Logout { provider } => {
            validate_provider_id(provider)?;
            let removed =
                modify_credentials(agent_dir, |credentials| credentials.remove(provider))?;
            if removed.is_some() {
                println!("Removed stored credential for {provider}.");
            } else {
                println!("No stored credential for {provider}.");
            }
        }
        AuthCommand::Status { provider } => {
            let credentials = read_credentials(&agent_dir.join("auth.json"))?;
            if let Some(provider) = provider {
                validate_provider_id(provider)?;
                match credentials.get(provider) {
                    Some(credential) => println!("{provider}\t{}\tstored", credential.kind()),
                    None => println!("{provider}\tunconfigured"),
                }
            } else if credentials.is_empty() {
                println!("No stored credentials.");
            } else {
                for (provider, credential) in credentials {
                    println!("{provider}\t{}\tstored", credential.kind());
                }
            }
        }
    }
    Ok(())
}

async fn run_anthropic_oauth() -> Result<StoredCredential, String> {
    let start = pi_plugin_anthropic::start_oauth()?;
    println!(
        "Open this URL to authorize Anthropic/Claude:\n{}",
        start.url
    );
    if let Err(error) = open_browser(&start.url) {
        eprintln!("Could not open the browser automatically: {error}");
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "Anthropic OAuth requires a terminal to paste the callback URL/code".to_string(),
        );
    }
    let input = rpassword::prompt_password("Paste the final callback URL or authorization code: ")
        .map_err(|error| format!("failed to read authorization code: {error}"))?;
    let credential = pi_plugin_anthropic::complete_oauth(&input, &start.verifier).await?;
    Ok(StoredCredential::Oauth {
        access: credential.access,
        refresh: credential.refresh,
        expires: credential.expires as f64,
        extra: BTreeMap::new(),
    })
}

async fn run_openai_oauth() -> Result<StoredCredential, String> {
    let authorization = pi_plugin_openai::start_openai_device_authorization().await?;
    println!(
        "Open this URL to authorize OpenAI Codex:\n{}",
        authorization.verification_uri
    );
    println!("Verification code: {}", authorization.user_code);
    if let Err(error) = open_browser(&authorization.verification_uri) {
        eprintln!("Could not open the browser automatically: {error}");
    }
    let credential = pi_plugin_openai::poll_openai_device_authorization(&authorization).await?;
    Ok(StoredCredential::Oauth {
        access: credential.access,
        refresh: credential.refresh,
        expires: credential.expires as f64,
        extra: BTreeMap::new(),
    })
}

async fn run_xai_oauth() -> Result<StoredCredential, String> {
    let authorization = pi_plugin_xai::start_device_authorization().await?;
    let url = authorization
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&authorization.verification_uri);
    println!("Open this URL to authorize xAI/Grok:\n{url}");
    println!("Verification code: {}", authorization.user_code);
    if let Err(error) = open_browser(url) {
        eprintln!("Could not open the browser automatically: {error}");
    }
    let credential = pi_plugin_xai::poll_device_authorization(&authorization).await?;
    Ok(StoredCredential::Oauth {
        access: credential.access,
        refresh: credential.refresh,
        expires: credential.expires as f64,
        extra: BTreeMap::new(),
    })
}

fn open_browser(url: &str) -> Result<(), String> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    }
    .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("browser opener exited with {status}"))
    }
}

fn select_provider(agent_dir: &Path, requested: Option<&str>) -> Result<String, String> {
    if let Some(provider) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        validate_provider_id(provider)?;
        return Ok(provider.to_string());
    }
    if !std::io::stdin().is_terminal() {
        return Err("auth login requires a provider in non-interactive mode".to_string());
    }
    let providers = provider_catalog(agent_dir)?;
    println!("Select provider to configure:");
    for (index, provider) in providers.iter().enumerate() {
        let methods = if oauth_supported(provider) {
            "OAuth or API key"
        } else {
            "API key"
        };
        println!("  {}) {provider} ({methods})", index + 1);
    }
    let selection = prompt_line("Provider number or ID: ")?;
    if let Ok(index) = selection.parse::<usize>()
        && let Some(provider) = index.checked_sub(1).and_then(|index| providers.get(index))
    {
        return Ok(provider.clone());
    }
    validate_provider_id(&selection)?;
    Ok(selection)
}

fn provider_catalog(agent_dir: &Path) -> Result<Vec<String>, String> {
    let mut providers = [
        "anthropic",
        "google",
        "openai-codex",
        "openai-compatible",
        "xai",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let models_path = agent_dir.join("models.json");
    let configured = pi_plugin_models::ModelsPlugin::load(
        pi_plugin_models::ModelsPluginOptions::new(&models_path),
    )
    .map_err(|error| error.to_string())?;
    providers.extend(
        configured
            .provider_ids()
            .into_iter()
            .map(|provider| provider.as_str().to_string()),
    );
    providers.extend(read_credentials(&agent_dir.join("auth.json"))?.into_keys());
    providers.sort();
    providers.dedup();
    Ok(providers)
}

fn select_auth_type(provider: &str) -> Result<bool, String> {
    if !oauth_supported(provider) {
        return Ok(false);
    }
    println!(
        "Select authentication for {provider}:\n  1) OAuth subscription/browser login\n  2) API key"
    );
    match prompt_line("Authentication number: ")?.as_str() {
        "1" | "oauth" => Ok(true),
        "2" | "api" | "api_key" => Ok(false),
        value => Err(format!("unknown authentication selection {value:?}")),
    }
}

fn oauth_supported(provider: &str) -> bool {
    matches!(provider, "anthropic" | "openai-codex" | "xai")
}

fn prompt_line(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|error| error.to_string())?;
    Ok(input.trim().to_string())
}

fn validate_provider_id(provider: &str) -> Result<(), String> {
    if provider.is_empty()
        || provider.len() > 128
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("invalid provider ID {provider:?}"));
    }
    Ok(())
}

fn read_credentials(path: &Path) -> Result<BTreeMap<String, StoredCredential>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    serde_json::from_str(&content).map_err(|error| format!("invalid {}: {error}", path.display()))
}

fn modify_credentials<T>(
    agent_dir: &Path,
    update: impl FnOnce(&mut BTreeMap<String, StoredCredential>) -> T,
) -> Result<T, String> {
    std::fs::create_dir_all(agent_dir)
        .map_err(|error| format!("failed to create {}: {error}", agent_dir.display()))?;
    let path = agent_dir.join("auth.json");
    let lock_path = agent_dir.join("auth.json.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("failed to open {}: {error}", lock_path.display()))?;
    lock.lock_exclusive()
        .map_err(|error| format!("failed to lock {}: {error}", lock_path.display()))?;
    let result = (|| {
        let mut credentials = read_credentials(&path)?;
        let result = update(&mut credentials);
        let encoded = serde_json::to_vec_pretty(&credentials)
            .map_err(|error| format!("failed to encode {}: {error}", path.display()))?;
        let temporary = agent_dir.join(format!(".auth.json.{}.tmp", std::process::id()));
        let mut file = create_secret_file(&temporary)?;
        file.write_all(&encoded)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("failed to replace {}: {error}", path.display())
        })?;
        Ok(result)
    })();
    let _ = FileExt::unlock(&lock);
    result
}

fn create_secret_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_catalog_includes_jsonc_custom_providers() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("models.json"),
            r#"{
              // Third-party providers use API-key auth by default.
              "providers": {
                "acme-gateway": {
                  "api": "openai-completions",
                  "baseUrl": "https://gateway.example/v1",
                  "models": [{"id":"model"}]
                }
              }
            }"#,
        )
        .unwrap();
        assert!(
            provider_catalog(directory.path())
                .unwrap()
                .contains(&"acme-gateway".to_string())
        );
        assert!(!oauth_supported("acme-gateway"));
    }

    #[test]
    fn login_catalog_includes_builtin_google_api_key_provider() {
        let directory = tempfile::tempdir().unwrap();

        let google = login_provider_catalog(directory.path())
            .unwrap()
            .into_iter()
            .find(|provider| provider.id == "google")
            .expect("Google must be available from /login without models.json");

        assert!(!google.supports_oauth);
        assert_eq!(google.stored_kind, None);
    }

    #[test]
    fn tui_auth_catalogs_distinguish_login_options_from_stored_credentials() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("auth.json"),
            r#"{
              "anthropic": {"type":"oauth","access":"token","refresh":"refresh","expires":123},
              "private-gateway": {"type":"api_key","key":"secret"}
            }"#,
        )
        .unwrap();

        let login = login_provider_catalog(directory.path()).unwrap();
        assert!(login.iter().any(|provider| {
            provider.id == "anthropic"
                && provider.supports_oauth
                && provider.stored_kind == Some("oauth")
        }));
        assert!(login.iter().any(|provider| {
            provider.id == "openai-compatible" && provider.stored_kind.is_none()
        }));

        let logout = logout_provider_catalog(directory.path()).unwrap();
        assert_eq!(logout.len(), 2);
        assert!(logout.iter().any(|provider| {
            provider.id == "private-gateway" && provider.stored_kind == Some("api_key")
        }));
    }

    #[test]
    fn reads_pi_api_key_and_oauth_shapes() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("auth.json"),
            r#"{
              "anthropic": {"type":"oauth","access":"token","refresh":"refresh","expires":123},
              "xai": {"type":"api_key","key":"xai-key","env":{"region":"test"}}
            }"#,
        )
        .unwrap();

        let anthropic = read_stored_credential(directory.path(), "anthropic")
            .unwrap()
            .unwrap();
        assert!(anthropic.is_oauth());
        assert_eq!(anthropic.secret(), Some("token"));
        let xai = read_stored_credential(directory.path(), "xai")
            .unwrap()
            .unwrap();
        assert!(!xai.is_oauth());
        assert_eq!(xai.secret(), Some("xai-key"));
    }

    #[tokio::test]
    async fn writes_and_removes_credentials_without_exposing_secrets() {
        let directory = tempfile::tempdir().unwrap();
        run(
            directory.path(),
            &AuthCommand::Login {
                provider: Some("xai".to_string()),
                api_key: true,
                oauth: false,
                oauth_token: false,
                token: Some("secret".to_string()),
                refresh_token: None,
                expires: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            read_stored_credential(directory.path(), "xai")
                .unwrap()
                .unwrap()
                .secret(),
            Some("secret")
        );
        run(
            directory.path(),
            &AuthCommand::Logout {
                provider: "xai".to_string(),
            },
        )
        .await
        .unwrap();
        assert!(
            read_stored_credential(directory.path(), "xai")
                .unwrap()
                .is_none()
        );
    }
}
