//! Authentication building blocks for Streamable HTTP MCP servers.
//!
//! This module deliberately contains no terminal or Coding-product policy. Hosts
//! inject provider credential lookup and drive OAuth redirects with their own UI.

mod refresh;

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fs2::FileExt;
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use pi_core::AbortSignal;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationMetadata, AuthorizationRequest,
    AuthorizationSession, CredentialStore, OAuthClientConfig, OAuthTokenResponse, StateStore,
    StoredAuthorizationState, StoredCredentials,
};
use rmcp::transport::streamable_http_client::{
    SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use serde::{Deserialize, Serialize};
use sse_stream::Sse;

use crate::{McpOAuthClientRegistration, McpOAuthConfig};

const MAX_STORE_BYTES: u64 = 1024 * 1024;

/// Resolves a configured Pi provider token at request time.
///
/// Implementations must not log or persist returned tokens. Returning `None`
/// sends the request without a bearer token so the remote server can return its
/// ordinary authentication challenge.
#[async_trait]
pub trait McpProviderTokenResolver: Send + Sync {
    async fn token(&self, provider: &str) -> Result<Option<String>, String>;
}

/// Connection-time authentication dependencies. They are immutable inputs to a
/// candidate generation, while credential values are read for every request.
#[derive(Clone)]
pub struct McpClientAuth {
    pub(crate) oauth_store: McpOAuthCredentialStore,
    pub(crate) provider_tokens: Option<Arc<dyn McpProviderTokenResolver>>,
    pub(crate) local_values: Option<crate::config::LocalValues>,
}

impl Default for McpClientAuth {
    fn default() -> Self {
        Self {
            oauth_store: McpOAuthCredentialStore::memory(),
            provider_tokens: None,
            local_values: None,
        }
    }
}

impl McpClientAuth {
    pub fn with_oauth_store(mut self, store: McpOAuthCredentialStore) -> Self {
        self.oauth_store = store;
        self
    }

    pub fn with_provider_tokens(mut self, resolver: Arc<dyn McpProviderTokenResolver>) -> Self {
        self.provider_tokens = Some(resolver);
        self
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpOAuthTokens {
    pub access_token: String,
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl std::fmt::Debug for McpOAuthTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthTokens")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct AuthDocument {
    #[serde(default)]
    servers: BTreeMap<String, ServerAuthState>,
}

#[derive(Default, Serialize, Deserialize)]
struct ServerAuthState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credentials: Option<StoredCredentials>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pending: BTreeMap<String, StoredAuthorizationState>,
}

#[derive(Clone)]
enum StoreBackend {
    File(PathBuf),
    Memory(Arc<Mutex<AuthDocument>>),
}

/// Durable OAuth credentials keyed by MCP server name and canonical URL.
///
/// Different names for the same URL intentionally keep separate accounts. The
/// file backend uses a sibling lock and atomic replacement; diagnostics never
/// include stored JSON or credential values.
#[derive(Clone)]
pub struct McpOAuthCredentialStore {
    backend: StoreBackend,
}

impl std::fmt::Debug for McpOAuthCredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialStore")
            .field(
                "backend",
                &match &self.backend {
                    StoreBackend::File(path) => path.display().to_string(),
                    StoreBackend::Memory(_) => "memory".into(),
                },
            )
            .finish()
    }
}

impl McpOAuthCredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: StoreBackend::File(path.into()),
        }
    }

    pub fn memory() -> Self {
        Self {
            backend: StoreBackend::Memory(Arc::new(Mutex::new(AuthDocument::default()))),
        }
    }

    pub fn has_credentials(&self, name: &str, server_url: &str) -> Result<bool, String> {
        let key = credential_key(name, server_url)?;
        self.read(|document| {
            document
                .servers
                .get(&key)
                .and_then(|state| state.credentials.as_ref())
                .is_some_and(|credentials| credentials.token_response.is_some())
        })
    }

    /// Opaque revision of completed credentials only. Pending authorization
    /// state and client registration alone must not trigger a ready generation.
    pub(crate) fn credentials_revision(
        &self,
        identities: &[(String, String)],
    ) -> Result<[u8; 32], String> {
        use sha2::{Digest, Sha256};
        let keys = identities
            .iter()
            .map(|(name, url)| credential_key(name, url))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        self.read(|document| {
            let credentials = document
                .servers
                .iter()
                .filter(|(key, _)| keys.contains(*key))
                .filter_map(|(key, state)| {
                    state
                        .credentials
                        .as_ref()
                        .filter(|credentials| credentials.token_response.is_some())
                        .map(|credentials| (key, credentials))
                })
                .collect::<BTreeMap<_, _>>();
            serde_json::to_vec(&credentials)
                .map(|bytes| Sha256::digest(bytes).into())
                .map_err(|_| "MCP OAuth credential revision failed".to_string())
        })?
    }

    pub fn save_tokens(
        &self,
        name: &str,
        server_url: &str,
        client_id: &str,
        tokens: McpOAuthTokens,
    ) -> Result<(), String> {
        let key = credential_key(name, server_url)?;
        let granted_scopes = tokens
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let token_response: OAuthTokenResponse = serde_json::from_value(serde_json::json!({
            "access_token": tokens.access_token,
            "token_type": tokens.token_type,
            "refresh_token": tokens.refresh_token,
            "expires_in": tokens.expires_in,
            "scope": tokens.scope,
        }))
        .map_err(|_| "Invalid OAuth tokens".to_string())?;
        let received = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs());
        self.update(|document| {
            document.servers.entry(key).or_default().credentials = Some(StoredCredentials::new(
                client_id.to_string(),
                Some(token_response),
                granted_scopes,
                received,
            ));
        })
    }

    /// Deletes credentials and pending authorization state for this exact name
    /// and URL. Returns whether anything was stored.
    pub fn sign_out(&self, name: &str, server_url: &str) -> Result<bool, String> {
        let key = credential_key(name, server_url)?;
        self.update_result(|document| document.servers.remove(&key).is_some())
    }

    pub(crate) fn for_server(&self, name: &str, server_url: &str) -> Result<ServerStore, String> {
        Ok(ServerStore {
            store: self.clone(),
            key: credential_key(name, server_url)?,
        })
    }

    fn read<T>(&self, read: impl FnOnce(&AuthDocument) -> T) -> Result<T, String> {
        match &self.backend {
            StoreBackend::Memory(document) => Ok(read(&document.lock().unwrap())),
            StoreBackend::File(path) => {
                with_file_document(path, |document| Ok((read(document), false)))
            }
        }
    }

    fn update(&self, update: impl FnOnce(&mut AuthDocument)) -> Result<(), String> {
        self.update_result(|document| update(document))
    }

    fn update_result<T>(&self, update: impl FnOnce(&mut AuthDocument) -> T) -> Result<T, String> {
        match &self.backend {
            StoreBackend::Memory(document) => Ok(update(&mut document.lock().unwrap())),
            StoreBackend::File(path) => {
                with_file_document(path, |document| Ok((update(document), true)))
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ServerStore {
    store: McpOAuthCredentialStore,
    key: String,
}

#[async_trait]
impl CredentialStore for ServerStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.store
            .read(|document| {
                document
                    .servers
                    .get(&self.key)
                    .and_then(|state| state.credentials.clone())
            })
            .map_err(AuthError::CredentialStoreError)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.store
            .update(|document| {
                document
                    .servers
                    .entry(self.key.clone())
                    .or_default()
                    .credentials = Some(credentials);
            })
            .map_err(AuthError::CredentialStoreError)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.store
            .update(|document| {
                document.servers.remove(&self.key);
            })
            .map_err(AuthError::CredentialStoreError)
    }
}

#[async_trait]
impl StateStore for ServerStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        self.store
            .update(|document| {
                document
                    .servers
                    .entry(self.key.clone())
                    .or_default()
                    .pending
                    .insert(csrf_token.to_string(), state);
            })
            .map_err(AuthError::CredentialStoreError)
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        self.store
            .read(|document| {
                document
                    .servers
                    .get(&self.key)
                    .and_then(|state| state.pending.get(csrf_token).cloned())
            })
            .map_err(AuthError::CredentialStoreError)
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        self.store
            .update(|document| {
                if let Some(state) = document.servers.get_mut(&self.key) {
                    state.pending.remove(csrf_token);
                }
            })
            .map_err(AuthError::CredentialStoreError)
    }
}

fn credential_key(name: &str, server_url: &str) -> Result<String, String> {
    let mut url = url::Url::parse(server_url).map_err(|_| "Invalid MCP server URL".to_string())?;
    url.set_fragment(None);
    Ok(format!("{}|{}", name.replace('-', "_"), url))
}

fn with_file_document<T>(
    path: &Path,
    operation: impl FnOnce(&mut AuthDocument) -> Result<(T, bool), String>,
) -> Result<T, String> {
    let parent = path.parent().ok_or("Invalid MCP OAuth credential path")?;
    fs::create_dir_all(parent).map_err(store_io)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(parent.join(".mcp-auth.json.lock"))
        .map_err(store_io)?;
    lock.lock_exclusive().map_err(store_io)?;
    let mut document = read_auth_document(path)?;
    let (result, changed) = operation(&mut document)?;
    if changed {
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| "MCP OAuth credential serialization failed".to_string())?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err("MCP OAuth credential store exceeds 1 MiB".into());
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(store_io)?;
        temp.write_all(&bytes).map_err(store_io)?;
        temp.write_all(b"\n").map_err(store_io)?;
        temp.as_file().sync_all().map_err(store_io)?;
        temp.persist(path).map_err(|error| store_io(error.error))?;
    }
    Ok(result)
}

fn read_auth_document(path: &Path) -> Result<AuthDocument, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthDocument::default());
        }
        Err(error) => return Err(store_io(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(store_io)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err("MCP OAuth credential store exceeds 1 MiB".into());
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(AuthDocument::default());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid MCP OAuth credential store".into())
}

fn store_io(error: std::io::Error) -> String {
    format!("MCP OAuth credential I/O failed: {}", error.kind())
}

#[derive(Debug, thiserror::Error)]
pub enum McpOAuthError {
    #[error("MCP OAuth operation cancelled")]
    Cancelled,
    #[error("MCP OAuth operation failed: {0}")]
    Failed(String),
    #[error("OAuth redirect does not match this sign-in")]
    RedirectMismatch,
}

/// Plain data a host can show in a browser-oriented UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOAuthRedirect {
    pub authorization_url: String,
    pub redirect_url: String,
    pub state: String,
}

/// A started OAuth authorization-code flow. The host may receive the response
/// through its own loopback listener or pass a full pasted redirect URL.
pub struct McpOAuthSignIn {
    state: rmcp::transport::auth::OAuthState,
    redirect: McpOAuthRedirect,
}

impl McpOAuthSignIn {
    /// Refreshes stored credentials without opening an authorization UI.
    pub async fn refresh(
        name: &str,
        server_url: &str,
        settings: &McpOAuthConfig,
        store: McpOAuthCredentialStore,
        signal: AbortSignal,
    ) -> Result<(), McpOAuthError> {
        validate_oauth_resource_url(server_url)?;
        let server = store
            .for_server(name, server_url)
            .map_err(McpOAuthError::Failed)?;
        let (mut manager, explicit_metadata) = tokio::select! {
            biased;
            () = signal.wait() => return Err(McpOAuthError::Cancelled),
            result = new_authorization_manager(
                server_url,
                settings.auth_server_metadata_url.as_deref(),
            ) => result.map_err(McpOAuthError::Failed)?,
        };
        manager.set_credential_store(server.clone());
        manager.set_state_store(server);
        manager.initialize_from_store().await.map_err(oauth_error)?;
        configure_preregistered_refresh(&mut manager, settings, explicit_metadata)
            .await
            .map_err(McpOAuthError::Failed)?;
        cancellable(&signal, manager.refresh_token())
            .await
            .map(|_| ())
    }

    pub async fn start(
        name: &str,
        server_url: &str,
        settings: &McpOAuthConfig,
        store: McpOAuthCredentialStore,
        redirect_url: &str,
        challenge: Option<&str>,
        signal: AbortSignal,
    ) -> Result<(Self, McpOAuthRedirect), McpOAuthError> {
        validate_oauth_resource_url(server_url)?;
        validate_redirect_url(redirect_url)?;
        let server = store
            .for_server(name, server_url)
            .map_err(McpOAuthError::Failed)?;
        let (mut manager, explicit_metadata) = tokio::select! {
            biased;
            () = signal.wait() => return Err(McpOAuthError::Cancelled),
            result = new_authorization_manager(
                server_url,
                settings.auth_server_metadata_url.as_deref(),
            ) => result.map_err(McpOAuthError::Failed)?,
        };
        manager.set_credential_store(server.clone());
        manager.set_state_store(server);
        manager.initialize_from_store().await.map_err(oauth_error)?;
        let metadata = match explicit_metadata {
            Some(metadata) => metadata,
            None => {
                cancellable(&signal, manager.resolve_metadata_from_challenge(challenge))
                    .await?
                    .metadata
            }
        };
        manager.set_metadata(metadata.clone());
        let (actual_redirect, client_metadata_url) =
            client_metadata_document(settings, redirect_url, server_url, &metadata)?;
        let mut request = AuthorizationRequest::new(&actual_redirect)
            .with_client_name(settings.client_name.as_deref().unwrap_or("pi"));
        if let Some(scope) = &settings.scope {
            request = request.with_scopes(scope.split_whitespace());
        }
        if let Some(client_id) = &settings.client_id {
            request = request.with_preregistered_client(client_id);
            if let Some(secret) = &settings.client_secret {
                request = request.with_client_secret(secret);
            }
        } else if let Some(client_metadata_url) = client_metadata_url {
            request = request.with_client_metadata_url(client_metadata_url);
        }
        let session = tokio::select! {
            biased;
            () = signal.wait() => return Err(McpOAuthError::Cancelled),
            result = AuthorizationSession::new(manager, request) => {
                result.map_err(|(_, error)| oauth_error(error))?
            }
        };
        let state = rmcp::transport::auth::OAuthState::Session(session);
        let authorization_url = state.get_authorization_url().await.map_err(oauth_error)?;
        let url = url::Url::parse(&authorization_url).map_err(|_| {
            McpOAuthError::Failed("authorization server returned an invalid URL".into())
        })?;
        let state_value = url
            .query_pairs()
            .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
            .ok_or_else(|| McpOAuthError::Failed("authorization URL omitted state".into()))?;
        let actual_redirect = url
            .query_pairs()
            .find_map(|(name, value)| (name == "redirect_uri").then(|| value.into_owned()))
            .unwrap_or_else(|| redirect_url.to_string());
        let redirect = McpOAuthRedirect {
            authorization_url,
            redirect_url: actual_redirect,
            state: state_value,
        };
        Ok((
            Self {
                state,
                redirect: redirect.clone(),
            },
            redirect,
        ))
    }

    pub async fn complete_redirect_url(
        &mut self,
        redirect_url: &str,
        signal: AbortSignal,
    ) -> Result<(), McpOAuthError> {
        validate_redirect_response(redirect_url, &self.redirect)?;
        cancellable(&signal, self.state.handle_callback_url(redirect_url)).await
    }
}

fn client_metadata_document(
    settings: &McpOAuthConfig,
    redirect_url: &str,
    server_url: &str,
    metadata: &AuthorizationMetadata,
) -> Result<(String, Option<String>), McpOAuthError> {
    if settings.client_id.is_some() {
        return Ok((redirect_url.to_string(), None));
    }
    let registration = settings.client_registration.unwrap_or_default();
    if registration == McpOAuthClientRegistration::Cimd && !metadata_documents_list(redirect_url) {
        return Err(McpOAuthError::Failed(
            "CIMD requires a localhost or 127.0.0.1 /callback redirect".into(),
        ));
    }
    let should_offer = match registration {
        McpOAuthClientRegistration::Dcr => false,
        McpOAuthClientRegistration::Cimd => true,
        McpOAuthClientRegistration::Auto => {
            settings.client_name.is_none() && metadata_documents_list(redirect_url)
        }
    };
    if !should_offer {
        return Ok((redirect_url.to_string(), None));
    }
    let supports_documents = metadata
        .additional_fields
        .get("client_id_metadata_document_supported")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && metadata
            .additional_fields
            .get("token_endpoint_auth_methods_supported")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|methods| methods.iter().any(|method| method == "none"));
    if !supports_documents {
        if registration == McpOAuthClientRegistration::Cimd {
            return Err(McpOAuthError::Failed(
                "authorization server does not support Client ID Metadata Documents for public clients".into(),
            ));
        }
        return Ok((redirect_url.to_string(), None));
    }
    let supports_issuer = metadata
        .additional_fields
        .get("authorization_response_iss_parameter_supported")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if supports_issuer {
        return Ok((
            redirect_url.to_string(),
            Some("https://pi.dev/oauth/client.json".into()),
        ));
    }
    use base64::Engine;
    use sha2::Digest;
    let canonical_server = url::Url::parse(server_url)
        .expect("validated OAuth server URL")
        .to_string();
    let digest = sha2::Sha256::digest(canonical_server.as_bytes());
    let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..9]);
    let mut redirect = url::Url::parse(redirect_url).expect("validated redirect URL");
    redirect.set_path(&format!("{}/{id}", redirect.path().trim_end_matches('/')));
    Ok((
        redirect.to_string(),
        Some(format!("https://pi.dev/oauth/{id}/client.json")),
    ))
}

fn metadata_documents_list(redirect_url: &str) -> bool {
    url::Url::parse(redirect_url).ok().is_some_and(|url| {
        matches!(url.host_str(), Some("localhost" | "127.0.0.1")) && url.path() == "/callback"
    })
}

fn validate_oauth_resource_url(server_url: &str) -> Result<(), McpOAuthError> {
    let url = url::Url::parse(server_url)
        .map_err(|_| McpOAuthError::Failed("invalid OAuth server URL".into()))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpOAuthError::Failed(
            "OAuth server URL must be HTTPS or loopback HTTP without userinfo or fragment".into(),
        ));
    }
    Ok(())
}

fn validate_redirect_url(redirect_url: &str) -> Result<(), McpOAuthError> {
    let url = url::Url::parse(redirect_url).map_err(|_| McpOAuthError::RedirectMismatch)?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(McpOAuthError::RedirectMismatch);
    }
    Ok(())
}

fn validate_redirect_response(
    response: &str,
    authorization: &McpOAuthRedirect,
) -> Result<(), McpOAuthError> {
    let response = url::Url::parse(response).map_err(|_| McpOAuthError::RedirectMismatch)?;
    let redirect = url::Url::parse(&authorization.redirect_url)
        .map_err(|_| McpOAuthError::RedirectMismatch)?;
    if response.origin() != redirect.origin() || response.path() != redirect.path() {
        return Err(McpOAuthError::RedirectMismatch);
    }
    let state = response
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()));
    if state.as_deref() != Some(&authorization.state) {
        return Err(McpOAuthError::RedirectMismatch);
    }
    Ok(())
}

async fn cancellable<T>(
    signal: &AbortSignal,
    future: impl Future<Output = Result<T, AuthError>>,
) -> Result<T, McpOAuthError> {
    tokio::select! {
        biased;
        () = signal.wait() => Err(McpOAuthError::Cancelled),
        result = future => result.map_err(oauth_error),
    }
}

fn oauth_error(error: AuthError) -> McpOAuthError {
    // rmcp's OAuth diagnostics contain endpoint/status context but its credential
    // structs redact tokens. Never include Debug output or response bodies here.
    McpOAuthError::Failed(error.to_string())
}

pub(crate) async fn oauth_http_client(
    name: &str,
    server_url: &str,
    settings: Option<&McpOAuthConfig>,
    auth: &McpClientAuth,
) -> Result<refresh::OAuthHttpClient, String> {
    let server = auth.oauth_store.for_server(name, server_url)?;
    let (mut manager, explicit_metadata) = new_authorization_manager(
        server_url,
        settings.and_then(|settings| settings.auth_server_metadata_url.as_deref()),
    )
    .await?;
    manager.set_credential_store(server.clone());
    manager.set_state_store(server.clone());
    manager
        .initialize_from_store()
        .await
        .map_err(|_| "OAuth credential state could not be loaded".to_string())?;
    if let (Some(values), Some(settings)) = (&auth.local_values, settings)
        && settings.client_secret.is_some()
    {
        return Ok(refresh::OAuthHttpClient::Local(
            refresh::LocalOAuthClient::new(
                manager,
                server,
                settings.clone(),
                values.clone(),
                explicit_metadata,
            ),
        ));
    }
    if let Some(settings) = settings {
        configure_preregistered_refresh(&mut manager, settings, explicit_metadata).await?;
    }
    Ok(refresh::OAuthHttpClient::Fixed(AuthClient::new(
        reqwest::Client::new(),
        manager,
    )))
}

async fn configure_preregistered_refresh(
    manager: &mut AuthorizationManager,
    settings: &McpOAuthConfig,
    explicit_metadata: Option<AuthorizationMetadata>,
) -> Result<(), String> {
    let (Some(client_id), Some(client_secret)) = (&settings.client_id, &settings.client_secret)
    else {
        return Ok(());
    };
    if explicit_metadata.is_none() {
        let metadata = manager
            .resolve_metadata()
            .await
            .map_err(|_| "OAuth authorization-server discovery failed".to_string())?
            .metadata;
        manager.set_metadata(metadata);
    }
    let redirect = settings.callback_url.clone().unwrap_or_else(|| {
        settings.callback_port.map_or_else(
            || "http://127.0.0.1/callback".to_string(),
            |port| format!("http://127.0.0.1:{port}/callback"),
        )
    });
    let mut config = OAuthClientConfig::new(client_id, redirect)
        .with_client_secret(client_secret)
        .with_scopes(
            settings
                .scope
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect(),
        );
    config.application_type = Some("native".into());
    manager
        .configure_client(config)
        .map_err(|_| "OAuth client configuration failed".to_string())
}

async fn new_authorization_manager(
    server_url: &str,
    metadata_url: Option<&str>,
) -> Result<(AuthorizationManager, Option<AuthorizationMetadata>), String> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let mut manager = AuthorizationManager::new(server_url)
        .await
        .map_err(|_| "invalid OAuth server URL".to_string())?;
    let Some(metadata_url) = metadata_url else {
        return Ok((manager, None));
    };
    let metadata_address = url::Url::parse(metadata_url)
        .map_err(|_| "Invalid OAuth authorization-server metadata URL".to_string())?;
    if !metadata_address.username().is_empty()
        || metadata_address.password().is_some()
        || metadata_address.fragment().is_some()
        || (metadata_address.scheme() != "https"
            && !(metadata_address.scheme() == "http"
                && matches!(
                    metadata_address.host_str(),
                    Some("localhost" | "127.0.0.1" | "::1")
                )))
    {
        return Err("Insecure OAuth authorization-server metadata URL".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "OAuth metadata client initialization failed".to_string())?;
    let response = client
        .get(metadata_url)
        .send()
        .await
        .map_err(|_| "OAuth authorization-server metadata request failed".to_string())?;
    if !response.status().is_success() {
        return Err("OAuth authorization-server metadata request failed".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "OAuth authorization-server metadata response failed".to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("OAuth authorization-server metadata exceeds 1 MiB".into());
    }
    let metadata: AuthorizationMetadata = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid OAuth authorization-server metadata".to_string())?;
    for endpoint in [
        Some(metadata.authorization_endpoint.as_str()),
        Some(metadata.token_endpoint.as_str()),
        metadata.registration_endpoint.as_deref(),
        metadata.issuer.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let endpoint = url::Url::parse(endpoint)
            .map_err(|_| "Invalid endpoint in OAuth authorization-server metadata".to_string())?;
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || (endpoint.scheme() != "https"
                && !(endpoint.scheme() == "http"
                    && matches!(endpoint.host_str(), Some("localhost" | "127.0.0.1" | "::1"))))
        {
            return Err("Insecure endpoint in OAuth authorization-server metadata".into());
        }
    }
    manager.set_metadata(metadata.clone());
    Ok((manager, Some(metadata)))
}

#[derive(Clone)]
pub(crate) struct ProviderAuthClient {
    inner: reqwest::Client,
    provider: String,
    resolver: Arc<dyn McpProviderTokenResolver>,
}

impl ProviderAuthClient {
    pub(crate) fn new(provider: String, resolver: Arc<dyn McpProviderTokenResolver>) -> Self {
        Self {
            inner: reqwest::Client::new(),
            provider,
            resolver,
        }
    }

    async fn token(&self) -> Result<Option<String>, StreamableHttpError<reqwest::Error>> {
        self.resolver.token(&self.provider).await.map_err(|_| {
            StreamableHttpError::UnexpectedServerResponse(
                "provider credential resolution failed".to_string().into(),
            )
        })
    }
}

impl StreamableHttpClient for ProviderAuthClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.inner
            .post_message(
                uri,
                message,
                session_id,
                self.token().await?,
                custom_headers,
            )
            .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.inner
            .post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                self.token().await?,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.inner
            .delete_session(uri, session_id, self.token().await?, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.inner
            .get_stream(
                uri,
                session_id,
                last_event_id,
                self.token().await?,
                custom_headers,
            )
            .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.inner
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                self.token().await?,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }
}
