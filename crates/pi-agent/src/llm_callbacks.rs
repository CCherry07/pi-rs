use std::fmt;
use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use pi_core::{
    AbortSignal, AssistantMessage, FrozenRegistries, Message, ProviderCallContext, ProviderError,
    ProviderId, ProviderRequest, ProviderStream,
};

type ConvertFn = dyn Fn(Vec<Message>) -> BoxFuture<'static, Vec<Message>> + Send + Sync;

/// Projects agent messages into provider messages after context transformation.
/// The callback owns its input; changing it does not change the stored transcript.
/// By default, only user, assistant and tool-result messages reach the provider.
#[derive(Clone, Default)]
pub struct ConvertToLlm(Option<Arc<ConvertFn>>);

impl ConvertToLlm {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(Vec<Message>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<Message>> + Send + 'static,
    {
        Self(Some(Arc::new(move |messages| Box::pin(callback(messages)))))
    }

    /// Whether no caller-specific projection has been configured.
    pub fn is_default(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) async fn call(&self, messages: Vec<Message>) -> Vec<Message> {
        match &self.0 {
            Some(callback) => callback(messages).await,
            None => messages
                .into_iter()
                .filter(|message| {
                    matches!(
                        message,
                        Message::User(_) | Message::Assistant(_) | Message::ToolResult(_)
                    )
                })
                .collect(),
        }
    }
}

impl fmt::Debug for ConvertToLlm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConvertToLlm").finish_non_exhaustive()
    }
}

type TransformFn =
    dyn Fn(Vec<Message>, AbortSignal) -> BoxFuture<'static, Vec<Message>> + Send + Sync;

/// Run-local context pruning or augmentation before [`ConvertToLlm`].
#[derive(Clone)]
pub struct TransformContext(Arc<TransformFn>);

impl TransformContext {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(Vec<Message>, AbortSignal) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<Message>> + Send + 'static,
    {
        Self(Arc::new(move |messages, signal| {
            Box::pin(callback(messages, signal))
        }))
    }

    pub(crate) async fn call(&self, messages: Vec<Message>, signal: AbortSignal) -> Vec<Message> {
        (self.0)(messages, signal).await
    }
}

impl fmt::Debug for TransformContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransformContext").finish_non_exhaustive()
    }
}

/// A provider event stream, or a terminal response with no preceding deltas.
/// A complete response emits `message_start` and `message_end` without updates.
pub enum AssistantResponse {
    Stream(ProviderStream),
    Complete(Box<AssistantMessage>),
}

impl From<AssistantMessage> for AssistantResponse {
    fn from(message: AssistantMessage) -> Self {
        Self::Complete(Box::new(message))
    }
}

pub(crate) enum StreamFnError {
    ProviderNotFound(ProviderId),
    Provider(ProviderError),
}

type StreamCallback = dyn Fn(
        ProviderRequest,
        ProviderCallContext,
        AbortSignal,
    ) -> BoxFuture<'static, Result<AssistantResponse, StreamFnError>>
    + Send
    + Sync;

/// Injectable provider dispatch, shared immutably by a run or runtime generation.
#[derive(Clone)]
pub struct StreamFn(Arc<StreamCallback>);

impl StreamFn {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(ProviderRequest, ProviderCallContext, AbortSignal) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<AssistantResponse, ProviderError>> + Send + 'static,
    {
        Self(Arc::new(move |request, context, signal| {
            let future = callback(request, context, signal);
            Box::pin(async move { future.await.map_err(StreamFnError::Provider) })
        }))
    }

    /// Builds the product default from a frozen generation's provider registry.
    pub fn from_registries(registries: Arc<FrozenRegistries>) -> Self {
        Self(Arc::new(move |request, context, signal| {
            let registries = Arc::clone(&registries);
            Box::pin(async move {
                let provider = registries.provider(context.provider_id()).ok_or_else(|| {
                    StreamFnError::ProviderNotFound(context.provider_id().clone())
                })?;
                provider
                    .stream(request, context, signal)
                    .await
                    .map(AssistantResponse::Stream)
                    .map_err(StreamFnError::Provider)
            })
        }))
    }

    pub(crate) async fn call(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<AssistantResponse, StreamFnError> {
        (self.0)(request, context, signal).await
    }
}

impl fmt::Debug for StreamFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamFn").finish_non_exhaustive()
    }
}
