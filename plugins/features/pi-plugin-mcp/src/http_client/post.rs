//! Private POST envelope: JSON members remain JSON, never synthesized SSE or RPC replies.
use super::*;
use rmcp::model::ServerJsonRpcMessage;

pub(crate) struct PostRequest {
    pub(crate) uri: Arc<str>,
    pub(crate) message: ClientJsonRpcMessage,
    pub(crate) session: Option<Arc<str>>,
    pub(crate) token: Option<String>,
    pub(crate) headers: HashMap<HeaderName, HeaderValue>,
    pub(crate) max_event_bytes: usize,
    /// Re-evaluated after each credential wait/retry, immediately before dispatch.
    pub(crate) dispatch: Option<DispatchProvider>,
    /// Successful response metadata is observable before content/body validation, matching the
    /// transport's session-capture order without adding it to public SDK error types.
    pub(crate) session_observer: Option<SessionObserver>,
}

pub(crate) enum PostResponse {
    Accepted(Option<String>),
    Json(JsonMessages, Option<String>),
    Sse(HttpStream, Option<String>),
}

pub(crate) struct JsonMessages(serde_json::Value);
impl JsonMessages {
    // Decode members at dispatch, not all-or-nothing: legacy emits the valid prefix
    // before the first malformed member. A syntactically invalid document emits none.
    pub(crate) fn is_batch(&self) -> bool {
        self.0.is_array()
    }

    pub(crate) fn into_iter(self) -> JsonMessageIter {
        let members = match self.0 {
            serde_json::Value::Array(members) => members,
            member => vec![member],
        };
        JsonMessageIter(members.into_iter())
    }

    pub(crate) fn into_single(self) -> Result<ServerJsonRpcMessage, HttpError> {
        decode(self.0)
    }
}

pub(crate) struct JsonMessageIter(std::vec::IntoIter<serde_json::Value>);
impl Iterator for JsonMessageIter {
    type Item = Result<ServerJsonRpcMessage, HttpError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(decode)
    }
}

fn decode(value: serde_json::Value) -> Result<ServerJsonRpcMessage, HttpError> {
    serde_json::from_value(value)
        .map_err(|_| client_error(PiHttpClientError::protocol("Invalid MCP JSON response")))
}

impl PostResponse {
    // Preserve the stock public adapter's single-message contract for its direct callers.
    // The product worker consumes this private envelope instead; no SDK type is modified.
    pub(crate) fn into_sdk(self) -> Result<StreamableHttpPostResponse, HttpError> {
        Ok(match self {
            Self::Accepted(_) => StreamableHttpPostResponse::Accepted,
            Self::Json(messages, session) => {
                StreamableHttpPostResponse::Json(messages.into_single()?, session)
            }
            Self::Sse(body, session) => StreamableHttpPostResponse::Sse(body, session),
        })
    }
}

impl PostRequest {
    /// Runs after credential acquisition, preserving the existing POST dispatch fence.
    pub(crate) async fn execute(
        &self,
        client: &PiHttpClient,
        auth_token: Option<String>,
    ) -> Result<PostResponse, HttpError> {
        let PostRequest {
            uri,
            message,
            session: session_id,
            headers: custom_headers,
            max_event_bytes: max_sse_event_size,
            dispatch,
            session_observer,
            ..
        } = self;
        let control = RequestControl::from_message(message);
        let wants_reply = matches!(&message, ClientJsonRpcMessage::Request(_));
        let discover = matches!(&message, ClientJsonRpcMessage::Request(request)
            if matches!(&request.request, ClientRequest::DiscoverRequest(_)));
        if let Some(control) = control.as_ref() {
            control
                .check_dispatch()
                .map_err(|error| error.into_http())?;
        }
        let dispatch = dispatch.as_ref().map_or_else(
            || HttpDispatch {
                session: session_id.clone(),
                version: None,
                headers: custom_headers.clone(),
            },
            |dispatch| dispatch(),
        );
        let (request, wire_session) = client.request(
            Method::POST,
            uri,
            dispatch.session.as_deref(),
            None,
            auth_token.as_deref(),
            dispatch.headers.clone(),
        )?;
        let origin = if discover && dispatch.session.is_none() && !wire_session {
            RequestOrigin::DiscoverUnattached
        } else {
            RequestOrigin::Other
        };
        let request = request.json(message);
        let response = request.send().await.map_err(network_error)?;
        check_status(
            &response,
            dispatch
                .session
                .as_deref()
                .is_some_and(|session| !session.is_empty()),
            origin,
        )?;
        let session = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if let (Some(observer), Some(session)) = (session_observer, session.as_deref()) {
            observer(session, dispatch.version.as_ref());
        }
        if !wants_reply {
            // Frozen send captures successful headers before discarding a control body.
            // Keep that metadata private; the public SDK Accepted type stays unchanged.
            return Ok(PostResponse::Accepted(session));
        }
        if matches!(
            response.status(),
            StatusCode::ACCEPTED | StatusCode::NO_CONTENT
        ) {
            return Err(client_error(PiHttpClientError::missing_reply()));
        }
        match content_type(&response).as_deref() {
            Some("application/json") => Ok(PostResponse::Json(
                JsonMessages(json_body(response).await?),
                session,
            )),
            Some("text/event-stream") => Ok(PostResponse::Sse(
                sse::bounded(response, *max_sse_event_size),
                session,
            )),
            _ => Err(client_error(PiHttpClientError::protocol(
                "Unsupported MCP response content type",
            ))),
        }
    }
}
