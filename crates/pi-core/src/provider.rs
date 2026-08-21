use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use crate::{
    AbortSignal, Message, ModelId, ProviderId, ProviderPluginDriver, StreamEvent, ThinkingLevel,
    ToolSpec,
};

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub model: ModelId,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub thinking_level: ThinkingLevel,
    /// Optional output cap for standalone operations such as compaction.
    /// Providers that do not support a request-side cap may ignore it.
    pub max_output_tokens: Option<u64>,
    /// Per-request headers contributed by routing layers such as models.json.
    /// Providers may ignore these when their protocol does not use HTTP.
    pub headers: BTreeMap<String, String>,
    /// Provider-specific request parameters. Concrete providers decide how
    /// these are represented on the wire.
    pub sampling_params: BTreeMap<String, serde_json::Value>,
}

pub type ProviderStream =
    Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static>>;

/// Generation-local execution capabilities passed separately from semantic
/// provider request data. Concrete providers invoke wire-level hooks at the
/// point where their final payload exists.
#[derive(Clone)]
pub struct ProviderCallContext {
    generation: u64,
    cwd: PathBuf,
    provider_id: ProviderId,
    model_id: ModelId,
    provider_plugins: Arc<ProviderPluginDriver>,
}

impl std::fmt::Debug for ProviderCallContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderCallContext")
            .field("generation", &self.generation)
            .field("cwd", &self.cwd)
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

impl ProviderCallContext {
    pub fn new(
        generation: u64,
        cwd: impl Into<PathBuf>,
        provider_id: ProviderId,
        model_id: ModelId,
        provider_plugins: Arc<ProviderPluginDriver>,
    ) -> Self {
        Self {
            generation,
            cwd: cwd.into(),
            provider_id,
            model_id,
            provider_plugins,
        }
    }

    pub fn without_plugins(
        cwd: impl Into<PathBuf>,
        provider_id: ProviderId,
        model_id: ModelId,
    ) -> Self {
        Self::new(
            0,
            cwd,
            provider_id,
            model_id,
            Arc::new(
                ProviderPluginDriver::new(Vec::new())
                    .expect("an empty provider plugin driver is always valid"),
            ),
        )
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn model_id(&self) -> &ModelId {
        &self.model_id
    }

    pub async fn before_provider_request(
        &self,
        signal: &AbortSignal,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        self.provider_plugins
            .before_provider_request(
                self.generation,
                &self.provider_id,
                &self.model_id,
                &self.cwd,
                signal,
                payload,
            )
            .await
            .map_err(|error| ProviderError::Failure(error.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider request aborted")]
    Aborted,
    #[error("provider protocol error: {0}")]
    Protocol(String),
    #[error("provider failed: {0}")]
    Failure(String),
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError>;
}
