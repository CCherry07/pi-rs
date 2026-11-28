//! The product's sole HTTP transport worker over rmcp's public transport/service contracts.
//!
//! rmcp still correlates RPC replies and owns the service. This worker owns HTTP session
//! state and stream lifetimes; it never wraps the SDK HTTP worker or fabricates RPC errors.
//! A response-stream failure completes the original send with a typed transport error.
mod bootstrap;
mod config;
mod headers;
mod json;
pub(crate) use config::HttpTransportConfig;
mod sse;
#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use http::{HeaderName, HeaderValue};
use rmcp::{
    RoleClient,
    model::{
        ClientJsonRpcMessage, ClientNotification, ClientRequest, GetExtensions, GetMeta,
        ProtocolVersion, RequestId, ServerJsonRpcMessage, ServerResult,
    },
    service::InboundStreamOrigin,
    transport::{
        WorkerTransport,
        worker::{Worker, WorkerConfig, WorkerContext, WorkerQuitReason},
    },
};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::JoinSet,
};

use crate::{
    client::operation::{OperationError, RequestControl},
    http_client::{
        HttpClient, HttpError, PiHttpClientError, PostRequest, PostResponse, StreamRequest,
    },
};

pub(crate) struct PiHttpTransport;

impl PiHttpTransport {
    pub(crate) fn with_client<C>(
        client: C,
        config: HttpTransportConfig,
    ) -> WorkerTransport<HttpWorker<C>>
    where
        C: HttpClient,
    {
        WorkerTransport::spawn(HttpWorker { client, config })
    }
}

pub(crate) struct HttpWorker<C> {
    client: C,
    config: HttpTransportConfig,
}

#[derive(Clone, Default)]
struct Session {
    id: Option<Arc<str>>,
    version: Option<ProtocolVersion>,
    tool_headers: Arc<headers::ToolHeaders>,
}

#[derive(Clone)]
struct CleanupSession {
    id: Arc<str>,
    version: Option<ProtocolVersion>,
}

struct Shared<C> {
    client: C,
    config: HttpTransportConfig,
    session: Arc<Mutex<Session>>,
    // Last acquired identity for terminal DELETE only; never dispatch/publication authority.
    cleanup: Arc<Mutex<Option<CleanupSession>>>,
    common: watch::Sender<Option<Session>>,
    incoming: mpsc::Sender<ServerJsonRpcMessage>,
}

struct PostPermit {
    permit: Option<OwnedSemaphorePermit>,
    opened: Arc<Notify>,
}

impl Drop for PostPermit {
    fn drop(&mut self) {
        self.permit.take();
        self.opened.notify_one();
    }
}

impl<C: HttpClient> Worker for HttpWorker<C> {
    // Retain original typed HTTP/auth/control evidence through the public worker contract.
    type Error = Arc<HttpError>;
    type Role = RoleClient;

    fn err_closed() -> Self::Error {
        Arc::new(HttpError::TransportChannelClosed)
    }
    fn err_join(error: tokio::task::JoinError) -> Self::Error {
        Arc::new(HttpError::TokioJoinError(error))
    }
    fn config(&self) -> WorkerConfig {
        let mut config = WorkerConfig::default();
        config.name = Some("pi-mcp-http".into());
        config.channel_buffer_capacity = self.config.channel_buffer_capacity;
        config
    }
    fn supports_request_cancellation() -> bool {
        true
    }
    fn is_control_message(message: &ClientJsonRpcMessage) -> bool {
        match message {
            ClientJsonRpcMessage::Response(_) | ClientJsonRpcMessage::Error(_) => true,
            ClientJsonRpcMessage::Notification(notification) => matches!(
                notification.notification,
                ClientNotification::CancelledNotification(_)
            ),
            _ => false,
        }
    }

    async fn run(
        self,
        mut context: WorkerContext<Self>,
    ) -> Result<(), WorkerQuitReason<Self::Error>> {
        let slots = Arc::new(Semaphore::new(self.config.max_concurrent_requests.max(1)));
        let opened = Arc::new(Notify::new());
        let (common, mut changes) = watch::channel(None);
        let shared = Arc::new(Shared {
            client: self.client,
            config: self.config,
            session: Arc::new(Mutex::new(Session::default())),
            cleanup: Arc::new(Mutex::new(None)),
            common,
            incoming: context.to_handler_tx.clone(),
        });
        let mut sends = JoinSet::new();
        let mut streams = JoinSet::new();
        let mut control_busy = false;
        loop {
            let (request, permit, control) = tokio::select! {
                biased;
                _ = context.cancellation_token.cancelled() => break,
                _ = changes.changed() => {
                    streams.abort_all();
                    // Join retired common streams before opening their replacement.
                    while streams.join_next().await.is_some() {}
                    let session = changes.borrow_and_update().clone();
                    if let Some(session) = session {
                        let shared = shared.clone();
                        streams.spawn(async move { shared.common_stream(session).await });
                    }
                    continue;
                }
                Some(result) = streams.join_next(), if !streams.is_empty() => {
                    // The common stream has no originating RPC. A terminal failure stops
                    // this stream, not unrelated admitted POSTs. Do not log server text.
                    if result.is_err() { break; }
                    continue;
                }
                Some(result) = sends.join_next(), if !sends.is_empty() => {
                    match result { Ok(true) => control_busy = false, Ok(false) => {}, Err(_) => break }
                    continue;
                }
                _ = opened.notified() => continue,
                request = context.control_from_handler_rx.recv(), if !control_busy => {
                    let Some(request) = request else { break };
                    control_busy = true;
                    (request, None, true)
                }
                request = context.from_handler_rx.recv(), if slots.available_permits() > 0 => {
                    let Some(request) = request else { break };
                    let permit = slots.clone().try_acquire_owned().expect("worker owns POST admission");
                    (request, Some(PostPermit { permit: Some(permit), opened: opened.clone() }), false)
                }
            };
            let shared = shared.clone();
            let closed = context.cancellation_token.clone();
            sends.spawn(async move {
                let cancellation = request.cancellation_token();
                let mut responder = Some(request.responder);
                let cancelled = async {
                    match cancellation.as_ref() {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending().await,
                    }
                };
                let result = tokio::select! {
                    biased;
                    _ = closed.cancelled() => Err(Arc::new(HttpError::TransportChannelClosed)),
                    _ = cancelled => Err(Arc::new(OperationError::Cancelled.into_http())),
                    result = async {
                        let send = shared.send(&request.message, &mut responder, permit);
                        if control {
                            tokio::time::timeout(shared.config.control_request_timeout, send).await
                                .map_err(|_| Arc::new(HttpError::ControlRequestTimeout))?
                        } else { send.await }
                    } => result,
                };
                let tail = match result {
                    Ok(tail) => {
                        if let Some(responder) = responder { let _ = responder.send(Ok(())); }
                        tail
                    }
                    Err(error) => {
                        if let Some(responder) = responder { let _ = responder.send(Err(error)); }
                        else {
                            // Setup already acknowledged its SSE headers so rmcp could
                            // read notifications/requests. An incomplete setup is fatal,
                            // not an invented RPC error or an indefinitely pending peer.
                            closed.cancel();
                        }
                        None
                    }
                };
                // A final response forbids further GETs, but does not discard trailing
                // notifications on the already-open body. No timeout-triggered reopen.
                if let Some(tail) = tail {
                    tokio::select! {
                        _ = closed.cancelled() => {},
                        _ = async { if let Some(token) = cancellation { token.cancelled().await }
                            else { std::future::pending::<()>().await } } => {},
                        _ = async {
                            match tail {
                                ResponseTail::Sse(body, session, id) => { let _ = shared.drain_tail(body, &session, &id).await; }
                                ResponseTail::Json(messages, session, id) => { let _ = shared.drain_json_tail(messages, &session, &id).await; }
                            }
                        } => {},
                    }
                }
                // The remaining fields of request retain rmcp's cancellation registration
                // through the body lifetime (no private SDK registration API is needed).
                drop(request.message);
                control
            });
        }
        sends.abort_all();
        streams.abort_all();
        while sends.join_next().await.is_some() {}
        while streams.join_next().await.is_some() {}
        if let Some(session) = shared.cleanup_snapshot()
            && let Some(id) = session.id.clone()
        {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                shared.client.delete_session(
                    shared.config.uri.clone(),
                    id,
                    None,
                    shared.headers(&session, None),
                ),
            )
            .await;
        }
        Ok(())
    }
}

fn answered(message: &ServerJsonRpcMessage, id: &RequestId) -> bool {
    match message {
        ServerJsonRpcMessage::Response(response) => &response.id == id,
        ServerJsonRpcMessage::Error(error) => error.id.as_ref() == Some(id),
        _ => false,
    }
}

enum ResponseTail {
    Sse(sse::Body, Session, RequestId),
    Json(crate::http_client::JsonMessageIter, Session, RequestId),
}
type Tail = Option<ResponseTail>;
type SendAck = Option<tokio::sync::oneshot::Sender<Result<(), Arc<HttpError>>>>;

impl<C: HttpClient> Shared<C> {
    fn snapshot(&self) -> Session {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn cleanup_snapshot(&self) -> Option<Session> {
        let acquired = self
            .cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let active = self.snapshot();
        if active.id.as_ref() == Some(&acquired.id) {
            return Some(active);
        }
        Some(Session {
            id: Some(acquired.id),
            version: acquired.version,
            ..Session::default()
        })
    }

    fn headers(
        &self,
        session: &Session,
        message: Option<&ClientJsonRpcMessage>,
    ) -> HashMap<HeaderName, HeaderValue> {
        Self::headers_from(self.config.custom_headers.clone(), session, message)
    }

    fn headers_from(
        mut headers: HashMap<HeaderName, HeaderValue>,
        session: &Session,
        message: Option<&ClientJsonRpcMessage>,
    ) -> HashMap<HeaderName, HeaderValue> {
        let version = Self::effective_version(session, message);
        if let Some(version) = version {
            if let Ok(value) = HeaderValue::from_str(version.as_str()) {
                headers.insert(HeaderName::from_static("mcp-protocol-version"), value);
            }
            if version >= ProtocolVersion::STANDARD_HEADERS
                && let Some(message) = message
                && let Ok(value) = serde_json::to_value(message)
            {
                for (name, value) in headers::standard_headers(&value, &session.tool_headers) {
                    headers.insert(name, value);
                }
            }
        }
        headers
    }

    fn effective_version(
        session: &Session,
        message: Option<&ClientJsonRpcMessage>,
    ) -> Option<ProtocolVersion> {
        message
            .and_then(|message| match message {
                ClientJsonRpcMessage::Request(request) => {
                    request.request.get_meta().protocol_version()
                }
                _ => None,
            })
            .or_else(|| session.version.clone())
    }

    async fn post(
        &self,
        message: ClientJsonRpcMessage,
        session: &Session,
    ) -> Result<PostResponse, HttpError> {
        let dispatch_current = Arc::clone(&self.session);
        let dispatch_headers = self.config.custom_headers.clone();
        let dispatch_message = message.clone();
        self.post_with_dispatch(
            message,
            session,
            Arc::new(move || {
                let session = dispatch_current
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                crate::http_client::HttpDispatch {
                    session: session.id.clone(),
                    version: Self::effective_version(&session, Some(&dispatch_message)),
                    headers: Self::headers_from(
                        dispatch_headers.clone(),
                        &session,
                        Some(&dispatch_message),
                    ),
                }
            }),
        )
        .await
    }

    async fn post_on_stream(
        &self,
        message: ClientJsonRpcMessage,
        session: &Session,
    ) -> Result<PostResponse, HttpError> {
        let dispatch_session = session.clone();
        let dispatch_headers = self.config.custom_headers.clone();
        let dispatch_message = message.clone();
        self.post_with_dispatch(
            message,
            session,
            Arc::new(move || crate::http_client::HttpDispatch {
                session: dispatch_session.id.clone(),
                version: Self::effective_version(&dispatch_session, Some(&dispatch_message)),
                headers: Self::headers_from(
                    dispatch_headers.clone(),
                    &dispatch_session,
                    Some(&dispatch_message),
                ),
            }),
        )
        .await
    }

    async fn post_with_dispatch(
        &self,
        message: ClientJsonRpcMessage,
        session: &Session,
        dispatch: crate::http_client::DispatchProvider,
    ) -> Result<PostResponse, HttpError> {
        {
            let mut acquired = self
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(acquired) = acquired.as_mut()
                && Some(&acquired.id) == session.id.as_ref()
            {
                acquired.version = session.version.clone();
            }
        }
        let observed_current = Arc::clone(&self.session);
        let cleanup = Arc::clone(&self.cleanup);
        let response = self
            .client
            .post(PostRequest {
                uri: self.config.uri.clone(),
                message: message.clone(),
                session: session.id.clone(),
                token: None,
                headers: self.headers(session, Some(&message)),
                max_event_bytes: self.config.max_sse_event_size,
                dispatch: Some(dispatch),
                session_observer: Some(Arc::new(move |id, version| {
                    let id: Arc<str> = id.into();
                    *cleanup
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(CleanupSession {
                            id: Arc::clone(&id),
                            version: version.cloned(),
                        });
                    observed_current
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .id = Some(id);
                })),
            })
            .await?;
        if let Some(id) = Self::returned_session(&response)
            && self.snapshot().id.as_deref() != Some(id)
        {
            // Custom test/backends may return a private envelope without executing the request.
            self.remember_cleanup_session(session, Some(id));
            let mut fallback = session.clone();
            self.capture_id(&mut fallback, Some(id));
        }
        Ok(response)
    }

    fn remember_cleanup_session(&self, session: &Session, id: Option<&str>) {
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            *self
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CleanupSession {
                id: id.into(),
                version: session.version.clone(),
            });
        }
    }

    fn returned_session(response: &PostResponse) -> Option<&str> {
        match response {
            PostResponse::Accepted(id) | PostResponse::Json(_, id) | PostResponse::Sse(_, id) => {
                id.as_deref().filter(|id| !id.is_empty())
            }
        }
    }

    fn capture(&self, session: &mut Session, response: &PostResponse) {
        self.capture_id(session, Self::returned_session(response));
    }

    fn capture_id(&self, session: &mut Session, id: Option<&str>) {
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            session.id = Some(id.into());
            let mut current = self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            current.id = session.id.clone();
        }
    }

    async fn send(
        &self,
        message: &ClientJsonRpcMessage,
        responder: &mut SendAck,
        permit: Option<PostPermit>,
    ) -> Result<Tail, Arc<HttpError>> {
        let mut session = self.snapshot();
        if let ClientJsonRpcMessage::Request(request) = message
            && let Some(version) = request.request.get_meta().protocol_version()
        {
            session.version = Some(version);
        }
        // A definitive rejection reaches the exact connection owner unchanged. Only that
        // owner can open a fresh SDK service; this transport never reinitializes itself.
        let result = self.post(message.clone(), &session).await;
        // An SSE body no longer consumes an ordinary HTTP POST slot.
        drop(permit);
        let response = result?;
        self.capture(&mut session, &response);
        if matches!(message, ClientJsonRpcMessage::Notification(message)
            if matches!(message.notification, ClientNotification::InitializedNotification(_)))
        {
            self.common.send_replace(Some(session.clone()));
        }
        let id = match message {
            ClientJsonRpcMessage::Request(request) => Some(&request.id),
            _ => None,
        };
        let setup = matches!(message, ClientJsonRpcMessage::Request(request)
            if matches!(request.request, ClientRequest::InitializeRequest(_) | ClientRequest::DiscoverRequest(_)));
        match response {
            PostResponse::Accepted(_) => Ok(None),
            PostResponse::Json(messages, _) => self
                .json_response(messages, &session, id, setup, responder)
                .await
                .map_err(Arc::new),
            PostResponse::Sse(body, _) => {
                let Some(id) = id else { return Ok(None) };
                if setup && let Some(responder) = responder.take() {
                    // Setup awaits send before receiving. Acknowledge real SSE headers
                    // before bounded receive-channel backpressure (including peer pings).
                    let _ = responder.send(Ok(()));
                }
                let control = RequestControl::from_message(message);
                let tail = self
                    .response_stream(body, &session, id, control.as_ref(), setup)
                    .await
                    .map_err(|error| {
                        if setup {
                            error
                        } else {
                            // Legacy emits a local response-stream error, not an HTTP POST
                            // rejection. Keep real typed evidence without fabricating RPC.
                            HttpError::Client(PiHttpClientError::response_stream(error))
                        }
                    })?;
                Ok(tail.map(|(body, session)| ResponseTail::Sse(body, session, id.clone())))
            }
        }
    }

    async fn forward(
        &self,
        mut message: ServerJsonRpcMessage,
        session: &Session,
        id: Option<&RequestId>,
    ) -> Result<(), HttpError> {
        if let ServerJsonRpcMessage::Response(response) = &mut message {
            if session
                .version
                .as_ref()
                .is_some_and(|version| version >= &ProtocolVersion::STANDARD_HEADERS)
                && let ServerResult::ListToolsResult(result) = &mut response.result
            {
                let mut current = self
                    .session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Arc::make_mut(&mut current.tool_headers).observe(&mut result.tools);
            }
            if let ServerResult::InitializeResult(result) = &response.result {
                let mut current = self
                    .session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                current.version = Some(result.protocol_version.clone());
            } else if matches!(response.result, ServerResult::DiscoverResult(_)) {
                {
                    let mut current = self
                        .session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    current.version = session.version.clone();
                }
                self.common.send_replace(Some(self.snapshot()));
            }
        }
        if let ServerJsonRpcMessage::Request(request) = &mut message {
            request.request.extensions_mut().insert(match id {
                Some(id) => InboundStreamOrigin::OutboundRequest(id.clone()),
                None => InboundStreamOrigin::Unassociated,
            });
        }
        self.incoming
            .send(message)
            .await
            .map_err(|_| HttpError::TransportChannelClosed)
    }
}
