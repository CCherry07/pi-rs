//! Kimi Code's RFC 8628 device flow, matching Pi's current OAuth client.
use reqwest::{Client, Url};
use serde::Deserialize;
use std::time::Duration;

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";

#[derive(Clone, Deserialize)]
pub struct Credential {
    #[serde(rename = "access_token")]
    pub access: String,
    #[serde(rename = "refresh_token")]
    pub refresh: String,
    #[serde(rename = "expires_in")]
    pub expires: u64,
}

#[derive(Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    #[serde(default = "interval")]
    pub interval: u64,
    #[serde(default = "expiry")]
    pub expires_in: u64,
    #[serde(skip)]
    host: String,
}
fn interval() -> u64 {
    5
}
fn expiry() -> u64 {
    900
}
fn host() -> Result<String, String> {
    let value = std::env::var("KIMI_CODE_OAUTH_HOST")
        .or_else(|_| std::env::var("KIMI_OAUTH_HOST"))
        .unwrap_or_else(|_| "https://auth.kimi.com".into());
    trusted_url(&value)?;
    Ok(value.trim_end_matches('/').into())
}
fn trusted_url(value: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|_| "invalid Kimi OAuth URL")?;
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err("untrusted Kimi OAuth URL".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("invalid Kimi OAuth URL credentials".into());
    }
    Ok(())
}
fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("TLS HTTP client")
}
fn credential(value: serde_json::Value) -> Result<Credential, String> {
    let mut credential: Credential =
        serde_json::from_value(value).map_err(|_| "invalid Kimi OAuth token response")?;
    if credential.access.is_empty() || credential.refresh.is_empty() || credential.expires == 0 {
        return Err("incomplete Kimi OAuth token response".into());
    }
    credential.expires = pi_utils::time::unix_timestamp_ms_u64()
        .saturating_add(credential.expires.saturating_mul(1000));
    Ok(credential)
}
pub async fn start() -> Result<DeviceAuthorization, String> {
    let host = host()?;
    start_at(&host).await
}
async fn start_at(host: &str) -> Result<DeviceAuthorization, String> {
    let response = client()
        .post(format!("{host}/api/oauth/device_authorization"))
        .header("accept", "application/json")
        .form(&[("client_id", CLIENT_ID)])
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Kimi device authorization failed: HTTP {}",
            response.status()
        ));
    }
    let mut device: DeviceAuthorization = response
        .json()
        .await
        .map_err(|_| "invalid Kimi device response")?;
    trusted_url(&device.verification_uri)?;
    trusted_url(&device.verification_uri_complete)?;
    if device.device_code.is_empty() || device.user_code.is_empty() {
        return Err("incomplete Kimi device response".into());
    }
    device.interval = device.interval.max(1);
    device.expires_in = device.expires_in.max(1);
    device.host = host.to_owned();
    Ok(device)
}
pub async fn poll(device: &DeviceAuthorization) -> Result<Credential, String> {
    let mut interval = device.interval;
    tokio::time::timeout(Duration::from_secs(device.expires_in), async {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let response = client()
                .post(format!("{}/api/oauth/token", device.host))
                .header("accept", "application/json")
                .form(&[
                    ("client_id", CLIENT_ID),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status();
            let value: serde_json::Value = response
                .json()
                .await
                .map_err(|_| "invalid Kimi token response")?;
            if status.is_success() {
                return credential(value);
            }
            match value.get("error").and_then(serde_json::Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => {
                    interval = value
                        .get("interval")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(interval + 5)
                        .max(interval + 5)
                }
                Some("access_denied") => return Err("Kimi login denied".into()),
                Some("expired_token") => return Err("Kimi device code expired".into()),
                _ => return Err(format!("Kimi token polling failed: HTTP {status}")),
            }
        }
    })
    .await
    .map_err(|_| "Kimi device code expired".to_owned())?
}
pub async fn refresh(refresh: &str) -> Result<Credential, String> {
    let host = host()?;
    refresh_at(&host, refresh).await
}
async fn refresh_at(host: &str, refresh: &str) -> Result<Credential, String> {
    for attempt in 0..=3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
        let response = client()
            .post(format!("{host}/api/oauth/token"))
            .header("accept", "application/json")
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
            ])
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(_) if attempt < 3 => continue,
            Err(error) => return Err(error.to_string()),
        };
        let status = response.status();
        if (status.as_u16() == 429 || status.is_server_error()) && attempt < 3 {
            continue;
        }
        if !status.is_success() {
            return Err(format!("Kimi token refresh failed: HTTP {status}"));
        }
        return credential(
            response
                .json()
                .await
                .map_err(|_| "invalid Kimi token response")?,
        );
    }
    Err("Kimi token refresh failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn device_flow_waits_for_authorization_and_refresh_retries_server_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in [
                (
                    "200 OK",
                    serde_json::json!({"device_code":"device", "user_code":"user", "verification_uri":"https://auth.kimi.com/verify", "verification_uri_complete":"https://auth.kimi.com/verify?code=user", "interval":1, "expires_in":30}),
                ),
                (
                    "400 Bad Request",
                    serde_json::json!({"error":"authorization_pending"}),
                ),
                (
                    "200 OK",
                    serde_json::json!({"access_token":"access", "refresh_token":"refresh", "expires_in":3600}),
                ),
                (
                    "503 Service Unavailable",
                    serde_json::json!({"error":"unavailable"}),
                ),
                (
                    "200 OK",
                    serde_json::json!({"access_token":"rotated", "refresh_token":"rotated-refresh", "expires_in":3600}),
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0u8; 2048];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let body = body.to_string();
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        tokio::time::timeout(Duration::from_secs(15), async {
            let device = start_at(&host).await.unwrap();
            assert_eq!(device.user_code, "user");
            let token = poll(&device).await.unwrap();
            assert_eq!(token.access, "access");
            assert!(token.expires > pi_utils::time::unix_timestamp_ms_u64());
            let rotated = refresh_at(&host, &token.refresh).await.unwrap();
            assert_eq!(rotated.access, "rotated");
            assert_eq!(rotated.refresh, "rotated-refresh");
            let requests = server.await.unwrap();
            assert!(requests[0].starts_with("POST /api/oauth/device_authorization "));
            assert!(requests[0].contains(CLIENT_ID));
            assert!(requests[1].contains("device_code=device"));
            assert!(
                requests[1]
                    .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code")
            );
            assert!(requests[3].contains("refresh_token=refresh"));
            assert!(requests[4].contains("grant_type=refresh_token"));
        })
        .await
        .unwrap();
    }
}
