use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use crate::{
    AbortSignal, Message, ModelId, ModelSpec, ProviderId, ProviderPluginDriver, StreamEvent,
    ThinkingBudgets, ThinkingLevel, ToolSpec,
};

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub model: ModelId,
    /// Effective generation-local model metadata. Runtime callers populate
    /// this from the frozen catalog so protocol adapters can honor model
    /// compatibility settings without depending on catalog policy.
    pub model_spec: Option<ModelSpec>,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub thinking_level: ThinkingLevel,
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Optional output cap for standalone operations such as compaction.
    /// Providers that do not support a request-side cap may ignore it.
    pub max_output_tokens: Option<u64>,
    /// Per-request headers contributed by routing layers such as models.json.
    /// Providers may ignore these when their protocol does not use HTTP.
    pub headers: BTreeMap<String, String>,
    /// Provider-specific request parameters. Concrete providers decide how
    /// these are represented on the wire.
    pub sampling_params: BTreeMap<String, serde_json::Value>,
    /// Stable affinity key when the caller owns a persisted session. Callers
    /// without a session leave this unset.
    pub session_id: Option<String>,
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

    pub async fn before_provider_headers(
        &self,
        signal: &AbortSignal,
        headers: BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        self.provider_plugins
            .before_provider_headers(
                self.generation,
                &self.provider_id,
                &self.model_id,
                &self.cwd,
                signal,
                headers,
            )
            .await
    }

    pub async fn after_provider_response(
        &self,
        signal: &AbortSignal,
        status: u16,
        headers: BTreeMap<String, String>,
    ) {
        self.provider_plugins
            .after_provider_response(
                self.generation,
                &self.provider_id,
                &self.model_id,
                &self.cwd,
                signal,
                status,
                headers,
            )
            .await;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAvailability {
    Available,
    MissingCredentials,
}

impl ProviderAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    fn name(&self) -> String {
        self.id().to_string()
    }

    /// Credential-blind availability for the current immutable generation.
    /// Providers with request-time credential resolution should override this
    /// when they can determine configuration without resolving secret values.
    fn availability(&self) -> ProviderAvailability {
        ProviderAvailability::Available
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError>;
}

/// Classifies transient provider/transport failure text using current Pi
/// semantics. Context-overflow detection remains a caller concern because it
/// depends on the active model window and compaction policy.
pub fn is_retryable_provider_error_message(error: &str) -> bool {
    let compact = error
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    let non_retryable = [
        "gousagelimiterror",
        "freeusagelimiterror",
        "monthlyusagelimitreached",
        "availablebalance",
        "insufficientquota",
        "outofbudget",
        "quotaexceeded",
        "billing",
    ];
    if non_retryable
        .iter()
        .any(|pattern| compact.contains(pattern))
    {
        return false;
    }
    [
        "overloaded",
        "ratelimit",
        "toomanyrequests",
        "429",
        "500",
        "502",
        "503",
        "504",
        "524",
        "serviceunavailable",
        "servererror",
        "internalerror",
        "providerreturnederror",
        "exceededrequestbufferlimitwhileretryingupstream",
        "networkerror",
        "connectionerror",
        "connectionrefused",
        "connectionlost",
        "othersideclosed",
        "fetchfailed",
        "getaddrinfo",
        "enotfound",
        "eaiagain",
        "upstreamconnect",
        "resetbeforeheaders",
        "sockethangup",
        "socketconnectionwasclosed",
        "timedout",
        "timeout",
        "terminated",
        "websocketclosed",
        "websocketerror",
        "endedwithout",
        "streamendedbeforemessagestop",
        "streamendedbeforeaterminalresponseevent",
        "http2requestdidnotgetaresponse",
        "retrydelay",
        "youcanretryyourrequest",
        "tryyourrequestagain",
        "pleaseretryyourrequest",
        "resourceexhausted",
    ]
    .iter()
    .any(|pattern| compact.contains(pattern))
}

#[cfg(test)]
mod retry_tests {
    use super::is_retryable_provider_error_message;

    #[test]
    fn retry_classifier_prioritizes_terminal_quota_and_billing_failures() {
        assert!(is_retryable_provider_error_message(
            "503 service unavailable"
        ));
        assert!(is_retryable_provider_error_message(
            "connection lost while reading stream"
        ));
        assert!(!is_retryable_provider_error_message(
            "429 insufficient_quota: check billing"
        ));
    }
}
