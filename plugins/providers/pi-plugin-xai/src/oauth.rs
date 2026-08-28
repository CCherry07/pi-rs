use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const DEFAULT_TOKEN_LIFETIME_SECONDS: u64 = 3_600;
const REFRESH_SKEW_MS: u64 = 5 * 60 * 1_000;
const MAX_RESPONSE_BYTES: usize = 256 * 1_024;

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval: Option<u64>,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct OAuthErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

pub async fn start_device_authorization() -> Result<DeviceAuthorization, String> {
    start_device_authorization_with_client(&Client::new()).await
}

async fn start_device_authorization_with_client(
    client: &Client,
) -> Result<DeviceAuthorization, String> {
    let response = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("referrer", "pi"),
        ])
        .send()
        .await
        .map_err(|error| format!("xAI device authorization request failed: {error}"))?;
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(oauth_failure("device authorization", status, &bytes));
    }
    let response: DeviceCodeResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("xAI device authorization returned invalid JSON: {error}"))?;
    validate_nonempty("device_code", &response.device_code)?;
    validate_nonempty("user_code", &response.user_code)?;
    let verification_uri = validate_verification_uri(&response.verification_uri)?;
    let verification_uri_complete = response
        .verification_uri_complete
        .as_deref()
        .map(validate_verification_uri)
        .transpose()?;
    if response.expires_in == 0 {
        return Err("invalid xAI OAuth response field: expires_in".to_string());
    }
    Ok(DeviceAuthorization {
        device_code: response.device_code,
        user_code: response.user_code,
        verification_uri,
        verification_uri_complete,
        interval_seconds: response.interval.filter(|value| *value > 0).unwrap_or(5),
        expires_in_seconds: response.expires_in,
    })
}

pub async fn poll_device_authorization(
    authorization: &DeviceAuthorization,
) -> Result<OAuthCredential, String> {
    let client = Client::new();
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_secs(authorization.expires_in_seconds);
    let mut interval = Duration::from_secs(authorization.interval_seconds.max(1));
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err("xAI device code expired".to_string());
        }
        let response = client
            .post(TOKEN_URL)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", CLIENT_ID),
                ("device_code", authorization.device_code.as_str()),
            ])
            .send()
            .await
            .map_err(|error| format!("xAI device token polling failed: {error}"))?;
        let status = response.status();
        let bytes = bounded_body(response).await?;
        if status.is_success() {
            return parse_token_response(&bytes, None);
        }
        let error: OAuthErrorResponse = serde_json::from_slice(&bytes).unwrap_or_default();
        match error.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval =
                    Duration::from_secs(error.interval.unwrap_or(interval.as_secs() + 5).max(1));
            }
            Some("access_denied" | "authorization_denied") => {
                return Err("xAI device authorization was denied".to_string());
            }
            Some("expired_token") => return Err("xAI device code expired".to_string()),
            _ => return Err(oauth_failure("device token polling", status, &bytes)),
        }
    }
}

pub async fn refresh(refresh_token: &str) -> Result<OAuthCredential, String> {
    validate_nonempty("refresh_token", refresh_token)?;
    let response = Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|error| format!("xAI token refresh request failed: {error}"))?;
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(oauth_failure("token refresh", status, &bytes));
    }
    parse_token_response(&bytes, Some(refresh_token))
}

fn parse_token_response(
    bytes: &[u8],
    previous_refresh_token: Option<&str>,
) -> Result<OAuthCredential, String> {
    let response: TokenResponse = serde_json::from_slice(bytes)
        .map_err(|error| format!("xAI token response returned invalid JSON: {error}"))?;
    validate_nonempty("access_token", &response.access_token)?;
    let refresh = response
        .refresh_token
        .or_else(|| previous_refresh_token.map(str::to_string))
        .ok_or_else(|| "invalid xAI OAuth response field: refresh_token".to_string())?;
    validate_nonempty("refresh_token", &refresh)?;
    let lifetime = response
        .expires_in
        .unwrap_or(DEFAULT_TOKEN_LIFETIME_SECONDS)
        .max(1);
    Ok(OAuthCredential {
        access: response.access_token,
        refresh,
        expires: now_ms()
            .saturating_add(lifetime.saturating_mul(1_000))
            .saturating_sub(REFRESH_SKEW_MS),
    })
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read xAI OAuth response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "xAI OAuth response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    Ok(bytes.to_vec())
}

fn validate_verification_uri(raw: &str) -> Result<String, String> {
    let url = Url::parse(raw)
        .map_err(|_| "untrusted verification URI in xAI OAuth response".to_string())?;
    if url.scheme() != "https" || url.host_str() != Some("auth.x.ai") {
        return Err("untrusted verification URI in xAI OAuth response".to_string());
    }
    Ok(url.to_string())
}

fn validate_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("invalid xAI OAuth response field: {field}"))
    } else {
        Ok(())
    }
}

fn oauth_failure(action: &str, status: StatusCode, bytes: &[u8]) -> String {
    let body: OAuthErrorResponse = serde_json::from_slice(bytes).unwrap_or_default();
    let detail = [body.error, body.error_description]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(": ");
    if detail.is_empty() {
        format!("xAI OAuth {action} failed (HTTP {})", status.as_u16())
    } else {
        format!(
            "xAI OAuth {action} failed (HTTP {}): {detail}",
            status.as_u16()
        )
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_xai_verification_urls() {
        assert!(validate_verification_uri("file:///tmp/token").is_err());
        assert!(validate_verification_uri("https://example.com/login").is_err());
        assert!(validate_verification_uri("https://auth.x.ai/device").is_ok());
    }

    #[test]
    fn token_response_preserves_rotated_or_previous_refresh_token() {
        let credential = parse_token_response(
            br#"{"access_token":"access","expires_in":3600}"#,
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(credential.refresh, "old-refresh");
        assert!(credential.expires > now_ms());
    }
}
