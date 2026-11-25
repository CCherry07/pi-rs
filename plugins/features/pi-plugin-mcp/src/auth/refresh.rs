//! rmcp freezes client settings; local configuration must resolve secrets only when refreshing.
//! Only token/401 coordination lives here. rmcp still constructs and exchanges every OAuth request.
use super::*;
use oauth2::TokenResponse;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex as AsyncMutex, watch};

#[derive(Clone)]
pub(crate) enum OAuthHttpClient {
    Fixed(AuthClient<reqwest::Client>),
    Local(LocalOAuthClient),
}

#[derive(Clone)]
pub(crate) struct LocalOAuthClient {
    http: reqwest::Client,
    state: Arc<RefreshState>,
}

struct RefreshState {
    manager: AsyncMutex<AuthorizationManager>,
    store: ServerStore,
    settings: McpOAuthConfig,
    values: crate::config::LocalValues,
    metadata: Option<AuthorizationMetadata>,
    completed: AtomicU64,
    flight: AsyncMutex<Option<Flight>>,
}

struct Flight {
    generation: u64,
    result: watch::Receiver<Option<RefreshResult>>,
}

type RefreshResult = Result<String, RefreshFailure>;

#[derive(Clone)]
enum RefreshFailure {
    AuthorizationRequired,
    Failed(String),
}

impl RefreshFailure {
    fn http(self) -> StreamableHttpError<reqwest::Error> {
        match self {
            Self::AuthorizationRequired => AuthError::AuthorizationRequired.into(),
            Self::Failed(message) => AuthError::TokenRefreshFailed(message).into(),
        }
    }
}

impl LocalOAuthClient {
    pub(super) fn new(
        manager: AuthorizationManager,
        store: ServerStore,
        settings: McpOAuthConfig,
        values: crate::config::LocalValues,
        metadata: Option<AuthorizationMetadata>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            state: Arc::new(RefreshState {
                manager: AsyncMutex::new(manager),
                store,
                settings,
                values,
                metadata,
                completed: AtomicU64::new(0),
                flight: AsyncMutex::new(None),
            }),
        }
    }

    async fn request<T, F, Fut>(
        &self,
        auth_token: Option<String>,
        call: F,
    ) -> Result<T, StreamableHttpError<reqwest::Error>>
    where
        F: Fn(Option<String>) -> Fut,
        Fut: Future<Output = Result<T, StreamableHttpError<reqwest::Error>>>,
    {
        let (token, observed) = match auth_token {
            Some(token) => (Some(token), self.state.completed.load(Ordering::Acquire)),
            None => self.state.token().await.map_err(RefreshFailure::http)?,
        };
        match call(token.clone()).await {
            Err(StreamableHttpError::AuthRequired(challenge)) => {
                let Some(stale) = token else {
                    return Err(StreamableHttpError::AuthRequired(challenge));
                };
                match self.state.refresh(stale.clone(), observed).await {
                    // Exactly one challenge-triggered retry; never recursively retry this call.
                    Ok(token) if token != stale => call(Some(token)).await,
                    Ok(_) | Err(RefreshFailure::AuthorizationRequired) => {
                        Err(StreamableHttpError::AuthRequired(challenge))
                    }
                    Err(error) => Err(error.http()),
                }
            }
            result => result,
        }
    }
}

impl RefreshState {
    async fn credentials(&self) -> Result<Option<StoredCredentials>, RefreshFailure> {
        CredentialStore::load(&self.store).await.map_err(|_| {
            RefreshFailure::Failed("MCP OAuth credential state could not be loaded".into())
        })
    }

    async fn token(self: &Arc<Self>) -> Result<(Option<String>, u64), RefreshFailure> {
        let observed = self.completed.load(Ordering::Acquire);
        let stored = self.credentials().await?;
        let Some((credentials, token)) = stored
            .as_ref()
            .and_then(|stored| stored.token_response.as_ref().map(|token| (stored, token)))
        else {
            return Ok((None, observed));
        };
        let expired = match (token.expires_in(), credentials.token_received_at) {
            (Some(expires), Some(received)) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // Keep rmcp's existing 30-second refresh skew and unknown-expiry behavior.
                expires
                    .as_secs()
                    .saturating_sub(now.saturating_sub(received))
                    < 30
            }
            _ => false,
        };
        let token = token.access_token().secret().to_owned();
        if !expired {
            return Ok((Some(token), observed));
        }
        match self.refresh(token, observed).await {
            Ok(token) => Ok((Some(token), self.completed.load(Ordering::Acquire))),
            Err(RefreshFailure::AuthorizationRequired) => {
                Ok((None, self.completed.load(Ordering::Acquire)))
            }
            Err(error) => Err(error),
        }
    }

    async fn refresh(self: &Arc<Self>, stale: String, observed: u64) -> RefreshResult {
        let mut flight = self.flight.lock().await;
        let mut result = match flight.as_ref() {
            // An overlapping request shares success OR failure, even if its 401 arrived late.
            Some(active) if active.generation > observed || active.result.borrow().is_none() => {
                active.result.clone()
            }
            _ => {
                let generation = self.completed.load(Ordering::Acquire) + 1;
                let (send, receive) = watch::channel(None);
                *flight = Some(Flight {
                    generation,
                    result: receive.clone(),
                });
                let owner = self.clone();
                // Refresh owns its lifetime: cancelling a waiter must not kill another caller's
                // helper, or lose a rotated token after the server has answered. Secret capture
                // has its own bounded signal; it never borrows any caller's abort state.
                tokio::spawn(async move {
                    let result = owner.exchange(stale).await;
                    owner.completed.store(generation, Ordering::Release);
                    send.send_replace(Some(result));
                });
                receive
            }
        };
        drop(flight);
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            result
                .changed()
                .await
                .map_err(|_| RefreshFailure::Failed("MCP OAuth refresh task stopped".into()))?;
        }
    }

    async fn exchange(&self, stale: String) -> RefreshResult {
        let current = self.credentials().await?;
        let Some(token) = current
            .as_ref()
            .and_then(|stored| stored.token_response.as_ref())
        else {
            return Err(RefreshFailure::AuthorizationRequired);
        };
        if token.access_token().secret() != &stale {
            return Ok(token.access_token().secret().to_owned());
        }
        if token.refresh_token().is_none() {
            return Err(RefreshFailure::AuthorizationRequired);
        }
        let settings = self
            .values
            .oauth(self.settings.clone(), pi_core::AbortHandle::new().1)
            .await
            .map_err(RefreshFailure::Failed)?;
        let mut manager = self.manager.lock().await;
        // A detached refresh must not overwrite a newer login or resurrect a logout.
        manager.set_credential_store(RefreshStore {
            inner: self.store.clone(),
            expected: credentials_revision(current.as_ref()).map_err(|_| {
                RefreshFailure::Failed("MCP OAuth credential snapshot failed".into())
            })?,
        });
        configure_preregistered_refresh(&mut manager, &settings, self.metadata.clone())
            .await
            .map_err(RefreshFailure::Failed)?;
        match manager.refresh_token().await {
            Ok(tokens) => Ok(tokens.access_token().secret().to_owned()),
            Err(AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_)) => {
                Err(RefreshFailure::AuthorizationRequired)
            }
            // Endpoint errors may echo credentials. Never propagate SDK body/error text.
            Err(_) => Err(RefreshFailure::Failed(
                "MCP OAuth token refresh failed".into(),
            )),
        }
    }
}

/// Atomic credential replacement stays in the existing store; a refresh cannot publish over
/// another actor's mutation while its helper or token request was in flight.
#[derive(Clone)]
struct RefreshStore {
    inner: ServerStore,
    expected: [u8; 32],
}

fn credentials_revision(value: Option<&StoredCredentials>) -> Result<[u8; 32], AuthError> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(&value).map_err(|_| {
        AuthError::CredentialStoreError("MCP OAuth credential snapshot failed".into())
    })?;
    Ok(Sha256::digest(bytes).into())
}

#[async_trait]
impl CredentialStore for RefreshStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let current = CredentialStore::load(&self.inner).await?;
        if credentials_revision(current.as_ref())? != self.expected {
            return Err(AuthError::AuthorizationRequired);
        }
        Ok(current)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let replaced = self
            .inner
            .store
            .update_result(|document| {
                let current = document
                    .servers
                    .get(&self.inner.key)
                    .and_then(|state| state.credentials.as_ref());
                if credentials_revision(current).ok() != Some(self.expected) {
                    return false;
                }
                document
                    .servers
                    .entry(self.inner.key.clone())
                    .or_default()
                    .credentials = Some(credentials);
                true
            })
            .map_err(AuthError::CredentialStoreError)?;
        if replaced {
            Ok(())
        } else {
            Err(AuthError::CredentialStoreError(
                "MCP OAuth credentials changed during refresh".into(),
            ))
        }
    }

    async fn clear(&self) -> Result<(), AuthError> {
        Err(AuthError::CredentialStoreError(
            "MCP OAuth refresh cannot clear credentials".into(),
        ))
    }
}

impl StreamableHttpClient for OAuthHttpClient {
    type Error = reqwest::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        match self {
            Self::Fixed(client) => {
                client
                    .post_message(uri, message, session_id, auth_token, custom_headers)
                    .await
            }
            Self::Local(client) => {
                client
                    .post_message(uri, message, session_id, auth_token, custom_headers)
                    .await
            }
        }
    }
    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        match self {
            Self::Fixed(client) => {
                client
                    .post_message_with_max_sse_event_size(
                        uri,
                        message,
                        session_id,
                        auth_token,
                        custom_headers,
                        max_sse_event_size,
                    )
                    .await
            }
            Self::Local(client) => {
                client
                    .post_message_with_max_sse_event_size(
                        uri,
                        message,
                        session_id,
                        auth_token,
                        custom_headers,
                        max_sse_event_size,
                    )
                    .await
            }
        }
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        match self {
            Self::Fixed(client) => {
                client
                    .delete_session(uri, session_id, auth_token, custom_headers)
                    .await
            }
            Self::Local(client) => {
                client
                    .delete_session(uri, session_id, auth_token, custom_headers)
                    .await
            }
        }
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        match self {
            Self::Fixed(client) => {
                client
                    .get_stream(uri, session_id, last_event_id, auth_token, custom_headers)
                    .await
            }
            Self::Local(client) => {
                client
                    .get_stream(uri, session_id, last_event_id, auth_token, custom_headers)
                    .await
            }
        }
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        match self {
            Self::Fixed(client) => {
                client
                    .get_stream_with_max_sse_event_size(
                        uri,
                        session_id,
                        last_event_id,
                        auth_token,
                        custom_headers,
                        max_sse_event_size,
                    )
                    .await
            }
            Self::Local(client) => {
                client
                    .get_stream_with_max_sse_event_size(
                        uri,
                        session_id,
                        last_event_id,
                        auth_token,
                        custom_headers,
                        max_sse_event_size,
                    )
                    .await
            }
        }
    }
}

impl StreamableHttpClient for LocalOAuthClient {
    type Error = reqwest::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.request(auth_token, |token| {
            self.http.post_message(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                token,
                custom_headers.clone(),
            )
        })
        .await
    }
    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.request(auth_token, |token| {
            self.http.post_message_with_max_sse_event_size(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                token,
                custom_headers.clone(),
                max_sse_event_size,
            )
        })
        .await
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.request(auth_token, |token| {
            self.http.delete_session(
                uri.clone(),
                session_id.clone(),
                token,
                custom_headers.clone(),
            )
        })
        .await
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.request(auth_token, |token| {
            self.http.get_stream(
                uri.clone(),
                session_id.clone(),
                last_event_id.clone(),
                token,
                custom_headers.clone(),
            )
        })
        .await
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.request(auth_token, |token| {
            self.http.get_stream_with_max_sse_event_size(
                uri.clone(),
                session_id.clone(),
                last_event_id.clone(),
                token,
                custom_headers.clone(),
                max_sse_event_size,
            )
        })
        .await
    }
}

#[cfg(all(test, unix))]
mod tests;
