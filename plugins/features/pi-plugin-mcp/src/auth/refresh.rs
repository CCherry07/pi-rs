//! One refresh coordinator for all OAuth clients, including Explicit and late local secrets.
//! Only token/rejection coordination lives here; rmcp still constructs and exchanges OAuth requests.
use super::*;
use crate::client::operation::RequestControl;
use oauth2::TokenResponse;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex as AsyncMutex, watch};

pub(crate) type OAuthHttpClient = LocalOAuthClient;

#[derive(Clone)]
pub(crate) struct LocalOAuthClient {
    http: PiHttpClient,
    state: Arc<RefreshState>,
    mode: ConnectionMode,
}

struct RefreshState {
    manager: AsyncMutex<AuthorizationManager>,
    store: ServerStore,
    settings: RefreshSettings,
    completed: AtomicU64,
    flight: AsyncMutex<Option<Flight>>,
    last_token: Mutex<Option<String>>,
}

enum RefreshSettings {
    /// The SDK manager already holds restored DCR/CIMD or fixed preregistration settings.
    Prepared,
    /// Only trusted local-secret settings need late resolution on each refresh.
    Local(Box<LocalRefreshSettings>),
}

struct LocalRefreshSettings {
    settings: McpOAuthConfig,
    values: crate::config::LocalValues,
    metadata: Option<AuthorizationMetadata>,
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
    fn http(self) -> HttpError {
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
        mode: ConnectionMode,
    ) -> Self {
        Self::with_settings(
            manager,
            store,
            RefreshSettings::Local(Box::new(LocalRefreshSettings {
                settings,
                values,
                metadata,
            })),
            mode,
        )
    }

    pub(super) fn prepared(
        manager: AuthorizationManager,
        store: ServerStore,
        mode: ConnectionMode,
    ) -> Self {
        Self::with_settings(manager, store, RefreshSettings::Prepared, mode)
    }

    fn with_settings(
        manager: AuthorizationManager,
        store: ServerStore,
        settings: RefreshSettings,
        mode: ConnectionMode,
    ) -> Self {
        Self {
            http: PiHttpClient::new(),
            state: Arc::new(RefreshState {
                manager: AsyncMutex::new(manager),
                store,
                settings,
                completed: AtomicU64::new(0),
                flight: AsyncMutex::new(None),
                last_token: Mutex::new(None),
            }),
            mode,
        }
    }

    fn control(&self, message: &ClientJsonRpcMessage) -> Option<RequestControl> {
        (self.mode == ConnectionMode::Supervised)
            .then(|| RequestControl::from_message(message))
            .flatten()
    }

    fn check_dispatch(control: Option<&RequestControl>) -> Result<(), HttpError> {
        control.map_or(Ok(()), |control| {
            control.check_dispatch().map_err(|error| error.into_http())
        })
    }

    fn check_completion(control: Option<&RequestControl>) -> Result<(), HttpError> {
        control.map_or(Ok(()), |control| {
            control
                .check_completion()
                .map_err(|error| error.into_http())
        })
    }

    fn finish_sdk_post(
        control: Option<&RequestControl>,
        project: impl FnOnce() -> Result<StreamableHttpPostResponse, HttpError>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let projection = provider_response(project());
        Self::check_completion(control)?;
        projection
    }

    async fn acquire_token<T>(
        &self,
        control: Option<&RequestControl>,
        acquire: impl Future<Output = Result<(Option<String>, T), RefreshFailure>>,
    ) -> Result<(Option<String>, T), HttpError> {
        Self::check_dispatch(control)?;
        // Terminal cleanup remembers acquisition, not successful dispatch. This never grants
        // permission to send: the strict fence still runs after acquisition and before POST.
        let acquired = acquire.await.and_then(|(token, value)| {
            self.state.remember_token(token.clone())?;
            Ok((token, value))
        });
        Self::check_dispatch(control)?;
        acquired.map_err(RefreshFailure::http)
    }

    async fn request<T, F, Fut>(
        &self,
        _uri: &str,
        control: Option<RequestControl>,
        auth_token: Option<String>,
        call: F,
    ) -> Result<T, HttpError>
    where
        F: Fn(Option<String>) -> Fut,
        Fut: Future<Output = Result<T, HttpError>>,
    {
        self.authorized_request(control, true, auth_token, call)
            .await
    }

    async fn authorized_request<T, F, Fut>(
        &self,
        control: Option<RequestControl>,
        shared_recovery_budget: bool,
        auth_token: Option<String>,
        call: F,
    ) -> Result<T, HttpError>
    where
        F: Fn(Option<String>) -> Fut,
        Fut: Future<Output = Result<T, HttpError>>,
    {
        Self::check_dispatch(control.as_ref())?;
        let acquire = async {
            match auth_token {
                Some(token) => Ok((Some(token), self.state.completed.load(Ordering::Acquire))),
                None => self.state.token().await,
            }
        };
        let (token, observed) = self.acquire_token(control.as_ref(), acquire).await?;
        let result = provider_response(call(token.clone()).await);
        Self::check_completion(control.as_ref())?;
        let error = match result {
            Err(StreamableHttpError::Client(error)) if error.auth_rejection().is_some() => error,
            result => return result,
        };
        if error
            .auth_rejection()
            .is_some_and(|rejection| rejection.is_insufficient_scope())
        {
            // Scope changes require explicit sign-in; refresh cannot enlarge the grant.
            return Err(StreamableHttpError::Client(error.require_sign_in()));
        }
        Self::check_dispatch(control.as_ref())?;
        let recoverable = self.state.can_recover(token.as_deref()).await;
        Self::check_dispatch(control.as_ref())?;
        if !recoverable.map_err(RefreshFailure::http)? {
            return Err(StreamableHttpError::Client(error.require_sign_in()));
        }
        // Reserve BEFORE starting or joining reactive refresh. No refund on failure/drop.
        // Each transport-owned common/response GET opening has its own at-most-one auth
        // reaction/replay, independent of the originating POST/resource allowance. This never
        // reexecutes the RPC. Outer reconnect eligibility, completion and cancellation/epoch
        // guarantees remain separate SSE-RESUME-CONTROL questions, not this local auth bound.
        if shared_recovery_budget
            && let Some(control) = &control
            && !control.try_recover().map_err(|error| error.into_http())?
        {
            return Err(StreamableHttpError::Client(error));
        }
        let refreshed = self.state.refresh(token, observed).await;
        Self::check_dispatch(control.as_ref())?;
        match refreshed {
            Ok(_) => {
                // Reload current storage, not a pre-exchange token: newer login/logout wins.
                // A successful callback permits a replay even if the token is unchanged.
                let acquire = async { self.state.current_token().await.map(|token| (token, ())) };
                let (token, ()) = self.acquire_token(control.as_ref(), acquire).await?;
                Self::check_dispatch(control.as_ref())?;
                let result = provider_response(call(token).await);
                Self::check_completion(control.as_ref())?;
                result
            }
            Err(RefreshFailure::AuthorizationRequired) => {
                Err(StreamableHttpError::Client(error.require_sign_in()))
            }
            Err(error) => Err(error.http()),
        }
    }
}

impl RefreshState {
    async fn credentials(&self) -> Result<Option<StoredCredentials>, RefreshFailure> {
        CredentialStore::load(&self.store).await.map_err(|_| {
            RefreshFailure::Failed("MCP OAuth credential state could not be loaded".into())
        })
    }

    fn remember_token(&self, token: Option<String>) -> Result<(), RefreshFailure> {
        *self
            .last_token
            .lock()
            .map_err(|_| RefreshFailure::Failed("MCP token state is unavailable".into()))? = token;
        Ok(())
    }

    async fn current_token(&self) -> Result<Option<String>, RefreshFailure> {
        Ok(self
            .credentials()
            .await?
            .and_then(|stored| stored.token_response)
            .map(|token| token.access_token().secret().to_owned()))
    }

    async fn can_recover(&self, stale: Option<&str>) -> Result<bool, RefreshFailure> {
        let current = self.credentials().await?;
        Ok(current
            .as_ref()
            .and_then(|stored| stored.token_response.as_ref())
            .is_some_and(|token| {
                Some(token.access_token().secret().as_str()) != stale
                    || token.refresh_token().is_some()
            }))
    }

    async fn token(self: &Arc<Self>) -> Result<(Option<String>, u64), RefreshFailure> {
        // Pi waits for an already-running exchange before reading its current stored token.
        let active = self
            .flight
            .lock()
            .await
            .as_ref()
            .filter(|flight| flight.result.borrow().is_none())
            .map(|flight| flight.result.clone());
        if let Some(active) = active {
            let _ = wait_refresh(active).await;
        }
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
                expires
                    .as_secs()
                    .saturating_sub(now.saturating_sub(received))
                    <= 30
            }
            _ => false,
        };
        let current = token.access_token().secret().to_owned();
        if expired && token.refresh_token().is_some() {
            // Proactive failure is not terminal: use CURRENT storage, including logout/new login.
            let _ = self.refresh(Some(current), observed).await;
            return Ok((
                self.current_token().await?,
                self.completed.load(Ordering::Acquire),
            ));
        }
        Ok((Some(current), observed))
    }

    async fn refresh(self: &Arc<Self>, stale: Option<String>, observed: u64) -> RefreshResult {
        let mut flight = self.flight.lock().await;
        let result = match flight.as_ref() {
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
        wait_refresh(result).await
    }

    async fn exchange(&self, stale: Option<String>) -> RefreshResult {
        let current = self.credentials().await?;
        let Some(token) = current
            .as_ref()
            .and_then(|stored| stored.token_response.as_ref())
        else {
            return Err(RefreshFailure::AuthorizationRequired);
        };
        if Some(token.access_token().secret()) != stale.as_ref() {
            return Ok(token.access_token().secret().to_owned());
        }
        if token.refresh_token().is_none() {
            return Err(RefreshFailure::AuthorizationRequired);
        }
        let local_settings = match &self.settings {
            RefreshSettings::Prepared => None,
            RefreshSettings::Local(local) => Some((
                local
                    .values
                    .oauth(local.settings.clone(), pi_core::AbortHandle::new().1)
                    .await
                    .map_err(RefreshFailure::Failed)?,
                local.metadata.clone(),
            )),
        };
        let mut manager = self.manager.lock().await;
        // A detached refresh must not overwrite a newer login or resurrect a logout.
        manager.set_credential_store(RefreshStore {
            inner: self.store.clone(),
            expected: credentials_revision(current.as_ref()).map_err(|_| {
                RefreshFailure::Failed("MCP OAuth credential snapshot failed".into())
            })?,
        });
        if let Some((settings, metadata)) = local_settings {
            configure_preregistered_refresh(&mut manager, &settings, metadata)
                .await
                .map_err(RefreshFailure::Failed)?;
        }
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

async fn wait_refresh(mut result: watch::Receiver<Option<RefreshResult>>) -> RefreshResult {
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

impl crate::http_client::HttpClient for LocalOAuthClient {
    async fn post(
        &self,
        request: crate::http_client::PostRequest,
    ) -> Result<crate::http_client::PostResponse, HttpError> {
        self.request(
            &request.uri,
            self.control(&request.message),
            request.token.clone(),
            |token| request.execute(&self.http, token),
        )
        .await
    }

    async fn open_stream(
        &self,
        request: crate::http_client::StreamRequest,
    ) -> Result<crate::http_client::StreamResponse, HttpError> {
        // Stream fences survive every token/refresh await, but auth reaction remains
        // local to this GET: an already-spent application allowance is irrelevant.
        self.authorized_request(
            request.control.clone(),
            false,
            request.token.clone(),
            |token| request.execute(&self.http, token),
        )
        .await
    }
}

impl StreamableHttpClient for LocalOAuthClient {
    type Error = PiHttpClientError;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_token,
            custom_headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
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
        let control = self.control(&message);
        let response = <Self as crate::http_client::HttpClient>::post(
            self,
            crate::http_client::PostRequest::from_sdk(
                uri,
                message,
                session_id,
                auth_token,
                custom_headers,
                max_sse_event_size,
            ),
        )
        .await?;
        Self::finish_sdk_post(control.as_ref(), || response.into_sdk())
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        // Cleanup never acquires credentials, joins refresh, interprets a response or publishes
        // errors. Its per-transport acquired-token cache is never read by GET/POST/replay.
        let token = match self.state.last_token.lock() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(()),
        };
        let _ = self
            .http
            .delete_session(uri, session_id, token, custom_headers)
            .await;
        Ok(())
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_token,
            custom_headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
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
        <Self as crate::http_client::HttpClient>::open_stream(
            self,
            crate::http_client::StreamRequest::from_sdk(
                uri,
                session_id,
                last_event_id,
                auth_token,
                custom_headers,
                max_sse_event_size,
            ),
        )
        .await
        .map(|response| response.body)
    }
}

#[cfg(test)]
mod sse_common_tests;
#[cfg(test)]
pub(crate) mod sse_fixture;
#[cfg(test)]
mod sse_tests;
#[cfg(test)]
mod stream_control_tests;
#[cfg(test)]
mod supervised_tests;
#[cfg(all(test, unix))]
mod tests;
