use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, StatusCode};
use serde::Deserialize;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const TIMEOUT_SECONDS: u64 = 15 * 60;
const MAX_RESPONSE_BYTES: usize = 256 * 1_024;

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_auth_id: String,
    pub user_code: String,
    pub verification_uri: String,
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
struct DeviceResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(deserialize_with = "deserialize_interval")]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize, Default)]
struct ErrorResponse {
    error: Option<ErrorCode>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ErrorCode {
    Text(String),
    Object { code: Option<String> },
}

impl ErrorCode {
    fn code(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            Self::Object { code } => code.as_deref(),
        }
    }
}

pub async fn start_device_authorization() -> Result<DeviceAuthorization, String> {
    let response = Client::new()
        .post(USER_CODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({"client_id": CLIENT_ID}))
        .send()
        .await
        .map_err(|error| format!("OpenAI Codex device authorization failed: {error}"))?;
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(failure("device authorization", status));
    }
    let value: DeviceResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid OpenAI Codex device response: {error}"))?;
    if value.device_auth_id.trim().is_empty() || value.user_code.trim().is_empty() {
        return Err("invalid OpenAI Codex device response fields".to_string());
    }
    Ok(DeviceAuthorization {
        device_auth_id: value.device_auth_id,
        user_code: value.user_code,
        verification_uri: VERIFICATION_URI.to_string(),
        interval_seconds: value.interval.max(1),
        expires_in_seconds: TIMEOUT_SECONDS,
    })
}

pub async fn poll_device_authorization(
    authorization: &DeviceAuthorization,
) -> Result<OAuthCredential, String> {
    let client = Client::new();
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(authorization.expires_in_seconds);
    let mut interval = Duration::from_secs(authorization.interval_seconds.max(1));
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err("OpenAI Codex device code expired".to_string());
        }
        let response = client
            .post(DEVICE_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": authorization.device_auth_id,
                "user_code": authorization.user_code
            }))
            .send()
            .await
            .map_err(|error| format!("OpenAI Codex device polling failed: {error}"))?;
        let status = response.status();
        let bytes = bounded_body(response).await?;
        if status.is_success() {
            let code: DeviceTokenResponse = serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid OpenAI Codex device token response: {error}"))?;
            return exchange_code(&code.authorization_code, &code.code_verifier).await;
        }
        let error: ErrorResponse = serde_json::from_slice(&bytes).unwrap_or_default();
        match error.error.as_ref().and_then(ErrorCode::code) {
            Some("deviceauth_authorization_pending") | None
                if matches!(status.as_u16(), 403 | 404) => {}
            Some("slow_down") => interval += Duration::from_secs(5),
            Some(code) => return Err(format!("OpenAI Codex device authorization failed: {code}")),
            None => return Err(failure("device polling", status)),
        }
    }
}

pub async fn refresh(refresh_token: &str) -> Result<OAuthCredential, String> {
    let response = Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|error| format!("OpenAI Codex token refresh failed: {error}"))?;
    parse_token_response(response, "refresh").await
}

async fn exchange_code(code: &str, verifier: &str) -> Result<OAuthCredential, String> {
    let response = Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", DEVICE_REDIRECT_URI),
        ])
        .send()
        .await
        .map_err(|error| format!("OpenAI Codex token exchange failed: {error}"))?;
    parse_token_response(response, "exchange").await
}

async fn parse_token_response(
    response: reqwest::Response,
    operation: &str,
) -> Result<OAuthCredential, String> {
    let status = response.status();
    let bytes = bounded_body(response).await?;
    if !status.is_success() {
        return Err(failure(&format!("token {operation}"), status));
    }
    let value: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid OpenAI Codex token response: {error}"))?;
    if value.access_token.trim().is_empty()
        || value.refresh_token.trim().is_empty()
        || value.expires_in == 0
    {
        return Err("invalid OpenAI Codex token response fields".to_string());
    }
    Ok(OAuthCredential {
        access: value.access_token,
        refresh: value.refresh_token,
        expires: now_ms().saturating_add(value.expires_in.saturating_mul(1_000)),
    })
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read OpenAI Codex OAuth response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "OpenAI Codex OAuth response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    Ok(bytes.to_vec())
}

fn failure(action: &str, status: StatusCode) -> String {
    format!(
        "OpenAI Codex OAuth {action} failed (HTTP {})",
        status.as_u16()
    )
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Number(u64),
        Text(String),
    }
    match Value::deserialize(deserializer)? {
        Value::Number(value) => Ok(value),
        Value::Text(value) => value.parse().map_err(serde::de::Error::custom),
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
    fn accepts_string_device_poll_interval() {
        let value: DeviceResponse =
            serde_json::from_str(r#"{"device_auth_id":"id","user_code":"code","interval":"5"}"#)
                .unwrap();
        assert_eq!(value.interval, 5);
    }
}
