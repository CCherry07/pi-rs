use std::time::Duration;

use base64::Engine;
use rand::RngCore;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: usize = 16 * 1_024;
const MAX_RESPONSE_BYTES: usize = 256 * 1_024;
const PERMANENT_EXPIRY_MS: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone)]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
}

pub struct OAuthLogin {
    pub url: String,
    pub callback_url: String,
    verifier: String,
    callback_path: String,
    listener: TcpListener,
}

#[derive(Debug, Deserialize)]
struct KeyResponse {
    key: String,
}

pub async fn start_oauth() -> Result<OAuthLogin, String> {
    let callback_host = callback_host()?;
    let listener = TcpListener::bind((callback_host.as_str(), 0))
        .await
        .map_err(|error| format!("failed to start OpenRouter OAuth callback server: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("failed to inspect OpenRouter OAuth callback server: {error}"))?
        .port();
    let mut random = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut random);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let callback_path = format!(
        "/oauth/callback/{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&random[..16])
    );
    let callback_authority = if callback_host.contains(':') {
        format!("[{callback_host}]")
    } else {
        callback_host
    };
    let callback_url = format!("http://{callback_authority}:{port}{callback_path}");
    let mut url = Url::parse(AUTHORIZE_URL).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("callback_url", &callback_url)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(OAuthLogin {
        url: url.to_string(),
        callback_url,
        verifier,
        callback_path,
        listener,
    })
}

impl OAuthLogin {
    pub async fn wait_for_credential(self) -> Result<OAuthCredential, String> {
        tokio::time::timeout(LOGIN_TIMEOUT, self.wait_for_callback())
            .await
            .map_err(|_| "OpenRouter OAuth login timed out".to_string())?
    }

    async fn wait_for_callback(self) -> Result<OAuthCredential, String> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|error| format!("OpenRouter OAuth callback failed: {error}"))?;
            match read_callback(&mut stream, &self.callback_path).await {
                Ok(Callback::Ignore) => {
                    send_html(
                        &mut stream,
                        404,
                        "OpenRouter OAuth callback route not found.",
                    )
                    .await?;
                }
                Ok(Callback::Denied(message)) => {
                    send_html(&mut stream, 400, "OpenRouter authorization was denied.").await?;
                    return Err(format!("OpenRouter authorization failed: {message}"));
                }
                Ok(Callback::Code(code)) => {
                    let credential = exchange_authorization_code(&code, &self.verifier).await;
                    match &credential {
                        Ok(_) => {
                            send_html(
                                &mut stream,
                                200,
                                "Signed in to OpenRouter. You may now close this page.",
                            )
                            .await?;
                        }
                        Err(_) => {
                            send_html(&mut stream, 502, "OpenRouter key exchange failed.").await?;
                        }
                    }
                    return credential;
                }
                Err(error) => {
                    let _ = send_html(&mut stream, 400, "Invalid OpenRouter OAuth callback.").await;
                    return Err(error);
                }
            }
        }
    }
}

enum Callback {
    Ignore,
    Denied(String),
    Code(String),
}

async fn read_callback(stream: &mut TcpStream, callback_path: &str) -> Result<Callback, String> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1_024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| format!("failed to read OpenRouter OAuth callback: {error}"))?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if request.len() > MAX_REQUEST_BYTES {
            return Err("OpenRouter OAuth callback request is too large".to_string());
        }
    }
    let request = std::str::from_utf8(&request)
        .map_err(|_| "OpenRouter OAuth callback is not valid UTF-8".to_string())?;
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| "OpenRouter OAuth callback request is empty".to_string())?;
    let mut fields = first_line.split_whitespace();
    let method = fields.next().unwrap_or_default();
    let target = fields.next().unwrap_or_default();
    if method != "GET" || target.is_empty() {
        return Ok(Callback::Ignore);
    }
    let url = Url::parse(&format!("http://localhost{target}"))
        .map_err(|error| format!("invalid OpenRouter OAuth callback URL: {error}"))?;
    if url.path() != callback_path {
        return Ok(Callback::Ignore);
    }
    if let Some(error) = query_value(&url, "error") {
        return Ok(Callback::Denied(
            query_value(&url, "error_description").unwrap_or(error),
        ));
    }
    let code = query_value(&url, "code")
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(|| "OpenRouter OAuth callback is missing code".to_string())?;
    Ok(Callback::Code(code))
}

fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
) -> Result<OAuthCredential, String> {
    let response = Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256"
        }))
        .timeout(EXCHANGE_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("OpenRouter OAuth key exchange failed: {error}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read OpenRouter OAuth response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "OpenRouter OAuth response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    if !status.is_success() {
        return Err(openrouter_error(status, &bytes));
    }
    let response: KeyResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid OpenRouter OAuth response: {error}"))?;
    if response.key.trim().is_empty() || response.key.contains(['\r', '\n']) {
        return Err("OpenRouter OAuth response carries no valid key".to_string());
    }
    Ok(OAuthCredential {
        access: response.key,
        refresh: String::new(),
        expires: PERMANENT_EXPIRY_MS,
    })
}

fn openrouter_error(status: StatusCode, bytes: &[u8]) -> String {
    let detail = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("error_description")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error").and_then(|error| error.get("message")))
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    detail.map_or_else(
        || {
            format!(
                "OpenRouter OAuth key exchange failed (HTTP {})",
                status.as_u16()
            )
        },
        |detail| {
            format!(
                "OpenRouter OAuth key exchange failed (HTTP {}): {}",
                status.as_u16(),
                detail.chars().take(2_000).collect::<String>()
            )
        },
    )
}

async fn send_html(stream: &mut TcpStream, status: u16, message: &str) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>OpenRouter OAuth</title><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| format!("failed to answer OpenRouter OAuth callback: {error}"))
}

fn callback_host() -> Result<String, String> {
    let host = std::env::var("PI_OAUTH_CALLBACK_HOST")
        .ok()
        .filter(|host| !host.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let parsed = Url::parse(&format!("http://{host}"))
        .map_err(|_| "PI_OAUTH_CALLBACK_HOST must be a hostname or IP address".to_string())?;
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host_str().is_none()
    {
        return Err("PI_OAUTH_CALLBACK_HOST must be a hostname or IP address".to_string());
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_uses_loopback_callback_and_pkce() {
        let login = start_oauth().await.unwrap();
        let url = Url::parse(&login.url).unwrap();
        assert_eq!(url.host_str(), Some("openrouter.ai"));
        assert!(
            url.query_pairs()
                .any(|(name, value)| name == "code_challenge" && !value.is_empty())
        );
        assert!(login.callback_url.starts_with("http://127.0.0.1:"));
    }

    #[tokio::test]
    async fn callback_parser_rejects_other_routes_and_extracts_code() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(b"GET /expected?code=abc HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
            read_callback(&mut stream, "/expected").await.unwrap(),
            Callback::Code(code) if code == "abc"
        ));
        client.await.unwrap();
    }
}
