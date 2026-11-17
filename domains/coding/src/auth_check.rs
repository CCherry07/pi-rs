//! Credential readiness without project discovery, sessions, or model requests.

use pi_core::{AbortHandle, ModelId, ProviderId};
use pi_plugin_models::ModelsPlugin;
use pi_runtime::PiRuntime;
use serde::Serialize;

use crate::Config;
use crate::builtin_providers::BuiltinProviderSet;
use crate::configuration::{apply_settings, initial_model_request};
use crate::credentials::{StoredCredentialSource, read_stored_credential};

#[derive(Clone, Debug, Default)]
pub struct AuthCheckOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub credentials: bool,
    pub no_refresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthCheckStatus {
    Ready,
    NotReady,
    Invalid,
}

impl AuthCheckStatus {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Ready => 0,
            Self::NotReady => 1,
            Self::Invalid => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NotReady => "not_ready",
            Self::Invalid => "invalid",
        }
    }
}

/// May contain a credential only when explicitly requested; deliberately no Debug.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheckResult {
    pub status: AuthCheckStatus,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<String>,
}

impl AuthCheckResult {
    fn state(provider: String, status: AuthCheckStatus, reason: Option<&'static str>) -> Self {
        Self {
            status,
            provider,
            reason,
            auth_type: None,
            credentials: None,
        }
    }
}

/// Like Pi's auth check, resolves configured credentials and optionally refreshes OAuth.
/// With no selector, checks the configured global default even if unavailable;
/// only an absent default falls back to normal initial model selection.
/// Project resources and extension providers are deliberately not loaded by this query.
pub async fn check_auth(config: &Config, options: &AuthCheckOptions) -> AuthCheckResult {
    let mut config = config.clone();
    let settings = pi_settings::SettingsManager::new(&config.agent_dir)
        .load(&pi_settings::SettingsContext::new(&config.cwd, false));
    apply_settings(&mut config, &settings);
    let requested = options
        .provider
        .as_deref()
        .or(options.model.as_deref())
        .or(config.runtime_settings.default_provider.as_deref())
        .unwrap_or(&config.provider)
        .to_owned();
    let mut selected = requested.clone();
    match check_inner(&config, options, &mut selected).await {
        Ok(result) => result,
        // Neither provider responses nor helper output can expose secrets here.
        Err(()) => {
            AuthCheckResult::state(selected, AuthCheckStatus::Invalid, Some("invalid_state"))
        }
    }
}

async fn check_inner(
    config: &Config,
    options: &AuthCheckOptions,
    selected: &mut String,
) -> Result<AuthCheckResult, ()> {
    let builtins = BuiltinProviderSet::prepare(config).map_err(|_| ())?;
    let models = ModelsPlugin::load(builtins.model_options(config)).map_err(|_| ())?;
    let runtime = builtins
        .register(PiRuntime::builder(), config)
        .provider_plugin(models.clone())
        .agent_options(pi_agent::AgentOptions {
            provider_id: ProviderId::new(&config.provider),
            model_id: ModelId::new(&config.fallback_model),
            cwd: config.cwd.clone(),
            ..Default::default()
        })
        .build()
        .map_err(|_| ())?;
    let provider = select_provider(config, options, &runtime)?;
    *selected = provider.to_string();
    if !runtime.has_provider(&provider) {
        return Ok(AuthCheckResult::state(
            selected.clone(),
            AuthCheckStatus::NotReady,
            Some("provider_not_found"),
        ));
    }
    let (_, signal) = AbortHandle::new();
    let route = models
        .catalog_request_config(&provider, &signal)
        .await
        .map_err(|_| ())?;
    let mut auth_type = if provider.as_str() == "openai-codex" {
        "oauth"
    } else {
        "api_key"
    };
    let mut credential = route.api_key.filter(|key| !key.trim().is_empty());
    if credential.is_none() {
        credential = route.headers.iter().find_map(|(name, value)| {
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.split_once(' '))
                .flatten()
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, value)| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        });
    }
    if credential.is_none() {
        credential = (config.provider == provider.as_str())
            .then(|| config.api_key.clone())
            .flatten();
    }
    if credential.is_none()
        && let Some((secret, kind)) = environment_credential(provider.as_str())
    {
        credential = Some(secret);
        auth_type = kind;
    }
    if credential.is_none() {
        let stored = if options.no_refresh {
            read_stored_credential(&config.agent_dir, provider.as_str()).map_err(|_| ())?
        } else {
            StoredCredentialSource::new(&config.agent_dir, provider.as_str(), None, vec![])
                .resolve_stored(&signal)
                .await
                .map_err(|_| ())?
        };
        if let Some(stored) = stored {
            auth_type = stored.kind();
            credential = stored.secret().map(str::to_owned);
        }
    }
    // Cloudflare credentials include account/gateway metadata in addition to the
    // token. The runtime's snapshot already combines those stored/env fields.
    // Cloud chains and auth-free local routes may be ready without a printable key.
    if !runtime.provider_is_available(&provider)
        && (credential.is_none()
            || matches!(
                provider.as_str(),
                "cloudflare-workers-ai" | "cloudflare-ai-gateway"
            ))
    {
        return Ok(AuthCheckResult::state(
            selected.clone(),
            AuthCheckStatus::NotReady,
            Some("credentials_not_configured"),
        ));
    }
    if options.credentials && credential.is_none() {
        return Ok(AuthCheckResult::state(
            selected.clone(),
            AuthCheckStatus::NotReady,
            Some("credential_not_available"),
        ));
    }
    Ok(AuthCheckResult {
        status: AuthCheckStatus::Ready,
        provider: selected.clone(),
        reason: None,
        auth_type: Some(auth_type),
        credentials: options.credentials.then_some(credential).flatten(),
    })
}

fn select_provider(
    config: &Config,
    options: &AuthCheckOptions,
    runtime: &PiRuntime,
) -> Result<ProviderId, ()> {
    let provider = options
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if options.provider.is_some() && provider.is_none() {
        return Err(());
    }
    if let Some(model) = options.model.as_deref() {
        let current = ProviderId::new(provider.unwrap_or(&config.provider));
        let model = runtime
            .resolve_model_reference(&current, model.trim())
            .ok_or(())?;
        if provider.is_some_and(|provider| model.provider.as_str() != provider) {
            return Err(());
        }
        return Ok(model.provider);
    }
    if let Some(provider) = provider {
        return Ok(ProviderId::new(provider));
    }
    // A readiness query must report the configured default even when it is
    // unavailable, rather than silently checking a different, available model.
    if let Some(model) = config.runtime_settings.default_model.as_deref() {
        let current = ProviderId::new(
            config
                .runtime_settings
                .default_provider
                .as_deref()
                .unwrap_or(&config.provider),
        );
        return runtime
            .resolve_model_reference(&current, model)
            .map(|model| model.provider)
            .ok_or(());
    }
    if let Some(provider) = config.runtime_settings.default_provider.as_deref() {
        return Ok(ProviderId::new(provider));
    }
    initial_model_request(None, None, &config.runtime_settings)
        .select(runtime)
        .map_err(|_| ())?;
    Ok(runtime.agent().state().provider_id)
}

fn environment_credential(provider: &str) -> Option<(String, &'static str)> {
    let names: &[&str] = match provider {
        "openai" | "openai-compatible" => &["OPENAI_API_KEY"],
        "anthropic" => &[
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
        ],
        "google" => &["GEMINI_API_KEY"],
        "google-vertex" => &["GOOGLE_CLOUD_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "azure" | "azure-openai-responses" => &["AZURE_OPENAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        "amazon-bedrock" => &["AWS_BEARER_TOKEN_BEDROCK"],
        _ => &[],
    };
    names
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    (
                        value,
                        if *name == "ANTHROPIC_OAUTH_TOKEN" {
                            "oauth"
                        } else {
                            "api_key"
                        },
                    )
                })
        })
        .or_else(|| {
            crate::gateway_definitions()
                .iter()
                .find(|entry| entry.id == provider)
                .and_then(|entry| {
                    entry.environment.iter().find_map(|name| {
                        std::env::var(name)
                            .ok()
                            .filter(|value| !value.trim().is_empty())
                    })
                })
                .map(|value| (value, "api_key"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, Config) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("models.json"), serde_json::to_vec(&json!({"providers": {
            "auth-fixture": {"api":"openai-completions", "baseUrl":"http://127.0.0.1:1", "apiKey":"fixture-secret", "models":[{"id":"fixture-model"}]}
        }})).unwrap()).unwrap();
        std::fs::write(
            directory.path().join("settings.json"),
            br#"{"defaultProvider":"auth-fixture","defaultModel":"fixture-model"}"#,
        )
        .unwrap();
        let mut config = Config::new(directory.path().to_owned(), directory.path().to_owned());
        config.api_key = None;
        (directory, config)
    }

    #[tokio::test]
    async fn default_probe_uses_configured_model_and_never_materializes_a_session() {
        let (directory, config) = fixture();
        let result = check_auth(&config, &AuthCheckOptions::default()).await;
        assert_eq!(result.status, AuthCheckStatus::Ready);
        assert_eq!(result.provider, "auth-fixture");
        assert_eq!(result.status.exit_code(), 0);
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("fixture-secret"));
        assert!(!wire.contains("credentials"));
        assert!(!directory.path().join("sessions").exists());
        assert!(!directory.path().join("auth.json").exists());
    }

    #[tokio::test]
    async fn explicit_model_and_secret_opt_in_share_provider_resolution() {
        let (_directory, config) = fixture();
        let result = check_auth(
            &config,
            &AuthCheckOptions {
                model: Some("auth-fixture/fixture-model".into()),
                credentials: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.status, AuthCheckStatus::Ready);
        assert_eq!(result.credentials.as_deref(), Some("fixture-secret"));
        let result = check_auth(
            &config,
            &AuthCheckOptions {
                provider: Some("missing-fixture".into()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.status, AuthCheckStatus::NotReady);
        assert_eq!(result.reason, Some("provider_not_found"));
        assert_eq!(result.status.exit_code(), 1);
    }

    #[tokio::test]
    async fn malformed_configuration_is_invalid_and_redacted() {
        let (directory, config) = fixture();
        std::fs::write(directory.path().join("auth.json"), "secret-invalid-json").unwrap();
        let result = check_auth(&config, &AuthCheckOptions::default()).await;
        assert_eq!(result.status, AuthCheckStatus::Invalid);
        assert_eq!(result.status.exit_code(), 2);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("secret-invalid-json")
        );
    }

    #[tokio::test]
    async fn no_refresh_reads_oauth_without_writing_and_failed_refresh_is_invalid() {
        let (directory, config) = fixture();
        std::fs::write(directory.path().join("auth.json"), br#"{"auth-fixture":{"type":"oauth","access":"expired-secret","refresh":"","expires":0}}"#).unwrap();
        std::fs::write(directory.path().join("models.json"), br#"{"providers":{"auth-fixture":{"api":"openai-completions","baseUrl":"http://127.0.0.1:1","models":[{"id":"fixture-model"}]}}}"#).unwrap();
        let result = check_auth(
            &config,
            &AuthCheckOptions {
                no_refresh: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.provider, "auth-fixture");
        assert_eq!(result.status, AuthCheckStatus::Ready);
        assert_eq!(result.auth_type, Some("oauth"));
        assert!(!directory.path().join("auth.json.lock").exists());
        let result = check_auth(&config, &AuthCheckOptions::default()).await;
        assert_eq!(result.status, AuthCheckStatus::Invalid);
        assert_eq!(
            read_stored_credential(directory.path(), "auth-fixture")
                .unwrap()
                .unwrap()
                .secret(),
            Some("expired-secret")
        );
    }
}
