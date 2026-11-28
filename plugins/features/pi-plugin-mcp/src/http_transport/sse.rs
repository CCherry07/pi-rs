//! Stream recovery, independent of RPC replay and of the originating POST's auth allowance.
use super::*;
use futures::{StreamExt, stream::BoxStream};

pub(super) type Body = BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;
const MAX_RETRIES: usize = 5;
const MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Cursor {
    id: Option<String>,
    delay: Option<Duration>,
    received: bool,
}

impl Cursor {
    fn message(&mut self, event: sse_stream::Sse) -> Option<ServerJsonRpcMessage> {
        if let Some(id) = event.id.filter(|id| !id.contains('\0')) {
            self.id = Some(id);
        }
        if let Some(delay) = event.retry {
            self.delay = Some(Duration::from_millis(delay));
        }
        let data = event.data?;
        self.received = true;
        if data.trim().is_empty() || event.event.as_deref().is_some_and(|kind| kind != "message") {
            return None;
        }
        // Malformed message data is not a retryable transport error. Like legacy, keep
        // consuming the stream; never emit a synthetic reply or server-controlled log.
        serde_json::from_str(&data).ok()
    }

    fn delay(&self, attempt: usize) -> Duration {
        self.delay
            .unwrap_or_else(|| Duration::from_secs(1 << attempt.min(5)).min(MAX_DELAY))
    }
}

fn stream_error(error: sse_stream::Error) -> HttpError {
    if let sse_stream::Error::Body(error) = error
        && let Ok(error) = error.downcast::<PiHttpClientError>()
    {
        return HttpError::Client(*error);
    }
    HttpError::Client(PiHttpClientError::protocol("Invalid MCP SSE stream"))
}

fn retryable(error: &HttpError) -> bool {
    let HttpError::Client(error) = error else {
        return false;
    };
    match error.status() {
        Some(status) => matches!(status.as_u16(), 408 | 429 | 500 | 502..=599),
        None => error
            .network_flags()
            .is_some_and(|flags| flags.connect || flags.timeout || flags.request || flags.body),
    }
}

impl<C: HttpClient> Shared<C> {
    async fn open_stream(
        &self,
        session: &Session,
        cursor: &Cursor,
        control: Option<&RequestControl>,
    ) -> Result<(Body, Session), HttpError> {
        // The ID is mutable metadata of this transport, not an RPC authority pin.
        // Keep the originating protocol context and control, but use the current ID
        // when creating each opening (including subsequent resume GETs).
        let mut opening = self.snapshot();
        opening.version = session.version.clone();
        let dispatch_current = Arc::clone(&self.session);
        let dispatch_headers = self.config.custom_headers.clone();
        let dispatch_version = opening.version.clone();
        let observed_current = Arc::clone(&self.session);
        let cleanup = Arc::clone(&self.cleanup);
        let response = self
            .client
            .open_stream(StreamRequest {
                uri: self.config.uri.clone(),
                session: opening.id.clone(),
                cursor: cursor.id.clone(),
                token: None,
                headers: self.headers(&opening, None),
                max_event_bytes: self.config.max_sse_event_size,
                control: control.cloned(),
                dispatch: Some(Arc::new(move || {
                    let mut session = dispatch_current
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    session.version = dispatch_version.clone();
                    crate::http_client::HttpDispatch {
                        session: session.id.clone(),
                        version: session.version.clone(),
                        headers: Self::headers_from(dispatch_headers.clone(), &session, None),
                    }
                })),
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
        opening.id = response.dispatched_session.clone();
        opening.version = response.version.clone();
        if let Some(id) = response.session.as_deref()
            && self.snapshot().id.as_deref() != Some(id)
        {
            self.remember_cleanup_session(&opening, Some(id));
            self.capture_id(&mut opening, Some(id));
        } else if response.session.is_some() {
            opening.id = response.session.as_deref().map(Into::into);
        }
        Ok((response.body, opening))
    }

    pub(super) async fn common_stream(&self, session: Session) -> Result<(), HttpError> {
        let mut cursor = Cursor::default();
        let mut attempt = 0;
        loop {
            let result = async {
                let (mut body, opening) = self.open_stream(&session, &cursor, None).await?;
                let opened = tokio::time::Instant::now();
                while let Some(event) = body.next().await {
                    if let Some(message) = cursor.message(event.map_err(stream_error)?) {
                        self.forward(message, &opening, None).await?;
                    }
                }
                if cursor.received || opened.elapsed() > MAX_DELAY {
                    attempt = 0;
                }
                Ok(())
            }
            .await;
            match result {
                Err(HttpError::ServerDoesNotSupportSse) => return Ok(()),
                Err(error) if !retryable(&error) => return Err(error),
                _ => {}
            }
            cursor.received = false;
            if attempt >= MAX_RETRIES {
                return Err(HttpError::UnexpectedEndOfStream);
            }
            tokio::time::sleep(cursor.delay(attempt)).await;
            attempt += 1;
        }
    }

    pub(super) async fn response_stream(
        &self,
        body: Body,
        session: &Session,
        id: &RequestId,
        control: Option<&RequestControl>,
        setup: bool,
    ) -> Result<Option<(Body, Session)>, HttpError> {
        let mut cursor = Cursor::default();
        let mut active = session.clone();
        let mut attempt = 0;
        let mut body = Some(body);
        let mut failure = None;
        let mut pending = bootstrap::SetupNotifications::default();
        loop {
            if let Some(mut stream) = body.take() {
                failure = None;
                while let Some(event) = stream.next().await {
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            failure = Some(stream_error(error));
                            break;
                        }
                    };
                    if let Some(message) = cursor.message(event) {
                        if let Some(control) = control {
                            control
                                .check_completion()
                                .map_err(|error| error.into_http())?;
                        }
                        if setup && self.bootstrap_ping(&message, &active).await? {
                            continue;
                        }
                        let message = if setup {
                            let Some(message) =
                                pending.capture(message, self.config.max_sse_event_size)?
                            else {
                                continue;
                            };
                            if matches!(message, ServerJsonRpcMessage::Error(_)) {
                                pending.forward(self, &active, id).await?;
                            }
                            message
                        } else {
                            message
                        };
                        let done = answered(&message, id);
                        self.forward(message, &active, Some(id)).await?;
                        if done {
                            pending.forward(self, &active, id).await?;
                            return Ok(Some((stream, active)));
                        }
                    }
                }
            }
            if failure.as_ref().is_some_and(|error| !retryable(error))
                || cursor.id.is_none()
                || attempt >= MAX_RETRIES
            {
                return Err(failure.unwrap_or(HttpError::UnexpectedEndOfStream));
            }
            if cursor.received {
                attempt = 0;
            }
            cursor.received = false;
            tokio::time::sleep(cursor.delay(attempt)).await;
            attempt += 1;
            if let Some(control) = control {
                control
                    .check_dispatch()
                    .map_err(|error| error.into_http())?;
            }
            match self.open_stream(&active, &cursor, control).await {
                Ok((stream, opening)) => {
                    active = opening;
                    body = Some(stream);
                }
                Err(error) => failure = Some(error),
            }
        }
    }

    pub(super) async fn drain_tail(&self, mut body: Body, session: &Session, id: &RequestId) {
        let mut cursor = Cursor::default();
        while let Some(Ok(event)) = body.next().await {
            if let Some(message) = cursor.message(event)
                && self.forward(message, session, Some(id)).await.is_err()
            {
                return;
            }
        }
        // No reconnect here, regardless of cursor/retry values in the completed response.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, watch};

    #[tokio::test]
    async fn resume_opening_uses_latest_session_id_and_originating_protocol_context() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let observed = Arc::new(Mutex::new(None));
        let wire = Arc::clone(&observed);
        let route = axum::routing::get(move |headers: http::HeaderMap| {
            let wire = Arc::clone(&wire);
            async move {
                *wire.lock().unwrap() = Some((
                    headers["mcp-session-id"].to_str().unwrap().to_owned(),
                    headers["mcp-protocol-version"].to_str().unwrap().to_owned(),
                    headers["last-event-id"].to_str().unwrap().to_owned(),
                ));
                (
                    [("content-type", "text/event-stream")],
                    format!(
                        "data: {}\n\n",
                        json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":1,"progress":1}})
                    ),
                )
                    .into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().route("/mcp", route))
                .await
                .unwrap();
        });
        let (incoming, _messages) = mpsc::channel(1);
        let (common, _changes) = watch::channel(None);
        let shared = Shared {
            client: crate::http_client::PiHttpClient::new(),
            config: HttpTransportConfig::with_uri(uri),
            session: Arc::new(Mutex::new(Session {
                id: Some("latest-session".into()),
                version: Some(ProtocolVersion::V_2026_07_28),
                ..Session::default()
            })),
            cleanup: Arc::new(Mutex::new(None)),
            common,
            incoming,
        };
        let origin = Session {
            id: Some("origin-session".into()),
            version: Some(ProtocolVersion::V_2025_03_26),
            ..Session::default()
        };
        let cursor = Cursor {
            id: Some("resume-cursor".to_owned()),
            ..Cursor::default()
        };

        let (_body, opening) = tokio::time::timeout(
            Duration::from_secs(2),
            shared.open_stream(&origin, &cursor, None),
        )
        .await
        .expect("bounded resume opening")
        .unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            Some((
                "latest-session".to_owned(),
                "2025-03-26".to_owned(),
                "resume-cursor".to_owned(),
            ))
        );
        assert_eq!(opening.id.as_deref(), Some("latest-session"));
        assert_eq!(opening.version, Some(ProtocolVersion::V_2025_03_26));
        server.abort();
    }

    #[tokio::test]
    async fn response_tail_keeps_the_opening_protocol_context_after_global_rotation() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (incoming, mut messages) = mpsc::channel(1);
        let (common, _changes) = watch::channel(None);
        let shared = Shared {
            client: crate::http_client::PiHttpClient::new(),
            config: HttpTransportConfig::with_uri("http://127.0.0.1:1/mcp"),
            session: Arc::new(Mutex::new(Session {
                id: Some("newer-session".into()),
                version: Some(ProtocolVersion::V_2024_11_05),
                ..Session::default()
            })),
            cleanup: Arc::new(Mutex::new(None)),
            common,
            incoming,
        };
        let opening = Session {
            id: Some("opening-session".into()),
            version: Some(ProtocolVersion::STANDARD_HEADERS),
            ..Session::default()
        };
        let response = json!({
            "jsonrpc":"2.0", "id":"tail-tools", "result":{"tools":[{
                "name":"echo", "inputSchema":{"type":"object","properties":{
                    "tenant":{"type":"string","x-mcp-header":"Tenant"}
                }}
            }]}
        });
        let body = futures::stream::iter([Ok(sse_stream::Sse {
            data: Some(response.to_string()),
            ..Default::default()
        })])
        .boxed();

        shared
            .drain_tail(body, &opening, &RequestId::String("origin".into()))
            .await;
        let forwarded = messages.recv().await.unwrap();
        assert!(matches!(
            forwarded,
            ServerJsonRpcMessage::Response(ref response)
                if matches!(&response.result, ServerResult::ListToolsResult(result) if result.tools.len() == 1)
        ));
        let mut request_session = shared.snapshot();
        request_session.version = Some(ProtocolVersion::STANDARD_HEADERS);
        let request: ClientJsonRpcMessage = serde_json::from_value(json!({
            "jsonrpc":"2.0", "id":"next", "method":"tools/call",
            "params":{"name":"echo","arguments":{"tenant":"tail-context"}}
        }))
        .unwrap();
        let headers = shared.headers(&request_session, Some(&request));
        assert_eq!(
            headers
                .get(&HeaderName::from_static("mcp-param-tenant"))
                .and_then(|value| value.to_str().ok()),
            Some("tail-context"),
            "wire headers: {headers:?}"
        );
    }
}
