use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;

#[cfg(test)]
use pi_coding::read_stored_credential;
use pi_coding::{StoredCredential, modify_credentials, read_credentials};

use crate::config::AuthCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthProviderInfo {
    pub(crate) id: String,
    pub(crate) supports_oauth: bool,
    pub(crate) stored_kind: Option<&'static str>,
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

pub(crate) async fn run(agent_dir: &Path, command: &AuthCommand) -> Result<(), String> {
    match command {
        AuthCommand::Check { .. } => {
            return Err("auth check requires the exit-code-aware CLI entry point".into());
        }
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
                    "openai" => run_chatgpt_oauth(agent_dir).await?,
                    "kimi-coding" => run_kimi_oauth().await?,
                    "github-copilot" => run_github_copilot_oauth().await?,
                    "openrouter" => run_openrouter_oauth().await?,
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
            if *oauth_token && !oauth_supported(&provider) {
                return Err(format!(
                    "provider {provider:?} does not support stored OAuth tokens"
                ));
            }
            if *oauth_token && provider == "openai" {
                return Err("OpenAI ChatGPT OAuth requires its issued client ID; use `pi auth login openai --oauth`".into());
            }
            if !*api_key && !*oauth_token && token.is_none() {
                let credential = match provider.as_str() {
                    "amazon-bedrock" => Some(run_amazon_bedrock_auth()?),
                    "google-vertex" => Some(run_google_vertex_auth()?),
                    "cloudflare-workers-ai" | "cloudflare-ai-gateway" => {
                        Some(run_cloudflare_auth(&provider)?)
                    }
                    _ => None,
                };
                if let Some(credential) = credential {
                    modify_credentials(agent_dir, |credentials| {
                        credentials.insert(provider.clone(), credential);
                    })?;
                    println!("Stored authentication configuration for {provider}.");
                    return Ok(());
                }
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
                    "openai" => run_chatgpt_oauth(agent_dir).await?,
                    "kimi-coding" => run_kimi_oauth().await?,
                    "github-copilot" => run_github_copilot_oauth().await?,
                    "openrouter" => run_openrouter_oauth().await?,
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

pub(crate) async fn check(
    config: &pi_coding::Config,
    command: &AuthCommand,
) -> Result<i32, String> {
    let AuthCommand::Check {
        provider,
        model,
        json,
        credentials,
        no_refresh,
    } = command
    else {
        return Err("expected auth check".into());
    };
    let result = pi_coding::check_auth(
        config,
        &pi_coding::AuthCheckOptions {
            provider: provider.clone(),
            model: model.clone(),
            credentials: *credentials,
            no_refresh: *no_refresh,
        },
    )
    .await;
    if *json {
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "{}",
            result
                .credentials
                .as_deref()
                .unwrap_or(result.status.as_str())
        );
    }
    Ok(result.status.exit_code())
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

async fn run_github_copilot_oauth() -> Result<StoredCredential, String> {
    let enterprise = prompt_line("GitHub Enterprise URL/domain (blank for github.com): ")?;
    let enterprise_domain = pi_plugin_copilot::normalize_enterprise_domain(&enterprise)?;
    let authorization =
        pi_plugin_copilot::start_github_copilot_device_authorization(enterprise_domain.as_deref())
            .await?;
    println!(
        "Open this URL to authorize GitHub Copilot:\n{}",
        authorization.verification_uri
    );
    println!("Verification code: {}", authorization.user_code);
    if let Err(error) = open_browser(&authorization.verification_uri) {
        eprintln!("Could not open the browser automatically: {error}");
    }
    let credential =
        pi_plugin_copilot::poll_github_copilot_device_authorization(&authorization).await?;
    let mut extra = BTreeMap::new();
    if let Some(domain) = &credential.enterprise_domain {
        extra.insert(
            "enterpriseUrl".to_string(),
            serde_json::Value::String(domain.clone()),
        );
    }
    extra.insert(
        "availableModelIds".to_string(),
        serde_json::json!(credential.available_model_ids),
    );
    Ok(StoredCredential::Oauth {
        access: credential.access,
        refresh: credential.refresh,
        expires: credential.expires as f64,
        extra,
    })
}

async fn run_openrouter_oauth() -> Result<StoredCredential, String> {
    let login = pi_plugin_openrouter::start_openrouter_oauth().await?;
    println!(
        "Open this URL to authorize OpenRouter:\n{}\nListening for the callback on {}",
        login.url, login.callback_url
    );
    if let Err(error) = open_browser(&login.url) {
        eprintln!("Could not open the browser automatically: {error}");
    }
    let credential = login.wait_for_credential().await?;
    Ok(StoredCredential::Oauth {
        access: credential.access,
        refresh: credential.refresh,
        expires: credential.expires as f64,
        extra: BTreeMap::new(),
    })
}

fn run_amazon_bedrock_auth() -> Result<StoredCredential, String> {
    require_interactive_auth("Amazon Bedrock")?;
    println!(
        "Select Amazon Bedrock authentication method:\n  1) Bearer token\n  2) AWS profile\n  3) Existing AWS credential chain"
    );
    match prompt_line("Authentication number: ")?.as_str() {
        "1" | "bearer" | "bearer-token" => {
            let token = rpassword::prompt_password("Amazon Bedrock bearer token: ")
                .map_err(|error| format!("failed to read bearer token: {error}"))?;
            let token = token.trim();
            if token.is_empty() {
                return Err("Amazon Bedrock bearer token cannot be empty".to_string());
            }
            Ok(StoredCredential::ApiKey {
                key: Some(token.to_string()),
                env: BTreeMap::new(),
                extra: BTreeMap::new(),
            })
        }
        "2" | "profile" | "aws-profile" => {
            let profile = prompt_line("AWS profile name: ")?;
            validate_environment_value("AWS profile", &profile)?;
            Ok(StoredCredential::ApiKey {
                key: None,
                env: BTreeMap::from([("AWS_PROFILE".to_string(), profile)]),
                extra: BTreeMap::new(),
            })
        }
        "3" | "chain" | "credential-chain" => Ok(StoredCredential::ApiKey {
            key: None,
            env: BTreeMap::new(),
            extra: BTreeMap::new(),
        }),
        value => Err(format!(
            "unknown Amazon Bedrock authentication selection {value:?}"
        )),
    }
}

fn run_google_vertex_auth() -> Result<StoredCredential, String> {
    require_interactive_auth("Google Vertex AI")?;
    println!(
        "Select Google Vertex AI authentication method:\n  1) Google Cloud API key\n  2) Application Default Credentials\n  3) Service account credentials file"
    );
    let method = prompt_line("Authentication number: ")?;
    if matches!(method.as_str(), "1" | "api" | "api-key") {
        let key = rpassword::prompt_password("Google Cloud API key: ")
            .map_err(|error| format!("failed to read API key: {error}"))?;
        let key = key.trim();
        if key.is_empty() {
            return Err("Google Cloud API key cannot be empty".to_string());
        }
        return Ok(StoredCredential::ApiKey {
            key: Some(key.to_string()),
            env: BTreeMap::new(),
            extra: BTreeMap::new(),
        });
    }
    let service_account = matches!(method.as_str(), "3" | "service" | "service-account");
    if !service_account && !matches!(method.as_str(), "2" | "adc") {
        return Err(format!(
            "unknown Google Vertex AI authentication selection {method:?}"
        ));
    }
    let project = prompt_line("Google Cloud project ID: ")?;
    validate_environment_value("Google Cloud project ID", &project)?;
    let location = prompt_line("Google Cloud location: ")?;
    validate_environment_value("Google Cloud location", &location)?;
    let mut env = BTreeMap::from([
        ("GOOGLE_CLOUD_PROJECT".to_string(), project),
        ("GOOGLE_CLOUD_LOCATION".to_string(), location),
    ]);
    if service_account {
        let path = prompt_line("Service account credentials file path: ")?;
        validate_environment_value("service account credentials path", &path)?;
        env.insert("GOOGLE_APPLICATION_CREDENTIALS".to_string(), path);
    }
    Ok(StoredCredential::ApiKey {
        key: None,
        env,
        extra: BTreeMap::new(),
    })
}

fn require_interactive_auth(provider: &str) -> Result<(), String> {
    if std::io::stdin().is_terminal() {
        Ok(())
    } else {
        Err(format!(
            "{provider} authentication setup requires a terminal; use --api-key --token for a bearer/API key"
        ))
    }
}

fn validate_environment_value(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.contains(['\r', '\n', '\0']) {
        Err(format!(
            "{label} cannot be empty or contain control characters"
        ))
    } else {
        Ok(())
    }
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
        let methods = match provider.as_str() {
            "amazon-bedrock" => "AWS credentials or bearer token",
            "google-vertex" => "API key, ADC, or service account",
            _ if oauth_supported(provider) => "OAuth or API key",
            _ => "API key",
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
        "amazon-bedrock",
        "anthropic",
        "azure-openai-responses",
        "github-copilot",
        "google",
        "google-vertex",
        "mistral",
        "openai-codex",
        "openai",
        "azure",
        "openai-compatible",
        "openrouter",
        "xai",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    providers.extend(
        pi_coding::gateway_definitions()
            .iter()
            .map(|definition| definition.id.clone()),
    );
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
    matches!(
        provider,
        "anthropic"
            | "github-copilot"
            | "openai-codex"
            | "openrouter"
            | "xai"
            | "openai"
            | "kimi-coding"
    )
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

async fn run_chatgpt_oauth(agent_dir: &Path) -> Result<StoredCredential, String> {
    std::fs::create_dir_all(agent_dir).map_err(|error| error.to_string())?;
    let path = agent_dir.join("device-id");
    let device = match std::fs::read_to_string(&path) {
        Ok(value) => value.trim().to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let value = uuid::Uuid::now_v7().to_string();
            std::fs::write(&path, &value).map_err(|error| error.to_string())?;
            value
        }
        Err(error) => return Err(error.to_string()),
    };
    let login = pi_plugin_openai::chatgpt_oauth::start(&device).await?;
    println!("Open this URL to sign in with ChatGPT:\n{}", login.url);
    if let Err(error) = open_browser(&login.url) {
        eprintln!("Could not open browser: {error}");
    }
    let value = login.wait().await?;
    Ok(StoredCredential::Oauth {
        access: value.access,
        refresh: value.refresh,
        expires: value.expires as f64,
        extra: value.metadata,
    })
}

async fn run_kimi_oauth() -> Result<StoredCredential, String> {
    let device = pi_coding::kimi_oauth::start().await?;
    println!(
        "Open this URL to sign in with Kimi Code:\n{}\nVerification code: {}",
        device.verification_uri_complete, device.user_code
    );
    if let Err(error) = open_browser(&device.verification_uri_complete) {
        eprintln!("Could not open browser: {error}");
    }
    let value = pi_coding::kimi_oauth::poll(&device).await?;
    Ok(StoredCredential::Oauth {
        access: value.access,
        refresh: value.refresh,
        expires: value.expires as f64,
        extra: BTreeMap::new(),
    })
}

fn run_cloudflare_auth(provider: &str) -> Result<StoredCredential, String> {
    require_interactive_auth("Cloudflare")?;
    let key =
        rpassword::prompt_password("Cloudflare API key: ").map_err(|error| error.to_string())?;
    validate_environment_value("Cloudflare API key", &key)?;
    let account = prompt_line("Cloudflare account ID: ")?;
    validate_environment_value("Cloudflare account ID", &account)?;
    let mut env = BTreeMap::from([("CLOUDFLARE_ACCOUNT_ID".into(), account)]);
    if provider == "cloudflare-ai-gateway" {
        let gateway = prompt_line("Cloudflare AI Gateway ID: ")?;
        validate_environment_value("Cloudflare AI Gateway ID", &gateway)?;
        env.insert("CLOUDFLARE_GATEWAY_ID".into(), gateway);
    }
    Ok(StoredCredential::ApiKey {
        key: Some(key),
        env,
        extra: BTreeMap::new(),
    })
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
    fn login_catalog_includes_all_builtin_provider_auth_surfaces() {
        let directory = tempfile::tempdir().unwrap();

        let catalog = login_provider_catalog(directory.path()).unwrap();
        for provider in [
            "amazon-bedrock",
            "azure-openai-responses",
            "github-copilot",
            "google",
            "google-vertex",
            "mistral",
            "openrouter",
        ] {
            assert!(
                catalog.iter().any(|candidate| candidate.id == provider),
                "missing {provider}"
            );
        }

        let google = catalog
            .iter()
            .find(|provider| provider.id == "google")
            .unwrap();
        assert!(!google.supports_oauth);
        assert_eq!(google.stored_kind, None);
        assert!(
            catalog
                .iter()
                .find(|provider| provider.id == "openrouter")
                .unwrap()
                .supports_oauth
        );
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
              "xai": {"type":"api_key","key":"xai-key","env":{"region":"test"}},
              "github-copilot": {"type":"oauth","access":"copilot","refresh":"github","expires":123,"availableModelIds":["gpt-4.1"]}
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
        let copilot = read_stored_credential(directory.path(), "github-copilot")
            .unwrap()
            .unwrap();
        assert_eq!(
            copilot.extra_strings("availableModelIds"),
            Some(vec!["gpt-4.1".to_string()])
        );
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
