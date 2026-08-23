use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use reqwest::{Client, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const REDIRECT_URI: &str = "http://localhost:53692/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const REFRESH_SKEW_MS: u64 = 5 * 60 * 1_000;
const MAX_RESPONSE_BYTES: usize = 256 * 1_024;

#[derive(Debug, Clone)]
pub struct OAuthStart {
    pub url: String,
    pub verifier: String,
}

#[derive(Debug, Clone)]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

pub fn start_oauth() -> Result<OAuthStart, String> {
    let mut random = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut random);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url = Url::parse(AUTHORIZE_URL).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &verifier);
    Ok(OAuthStart {
        url: url.to_string(),
        verifier,
    })
}

pub async fn complete_oauth(input: &str, verifier: &str) -> Result<OAuthCredential, String> {
    let (code, state) = parse_authorization_input(input)?;
    if state.as_deref().is_some_and(|state| state != verifier) {
        return Err("Anthropic OAuth state mismatch".to_string());
    }
    let response = Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "state": state.unwrap_or_else(|| verifier.to_string()),
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier
        }))
        .send()
        .await
        .map_err(|error| format!("Anthropic token exchange failed: {error}"))?;
    parse_response(response, None, "exchange").await
}

pub async fn refresh(refresh_token: &str) -> Result<OAuthCredential, String> {
    if refresh_token.trim().is_empty() {
        return Err("Anthropic OAuth refresh token is empty".to_string());
    }
    let response = Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh_token
        }))
        .send()
        .await
        .map_err(|error| format!("Anthropic token refresh failed: {error}"))?;
    parse_response(response, Some(refresh_token), "refresh").await
}

async fn parse_response(
    response: reqwest::Response,
    previous_refresh: Option<&str>,
    operation: &str,
) -> Result<OAuthCredential, String> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read Anthropic OAuth response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "Anthropic OAuth response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    if !status.is_success() {
        return Err(format!(
            "Anthropic token {operation} failed (HTTP {})",
            status.as_u16()
        ));
    }
    let token: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid Anthropic token response: {error}"))?;
    if token.access_token.trim().is_empty() || token.expires_in == 0 {
        return Err("invalid Anthropic token response fields".to_string());
    }
    let refresh = token
        .refresh_token
        .or_else(|| previous_refresh.map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Anthropic token response is missing refresh_token".to_string())?;
    Ok(OAuthCredential {
        access: token.access_token,
        refresh,
        expires: now_ms()
            .saturating_add(token.expires_in.saturating_mul(1_000))
            .saturating_sub(REFRESH_SKEW_MS),
    })
}

fn parse_authorization_input(input: &str) -> Result<(String, Option<String>), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("missing Anthropic authorization code".to_string());
    }
    if let Ok(url) = Url::parse(input) {
        let code = url
            .query_pairs()
            .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
            .ok_or_else(|| "callback URL is missing code".to_string())?;
        let state = url
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()));
        return Ok((code, state));
    }
    if let Some((code, state)) = input.split_once('#') {
        return Ok((code.to_string(), Some(state.to_string())));
    }
    Ok((input.to_string(), None))
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
    fn start_uses_pkce_and_expected_identity() {
        let start = start_oauth().unwrap();
        let url = Url::parse(&start.url).unwrap();
        assert_eq!(url.host_str(), Some("claude.ai"));
        assert!(
            url.query_pairs()
                .any(|(key, value)| key == "state" && value == start.verifier)
        );
        assert!(
            url.query_pairs()
                .any(|(key, value)| key == "code_challenge" && !value.is_empty())
        );
    }

    #[test]
    fn callback_state_is_parsed() {
        assert_eq!(
            parse_authorization_input("http://localhost:53692/callback?code=abc&state=state")
                .unwrap(),
            ("abc".to_string(), Some("state".to_string()))
        );
    }
}
