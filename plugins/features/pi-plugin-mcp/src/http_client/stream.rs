//! Explicit in-process GET authority; never serialized, task-local, or keyed by request IDs.
use super::*;

pub(crate) type HttpStream = BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;

/// Real successful GET metadata travels with its body, not through a side channel.
pub(crate) struct StreamResponse {
    pub(crate) body: HttpStream,
    pub(crate) session: Option<String>,
    pub(crate) dispatched_session: Option<Arc<str>>,
    pub(crate) version: Option<ProtocolVersion>,
}

pub(crate) struct StreamRequest {
    pub(crate) uri: Arc<str>,
    pub(crate) session: Option<Arc<str>>,
    pub(crate) cursor: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) headers: HashMap<HeaderName, HeaderValue>,
    pub(crate) max_event_bytes: usize,
    pub(crate) control: Option<RequestControl>,
    /// Re-evaluated after each credential wait/retry, immediately before dispatch.
    pub(crate) dispatch: Option<DispatchProvider>,
    pub(crate) session_observer: Option<SessionObserver>,
}

impl StreamRequest {
    pub(crate) fn from_sdk(
        uri: Arc<str>,
        session: Option<Arc<str>>,
        cursor: Option<String>,
        token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max_event_bytes: usize,
    ) -> Self {
        Self {
            uri,
            session,
            cursor,
            token,
            headers,
            max_event_bytes,
            control: None,
            dispatch: None,
            session_observer: None,
        }
    }

    pub(crate) fn check_dispatch(&self) -> Result<(), HttpError> {
        self.control.as_ref().map_or(Ok(()), |control| {
            control.check_dispatch().map_err(|error| error.into_http())
        })
    }

    pub(crate) fn check_completion(&self) -> Result<(), HttpError> {
        self.control.as_ref().map_or(Ok(()), |control| {
            control
                .check_completion()
                .map_err(|error| error.into_http())
        })
    }

    /// Called only after credential acquisition. The underlying HTTP client does not
    /// acquire credentials, so there is no asynchronous credential wait past this fence.
    pub(crate) async fn execute(
        &self,
        client: &PiHttpClient,
        token: Option<String>,
    ) -> Result<StreamResponse, HttpError> {
        self.check_dispatch()?;
        let dispatch = self.dispatch.as_ref().map_or_else(
            || HttpDispatch {
                session: self.session.clone(),
                version: None,
                headers: self.headers.clone(),
            },
            |dispatch| dispatch(),
        );
        let result = self.send(client, token, dispatch).await;
        self.check_completion()?;
        result
    }

    async fn send(
        &self,
        client: &PiHttpClient,
        token: Option<String>,
        dispatch: HttpDispatch,
    ) -> Result<StreamResponse, HttpError> {
        let (request, _) = client.request(
            Method::GET,
            &self.uri,
            dispatch.session.as_deref(),
            self.cursor.as_deref(),
            token.as_deref(),
            dispatch.headers.clone(),
        )?;
        let response = request.send().await.map_err(network_error)?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        check_status(
            &response,
            dispatch
                .session
                .as_deref()
                .is_some_and(|session| !session.is_empty()),
            RequestOrigin::Other,
        )?;
        let session = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if let (Some(observer), Some(session)) = (&self.session_observer, session.as_deref()) {
            observer(session, dispatch.version.as_ref());
        }
        if content_type(&response).as_deref() != Some("text/event-stream") {
            return Err(client_error(PiHttpClientError::protocol(
                "Unsupported MCP GET response content type",
            )));
        }
        Ok(StreamResponse {
            body: sse::bounded(response, self.max_event_bytes),
            session,
            dispatched_session: dispatch.session,
            version: dispatch.version,
        })
    }
}

/// The private seam retains response metadata, multi-message POST responses and explicit
/// GET authority fences unavailable in the stock HTTP-client trait.
/// Implementations must check after token/refresh waits; GET gets its own local auth
/// allowance, never the originating application's recovery allowance.
pub(crate) trait HttpClient: StreamableHttpClient<Error = PiHttpClientError> + Sync {
    fn post(
        &self,
        request: PostRequest,
    ) -> impl Future<Output = Result<PostResponse, HttpError>> + Send;
    fn open_stream(
        &self,
        request: StreamRequest,
    ) -> impl Future<Output = Result<StreamResponse, HttpError>> + Send;
}

impl HttpClient for PiHttpClient {
    async fn post(&self, request: PostRequest) -> Result<PostResponse, HttpError> {
        request.execute(self, request.token.clone()).await
    }

    async fn open_stream(&self, request: StreamRequest) -> Result<StreamResponse, HttpError> {
        request.execute(self, request.token.clone()).await
    }
}
