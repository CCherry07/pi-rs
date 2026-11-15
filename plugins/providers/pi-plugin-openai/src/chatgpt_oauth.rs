//! OpenAI direct Responses token sharing. Distinct from Codex device OAuth.
use base64::Engine;
use rand::RngCore;
use reqwest::{Client, Url};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const REDIRECT: &str = "http://127.0.0.1:1455/auth/callback";
const RESOURCE: &str = "https://api.openai.com/v1";
const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";

pub struct Credential {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
}
pub struct Login {
    pub url: String,
    listener: TcpListener,
    verifier: String,
    state: String,
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
pub async fn start(device_id: &str) -> Result<Login, String> {
    let device = uuid::Uuid::parse_str(device_id)
        .map_err(|_| "ChatGPT login requires a UUID installation ID")?;
    let host = std::env::var("PI_OAUTH_CALLBACK_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let listener = TcpListener::bind((host.as_str(), 1455))
        .await
        .map_err(|error| format!("cannot bind ChatGPT OAuth callback port 1455: {error}"))?;
    let verifier = random();
    let state = random();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url =
        Url::parse("https://auth.openai.com/api/accounts/authorize").expect("constant OAuth URL");
    url.query_pairs_mut()
        .append_pair("client_id", "dynamic_agent_client")
        .append_pair("agent_name_hint", "Pi")
        .append_pair("ext_agent_host_id", &format!("urn:uuid:{device}"))
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("resource", RESOURCE)
        .append_pair(
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        )
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("nonce", &random());
    Ok(Login {
        url: url.into(),
        listener,
        verifier,
        state,
    })
}
fn callback(value: &str, state: &str) -> Result<(String, String), String> {
    let url = Url::parse(value).map_err(|_| "paste the complete ChatGPT callback URL")?;
    let expected = Url::parse(REDIRECT).expect("constant redirect");
    if url.origin() != expected.origin() || url.path() != expected.path() {
        return Err("unexpected ChatGPT callback URL".into());
    }
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    if query.get("state").map(|value| value.as_ref()) != Some(state) {
        return Err("ChatGPT OAuth state mismatch".into());
    }
    if query.contains_key("error") {
        return Err("ChatGPT authorization denied".into());
    }
    let required = |key| {
        query
            .get(key)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.to_string())
            .ok_or_else(|| format!("ChatGPT callback missing {key}"))
    };
    Ok((required("code")?, required("client_id")?))
}
impl Login {
    pub async fn complete_manual(&self, input: &str) -> Result<Credential, String> {
        let (code, client_id) = callback(input, &self.state)?;
        exchange(&code, &self.verifier, &client_id).await
    }
    pub async fn wait(self) -> Result<Credential, String> {
        tokio::time::timeout(Duration::from_secs(600), async {
            loop {
                let (mut socket,_) = self.listener.accept().await.map_err(|error| error.to_string())?;
                let mut bytes = Vec::new();
                let read = tokio::time::timeout(Duration::from_secs(5),async {
                    loop {
                        let mut chunk = [0u8;1024];
                        let size = socket.read(&mut chunk).await.map_err(|error| error.to_string())?;
                        if size == 0 { return Err("empty callback".to_string()); }
                        bytes.extend_from_slice(&chunk[..size]);
                        if bytes.len() > 16*1024 { return Err("oversized callback".to_string()); }
                        if bytes.windows(4).any(|part| part == b"\r\n\r\n") { return Ok(()); }
                    }
                }).await;
                if !matches!(read,Ok(Ok(()))) { continue; }
                let request = String::from_utf8_lossy(&bytes);
                let path = request.lines().next().and_then(|line| line.strip_prefix("GET ")).and_then(|line| line.split_whitespace().next()).unwrap_or("/");
                let result = callback(&format!("http://127.0.0.1:1455{path}"),&self.state);
                let (status, body) = if result.is_ok() { ("200 OK", "ChatGPT connected. You may close this window.") } else { ("400 Bad Request", "Invalid OAuth callback. Return to the login page.") };
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = socket.write_all(response.as_bytes()).await;
                if let Ok((code,client_id)) = result { return exchange(&code,&self.verifier,&client_id).await; }
            }
        }).await.map_err(|_| "ChatGPT login timed out".to_string())?
    }
}
async fn request(fields: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let response = Client::new()
        .post(TOKEN)
        .timeout(Duration::from_secs(30))
        .form(fields)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "ChatGPT token exchange failed: HTTP {}",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|_| "invalid ChatGPT token response".into())
}
fn parse(value: serde_json::Value, client_id: &str) -> Result<Credential, String> {
    let required = |key| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("ChatGPT token response missing {key}"))
    };
    let scopes = required("scope")?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !scopes.iter().any(|scope| scope == DIRECT_SCOPE) {
        return Err("ChatGPT grant lacks direct token scope".into());
    }
    let expires = value
        .get("expires_in")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or("invalid ChatGPT token expiry")?;
    Ok(Credential {
        access: required("access_token")?,
        refresh: required("refresh_token")?,
        expires: pi_utils::time::unix_timestamp_ms_u64()
            .saturating_add(expires.saturating_mul(1000))
            .saturating_sub(180_000),
        metadata: BTreeMap::from([
            ("clientId".into(), serde_json::json!(client_id)),
            ("scopes".into(), serde_json::json!(scopes)),
        ]),
    })
}
async fn exchange(code: &str, verifier: &str, client_id: &str) -> Result<Credential, String> {
    let value = request(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", REDIRECT),
        ("resource", RESOURCE),
    ])
    .await?;
    if value
        .get("id_token")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("ChatGPT token exchange missing ID token".into());
    }
    parse(value, client_id)
}
pub async fn refresh(refresh: &str, client_id: &str) -> Result<Credential, String> {
    if client_id.trim().is_empty() {
        return Err("stored OpenAI OAuth credential is missing clientId; reconnect ChatGPT".into());
    }
    let value = request(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh),
        ("resource", RESOURCE),
    ])
    .await?;
    parse(value, client_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_requires_matching_origin_state_and_issued_client() {
        assert_eq!(
            callback(
                "http://127.0.0.1:1455/auth/callback?state=s&code=c&client_id=issued",
                "s"
            )
            .unwrap(),
            ("c".into(), "issued".into())
        );
        for value in [
            "http://evil.example/auth/callback?state=s&code=c&client_id=issued",
            "http://127.0.0.1:1455/auth/callback?state=wrong&code=c&client_id=issued",
            "http://127.0.0.1:1455/auth/callback?state=s&code=c",
        ] {
            assert!(callback(value, "s").is_err());
        }
    }
    #[test]
    fn tokens_require_direct_scope_and_preserve_dynamic_client_id() {
        let token = serde_json::json!({"access_token":"a","refresh_token":"r","expires_in":3600,"scope":"openid chatgpt.tokens.use.direct"});
        let value = parse(token.clone(), "issued").unwrap();
        assert_eq!(value.metadata["clientId"], "issued");
        let mut invalid = token;
        invalid["scope"] = serde_json::json!("openid");
        assert!(parse(invalid, "issued").is_err());
    }
}
