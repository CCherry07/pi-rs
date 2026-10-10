//! Request-time credentials borrowed from the Codex CLI, including token rotation.
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use fs2::FileExt;
use pi_plugin::ProviderError;
use serde_json::Value;

use crate::oauth::{self, TokenResponse};

#[derive(Clone, Default)]
pub struct CodexCredentials {
    pub(crate) access_token: Option<String>,
    pub(crate) account_id: Option<String>,
    source: Option<PathBuf>,
}

impl std::fmt::Debug for CodexCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexCredentials")
            .field("configured", &self.is_configured())
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl CodexCredentials {
    pub fn discover() -> Self {
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return Self::discover_from([PathBuf::from(home).join("auth.json")]);
        }
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Self::default();
        };
        Self::discover_from([
            home.join(".codex/auth.json"),
            home.join(".config/codex/auth.json"),
        ])
    }

    pub fn discover_from(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        paths
            .into_iter()
            .find_map(|path| {
                let path = path.canonicalize().ok()?;
                Self::from_document(&read_document(&path).ok()?, path).ok()
            })
            .unwrap_or_default()
    }

    fn from_document(value: &Value, path: PathBuf) -> Result<Self, String> {
        let access = token_location(value, "access")
            .and_then(|pointer| value.pointer(pointer).and_then(Value::as_str))
            .ok_or("Codex login is missing an access token; run codex login again")?;
        let mut credentials = Self::from_access_token(access.trim());
        if !credentials.is_configured() {
            return Err(
                "Codex login token is missing chatgpt_account_id; run codex login again".into(),
            );
        }
        credentials.source = Some(path);
        Ok(credentials)
    }

    pub fn from_access_token(access_token: impl Into<String>) -> Self {
        let access_token = access_token.into();
        let claims = claims(&access_token);
        let account_id = claims.as_ref().and_then(|claims| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .or_else(|| claims.get("chatgpt_account_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
        Self {
            access_token: Some(access_token),
            account_id,
            source: None,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.access_token.is_some() && self.account_id.is_some()
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    fn expiring(&self) -> bool {
        self.access_token
            .as_deref()
            .and_then(claims)
            .and_then(|claims| claims.get("exp").and_then(Value::as_u64))
            .is_some_and(|expires| expires <= now_seconds().saturating_add(300))
    }

    pub(crate) async fn current(&self, force: bool) -> Result<Self, ProviderError> {
        self.current_with(force, |refresh| async move {
            oauth::refresh_tokens(&refresh).await
        })
        .await
        .map_err(|error| ProviderError::Failure(format!("Codex token refresh failed: {error}")))
    }

    async fn current_with<F, Fut>(&self, force: bool, refresh: F) -> Result<Self, String>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<TokenResponse, String>>,
    {
        let Some(path) = &self.source else {
            return Ok(self.clone());
        };
        let current = Self::from_document(&read_document(path)?, path.clone())?;
        if !current.expiring() && (!force || current.access_token != self.access_token) {
            return Ok(current);
        }
        // Lock a stable sidecar inode, not auth.json, which is atomically replaced.
        // All Pi processes/subagents using this login share the same lock.
        let _lock = refresh_lock(path).await?;
        let mut document = read_document(path)?;
        let current = Self::from_document(&document, path.clone())?;
        if !current.expiring() && (!force || current.access_token != self.access_token) {
            return Ok(current);
        }
        let refresh_pointer = token_location(&document, "refresh")
            .ok_or("Codex login has no refresh token; run codex login again")?;
        let refresh_token = document
            .pointer(refresh_pointer)
            .and_then(Value::as_str)
            .ok_or("Invalid Codex refresh token; run codex login again")?
            .to_string();
        let next = refresh(refresh_token).await?;
        let latest = read_document(path)?;
        // Codex itself does not acquire Pi's lock. Never overwrite a concurrent login.
        if latest != document {
            let latest = Self::from_document(&latest, path.clone())?;
            if !latest.expiring() {
                return Ok(latest);
            }
            return Err(
                "Codex login changed during refresh; retry after the login completes".into(),
            );
        }
        let access_pointer = token_location(&document, "access").expect("validated access token");
        *document
            .pointer_mut(access_pointer)
            .expect("validated access path") = Value::String(next.access_token);
        *document
            .pointer_mut(refresh_pointer)
            .expect("validated refresh path") = Value::String(next.refresh_token);
        if let Some(id_token) = next.id_token {
            if document.get("tokens").is_some_and(Value::is_object) {
                document["tokens"]["id_token"] = Value::String(id_token);
            } else {
                document["id_token"] = Value::String(id_token);
            }
        }
        let renewed = Self::from_document(&document, path.clone())?;
        if document.get("tokens").is_some_and(Value::is_object) {
            document["tokens"]["account_id"] =
                Value::String(renewed.account_id.clone().expect("validated account"));
        }
        document["last_refresh"] = Value::String(
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|error| error.to_string())?,
        );
        write_document(path, &document)?;
        Ok(renewed)
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn token_location(value: &Value, kind: &str) -> Option<&'static str> {
    let paths = if kind == "access" {
        [
            "/tokens/access_token",
            "/tokens/accessToken",
            "/access_token",
            "/accessToken",
        ]
    } else {
        [
            "/tokens/refresh_token",
            "/tokens/refreshToken",
            "/refresh_token",
            "/refreshToken",
        ]
    };
    paths.into_iter().find(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn read_document(path: &Path) -> Result<Value, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| format!("cannot read Codex login: {error}"))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Codex login file exceeds 1 MiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid Codex login JSON".into())
}

async fn refresh_lock(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(path.with_extension("pi-refresh.lock"))
        .map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(35);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if tokio::time::Instant::now() >= deadline {
                    return Err("timed out waiting for Codex token refresh lock".into());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn write_document(path: &Path, document: &Value) -> Result<(), String> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().expect("absolute credential path"))
            .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(&mut temporary, document).map_err(|error| error.to_string())?;
    temporary
        .write_all(b"\n")
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| format!("cannot persist refreshed Codex login: {}", error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn jwt(expires: u64) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"exp":expires,"chatgpt_account_id":"account"}).to_string());
        format!("header.{payload}.signature")
    }

    fn setup() -> (tempfile::TempDir, PathBuf, CodexCredentials) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        write_document(
            &path,
            &json!({"unknown":"preserve", "tokens":{
            "access_token":jwt(1), "refresh_token":"old-refresh", "id_token":"old-id"}}),
        )
        .unwrap();
        let credentials = CodexCredentials::discover_from([path.clone()]);
        (dir, path, credentials)
    }

    fn renewed() -> TokenResponse {
        TokenResponse {
            access_token: jwt(now_seconds() + 3600),
            refresh_token: "rotated-refresh".into(),
            id_token: Some("rotated-id".into()),
            expires_in: 3600,
        }
    }

    #[tokio::test]
    async fn concurrent_expired_logins_refresh_once_and_persist_rotation() {
        let (_dir, path, credentials) = setup();
        let calls = AtomicUsize::new(0);
        let refresh = |token: String| {
            let calls = &calls;
            async move {
                assert_eq!(token, "old-refresh");
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(renewed())
            }
        };
        let (a, b) = tokio::join!(
            credentials.current_with(false, refresh),
            credentials.current_with(false, refresh)
        );
        assert_eq!(a.unwrap().access_token, b.unwrap().access_token);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let saved = read_document(&path).unwrap();
        assert_eq!(saved["tokens"]["refresh_token"], "rotated-refresh");
        assert_eq!(saved["tokens"]["id_token"], "rotated-id");
        assert_eq!(saved["unknown"], "preserve");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn failed_refresh_keeps_login_and_releases_lock() {
        let (_dir, path, credentials) = setup();
        let original = std::fs::read(&path).unwrap();
        assert!(
            credentials
                .current_with(false, |_| async { Err("HTTP 401".into()) })
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            credentials
                .current_with(false, |_| async { Ok(renewed()) })
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn http_rejection_forces_refresh_even_before_jwt_expiry() {
        let (_dir, path, _) = setup();
        write_document(
            &path,
            &json!({"tokens":{
            "access_token":jwt(now_seconds()+7200), "refresh_token":"old-refresh"}}),
        )
        .unwrap();
        let credentials = CodexCredentials::discover_from([path.clone()]);
        let calls = AtomicUsize::new(0);
        credentials
            .current_with(true, |_| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(renewed())
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            read_document(&path).unwrap()["tokens"]["refresh_token"],
            "rotated-refresh"
        );
    }

    #[tokio::test]
    async fn concurrent_external_login_is_not_overwritten() {
        let (_dir, path, credentials) = setup();
        let updated =
            json!({"tokens":{"access_token":jwt(now_seconds()+7200),"refresh_token":"external"}});
        credentials
            .current_with(false, |_| async {
                write_document(&path, &updated).unwrap();
                Ok(renewed())
            })
            .await
            .unwrap();
        assert_eq!(read_document(&path).unwrap(), updated);
    }
}
