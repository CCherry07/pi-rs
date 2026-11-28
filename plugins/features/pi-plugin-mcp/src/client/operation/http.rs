//! Static-header dispatch fence over the private HTTP backend and stock SDK transport.
use super::RequestControl;
use crate::http_client::{PiHttpClient, PiHttpClientError};
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Default)]
pub(crate) struct DispatchHttpClient(PiHttpClient);

impl crate::http_client::HttpClient for DispatchHttpClient {
    async fn post(
        &self,
        request: crate::http_client::PostRequest,
    ) -> Result<crate::http_client::PostResponse, crate::http_client::HttpError> {
        if let Some(control) = RequestControl::from_message(&request.message) {
            control
                .check_dispatch()
                .map_err(|error| error.into_http())?;
        }
        request.execute(&self.0, request.token.clone()).await
    }

    async fn open_stream(
        &self,
        request: crate::http_client::StreamRequest,
    ) -> Result<crate::http_client::StreamResponse, crate::http_client::HttpError> {
        request.execute(&self.0, request.token.clone()).await
    }
}

impl StreamableHttpClient for DispatchHttpClient {
    type Error = PiHttpClientError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        if let Some(control) = RequestControl::from_message(&message) {
            control
                .check_dispatch()
                .map_err(|error| error.into_http())?;
        }
        self.0
            .post_message(uri, message, session, auth, headers)
            .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        if let Some(control) = RequestControl::from_message(&message) {
            control
                .check_dispatch()
                .map_err(|error| error.into_http())?;
        }
        self.0
            .post_message_with_max_sse_event_size(uri, message, session, auth, headers, max)
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.0.delete_session(uri, session, auth, headers).await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<
        BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>,
        StreamableHttpError<Self::Error>,
    > {
        self.0.get_stream(uri, session, last, auth, headers).await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max: usize,
    ) -> Result<
        BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>,
        StreamableHttpError<Self::Error>,
    > {
        self.0
            .get_stream_with_max_sse_event_size(uri, session, last, auth, headers, max)
            .await
    }
}
