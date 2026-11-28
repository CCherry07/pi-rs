//! Static-header dispatch fence over the private HTTP backend and stock SDK transport.
use super::RequestControl;
use crate::http_client::{HttpClient, PiHttpClient, PiHttpClientError, PostRequest, StreamRequest};
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
    StreamableHttpPostResponse,
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
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session,
            auth,
            headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
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
        <Self as HttpClient>::post(
            self,
            PostRequest::from_sdk(uri, message, session, auth, headers, max),
        )
        .await?
        .into_sdk()
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
        self.get_stream_with_max_sse_event_size(
            uri,
            session,
            last,
            auth,
            headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
        .await
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
        <Self as HttpClient>::open_stream(
            self,
            StreamRequest::from_sdk(uri, session, last, auth, headers, max),
        )
        .await
        .map(|response| response.body)
    }
}
