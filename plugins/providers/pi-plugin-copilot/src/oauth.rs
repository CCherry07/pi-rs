use std::collections::BTreeSet;
use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const USER_AGENT: &str = "GitHubCopilotChat/0.35.0";
const MAX_RESPONSE_BYTES: usize = 256 * 1_024;
const DEFAULT_INTERVAL_SECONDS: u64 = 5;
const EXPIRY_SKEW_MS: u64 = 5 * 60 * 1_000;
const COPILOT_API_VERSION: &str = "2026-06-01";

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
    pub enterprise_domain: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthCredential {
    /// Short-lived Copilot inference token.
    pub access: String,
    /// Long-lived GitHub access token used to mint another Copilot token.
    pub refresh: String,
    pub expires: u64,
    pub enterprise_domain: Option<String>,
    pub available_model_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    interval: Option<u64>,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
}

pub fn normalize_enterprise_domain(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let candidate = if value.contains("://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    let url = Url::parse(&candidate)
        .map_err(|error| format!("invalid GitHub Enterprise URL/domain: {error}"))?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(
            "GitHub Enterprise must be an HTTPS hostname without credentials or a port".to_string(),
        );
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "GitHub Enterprise hostname is missing".to_string())?;
    Ok(Some(host.to_ascii_lowercase()))
}

pub async fn start_device_authorization(
    enterprise_domain: Option<&str>,
) -> Result<DeviceAuthorization, String> {
    let enterprise_domain = enterprise_domain
        .map(normalize_enterprise_domain)
        .transpose()?
        .flatten();
    let domain = enterprise_domain.as_deref().unwrap_or("github.com");
    let response = Client::new()
        .post(format!("https://{domain}/login/device/code"))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .form(&[("client_id", CLIENT_ID), ("scope", "read:user")])
        .send()
        .await
        .map_err(|error| format!("GitHub Copilot device authorization failed: {error}"))?;
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(failure("device authorization", status, &bytes));
    }
    let value: DeviceCodeResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid GitHub Copilot device response: {error}"))?;
    let verification_uri = validate_verification_uri(&value.verification_uri)?;
    if value.device_code.trim().is_empty()
        || value.user_code.trim().is_empty()
        || value.expires_in == 0
    {
        return Err("invalid GitHub Copilot device response fields".to_string());
    }
    Ok(DeviceAuthorization {
        device_code: value.device_code,
        user_code: value.user_code,
        verification_uri,
        interval_seconds: value.interval.unwrap_or(DEFAULT_INTERVAL_SECONDS).max(1),
        expires_in_seconds: value.expires_in,
        enterprise_domain,
    })
}

pub async fn poll_device_authorization(
    authorization: &DeviceAuthorization,
) -> Result<OAuthCredential, String> {
    let domain = authorization
        .enterprise_domain
        .as_deref()
        .unwrap_or("github.com");
    let client = Client::new();
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(authorization.expires_in_seconds);
    let mut interval = Duration::from_secs(authorization.interval_seconds.max(1));
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err("GitHub Copilot device code expired".to_string());
        }
        let response = client
            .post(format!("https://{domain}/login/oauth/access_token"))
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT)
            .form(&[
                ("client_id", CLIENT_ID),
                ("device_code", authorization.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|error| format!("GitHub Copilot device polling failed: {error}"))?;
        let status = response.status();
        let bytes = bounded_body(response).await?;
        if !status.is_success() {
            return Err(failure("device polling", status, &bytes));
        }
        let value: DeviceTokenResponse = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid GitHub device token response: {error}"))?;
        if let Some(access_token) = value
            .access_token
            .filter(|access_token| !access_token.trim().is_empty())
        {
            let credential =
                mint_copilot_token(&access_token, authorization.enterprise_domain.as_deref())
                    .await?;
            return attach_available_models(credential, true).await;
        }
        match value.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = Duration::from_secs(
                    value
                        .interval
                        .unwrap_or(interval.as_secs().saturating_add(5))
                        .max(interval.as_secs().saturating_add(1)),
                );
            }
            Some(error) => {
                let suffix = value
                    .error_description
                    .filter(|message| !message.trim().is_empty())
                    .map_or(String::new(), |message| format!(": {message}"));
                return Err(format!("GitHub device flow failed: {error}{suffix}"));
            }
            None => return Err("invalid GitHub device token response fields".to_string()),
        }
    }
}

pub async fn refresh(
    github_access_token: &str,
    enterprise_domain: Option<&str>,
) -> Result<OAuthCredential, String> {
    let credential = mint_copilot_token(github_access_token, enterprise_domain).await?;
    attach_available_models(credential, false).await
}

async fn mint_copilot_token(
    github_access_token: &str,
    enterprise_domain: Option<&str>,
) -> Result<OAuthCredential, String> {
    let enterprise_domain = enterprise_domain
        .map(normalize_enterprise_domain)
        .transpose()?
        .flatten();
    let domain = enterprise_domain.as_deref().unwrap_or("github.com");
    let response = Client::new()
        .get(format!("https://api.{domain}/copilot_internal/v2/token"))
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {github_access_token}"))
        .header("User-Agent", USER_AGENT)
        .header("Editor-Version", "vscode/1.107.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
        .header("Copilot-Integration-Id", "vscode-chat")
        .send()
        .await
        .map_err(|error| format!("GitHub Copilot token refresh failed: {error}"))?;
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(failure("token refresh", status, &bytes));
    }
    let value: CopilotTokenResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid GitHub Copilot token response: {error}"))?;
    if value.token.trim().is_empty() || value.expires_at == 0 {
        return Err("invalid GitHub Copilot token response fields".to_string());
    }
    Ok(OAuthCredential {
        access: value.token,
        refresh: github_access_token.to_string(),
        expires: value
            .expires_at
            .saturating_mul(1_000)
            .saturating_sub(EXPIRY_SKEW_MS),
        enterprise_domain,
        available_model_ids: Vec::new(),
    })
}

async fn attach_available_models(
    mut credential: OAuthCredential,
    enable_policy_models: bool,
) -> Result<OAuthCredential, String> {
    let base_url = crate::base_url_from_token(&credential.access)
        .or_else(|| {
            credential
                .enterprise_domain
                .as_ref()
                .map(|domain| format!("https://copilot-api.{domain}"))
        })
        .unwrap_or_else(|| crate::COPILOT_BASE_URL.to_string());
    let client = Client::new();
    let response = client
        .get(format!("{base_url}/models"))
        .headers(copilot_headers(&credential.access)?)
        .send()
        .await
        .map_err(|error| format!("GitHub Copilot model catalog request failed: {error}"))?;
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(failure("model catalog request", status, &bytes));
    }
    let catalog = parse_model_catalog(
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid GitHub Copilot models response: {error}"))?,
        base_url == crate::COPILOT_BASE_URL,
    )?;
    let mut available = catalog.available;
    if enable_policy_models {
        for model in catalog.policy {
            let mut url = Url::parse(&base_url)
                .map_err(|error| format!("invalid GitHub Copilot API URL: {error}"))?;
            url.path_segments_mut()
                .map_err(|_| "invalid GitHub Copilot API URL".to_string())?
                .extend(["models", model.as_str(), "policy"]);
            let response = client
                .post(url)
                .headers(copilot_headers(&credential.access)?)
                .header("Content-Type", "application/json")
                .header("openai-intent", "chat-policy")
                .header("x-interaction-type", "chat-policy")
                .json(&serde_json::json!({"state": "enabled"}))
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    available.insert(model);
                }
                Ok(response) if response.status() == StatusCode::TOO_MANY_REQUESTS => break,
                Ok(_) | Err(_) => {}
            }
        }
    }
    credential.available_model_ids = available.into_iter().collect();
    Ok(credential)
}

fn copilot_headers(token: &str) -> Result<reqwest::header::HeaderMap, String> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    let authorization = format!("Bearer {token}");
    for (name, value) in [
        ("accept", "application/json"),
        ("authorization", authorization.as_str()),
        ("user-agent", USER_AGENT),
        ("editor-version", "vscode/1.107.0"),
        ("editor-plugin-version", "copilot-chat/0.35.0"),
        ("copilot-integration-id", "vscode-chat"),
        ("x-github-api-version", COPILOT_API_VERSION),
    ] {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("invalid GitHub Copilot header name: {error}"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| "invalid GitHub Copilot token header".to_string())?;
        headers.insert(name, value);
    }
    Ok(headers)
}

struct ModelCatalog {
    available: BTreeSet<String>,
    policy: Vec<String>,
}

fn parse_model_catalog(
    value: serde_json::Value,
    allow_policy_fallback: bool,
) -> Result<ModelCatalog, String> {
    let items = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "invalid GitHub Copilot models response".to_string())?;
    let known = crate::github_copilot_models()
        .into_iter()
        .map(|model| model.id.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let models = items
        .iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?;
            if item
                .pointer("/capabilities/supports/tool_calls")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
            {
                return None;
            }
            Some((
                id.to_string(),
                item.get("model_picker_enabled")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
                item.pointer("/policy/state")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            ))
        })
        .collect::<Vec<_>>();
    let mut available = models
        .iter()
        .filter(|(_, picker, state)| *picker && state.as_deref() != Some("disabled"))
        .map(|(id, _, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let use_policy_fallback = allow_policy_fallback && available.is_empty();
    if use_policy_fallback {
        available.extend(
            models
                .iter()
                .filter(|(_, _, state)| state.as_deref() == Some("enabled"))
                .map(|(id, _, _)| id.clone()),
        );
    }
    let policy = models
        .into_iter()
        .filter(|(id, picker, state)| {
            state.as_deref() == Some("unconfigured")
                && known.contains(id)
                && (*picker || use_policy_fallback)
        })
        .map(|(id, _, _)| id)
        .collect();
    Ok(ModelCatalog { available, policy })
}

fn validate_verification_uri(value: &str) -> Result<String, String> {
    let url = Url::parse(value)
        .map_err(|_| "untrusted verification_uri in GitHub device response".to_string())?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err("untrusted verification_uri in GitHub device response".to_string());
    }
    Ok(url.to_string())
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read GitHub Copilot response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "GitHub Copilot response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    Ok(bytes.to_vec())
}

fn failure(action: &str, status: StatusCode, body: &[u8]) -> String {
    let detail = String::from_utf8_lossy(body);
    let detail = detail.trim();
    if detail.is_empty() {
        format!("GitHub Copilot {action} failed (HTTP {})", status.as_u16())
    } else {
        format!(
            "GitHub Copilot {action} failed (HTTP {}): {}",
            status.as_u16(),
            truncate(detail, 2_000)
        )
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enterprise_domain_is_normalized_and_rejects_unsafe_urls() {
        assert_eq!(
            normalize_enterprise_domain("https://Company.GHE.com/path").unwrap(),
            Some("company.ghe.com".to_string())
        );
        assert!(normalize_enterprise_domain("http://example.com").is_err());
        assert!(normalize_enterprise_domain("https://user@example.com").is_err());
    }

    #[test]
    fn account_catalog_filters_disabled_and_non_tool_models() {
        let catalog = parse_model_catalog(
            serde_json::json!({"data": [
                {"id":"gpt-5.4","model_picker_enabled":true,"policy":{"state":"enabled"},"capabilities":{"supports":{"tool_calls":true}}},
                {"id":"grok-4.6","model_picker_enabled":true,"policy":{"state":"disabled"},"capabilities":{"supports":{"tool_calls":true}}},
                {"id":"gpt-4.1","model_picker_enabled":true,"policy":{"state":"enabled"},"capabilities":{"supports":{"tool_calls":false}}},
                {"id":"claude-sonnet-4.6","model_picker_enabled":true,"policy":{"state":"unconfigured"},"capabilities":{"supports":{"tool_calls":true}}}
            ]}),
            true,
        )
        .unwrap();
        assert_eq!(
            catalog.available,
            BTreeSet::from(["claude-sonnet-4.6".to_string(), "gpt-5.4".to_string()])
        );
        assert_eq!(catalog.policy, vec!["claude-sonnet-4.6".to_string()]);
    }
}
