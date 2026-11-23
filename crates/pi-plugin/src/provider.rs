use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::Stream;

use crate::{
    AbortSignal, Message, ModelId, ModelSpec, ProviderId, ProviderPluginDriver, StreamEvent,
    ThinkingBudgets, ThinkingLevel, ToolSpec,
};

/// Request-local prompt-cache policy. An absent override preserves the provider default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

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
    /// Overrides provider/environment prompt-cache retention for this request only.
    pub cache_retention: Option<CacheRetention>,
    /// Per-request headers contributed by routing layers such as models.json.
    /// Providers may ignore these when their protocol does not use HTTP.
    pub headers: BTreeMap<String, String>,
    /// Provider-specific request parameters. Concrete providers decide how
    /// these are represented on the wire.
    pub sampling_params: BTreeMap<String, serde_json::Value>,
    /// Routing identity, normally the persisted session affinity key. Standalone
    /// callers may supply a fresh identity without enabling prompt caching.
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

    /// Credential-blind availability for one typed catalog entry. Routing
    /// overlays can distinguish per-model credentials without exposing them.
    fn model_availability(&self, _model: &crate::AnyModelSpec) -> ProviderAvailability {
        self.availability()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let _ = (request, context, signal);
        Err(ProviderError::Protocol(
            "provider does not support chat".into(),
        ))
    }

    async fn classify(
        &self,
        model: crate::ClassifierModelSpec,
        request: crate::ClassifierContext,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<crate::ClassifierResult, ProviderError> {
        let _ = (model, request, context, signal);
        Err(ProviderError::Protocol(
            "provider does not support classification".into(),
        ))
    }

    async fn generate_images(
        &self,
        model: crate::ImageModelSpec,
        request: crate::ImagesContext,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<crate::ImagesResult, ProviderError> {
        let _ = (model, request, context, signal);
        Err(ProviderError::Protocol(
            "provider does not support image generation".into(),
        ))
    }
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
        "subscriptionsharingusagelimitexceeded",
        "subscriptionsharingworkplaceindividualusagelimitexceeded",
    ];
    if non_retryable
        .iter()
        .any(|pattern| compact.contains(pattern))
        || has_json_error_code(error, "1308")
    {
        return false;
    }
    // Keep numeric boundaries: request IDs and byte counts are not HTTP statuses.
    if error
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| matches!(token, "429" | "500" | "502" | "503" | "504" | "520" | "524"))
    {
        return true;
    }
    let lower = error.to_ascii_lowercase();
    if lower
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|token| {
            matches!(
                token,
                "econnreset" | "econnaborted" | "econnrefused" | "etimedout" | "epipe"
            )
        })
        || lower.split("request to ").skip(1).any(|tail| {
            tail.split_once(" failed, reason:")
                .is_some_and(|(url, _)| !url.is_empty() && !url.chars().any(char::is_whitespace))
        })
    {
        return true;
    }
    [
        "overloaded",
        "serverbusy",
        "serversarecurrentlybusy",
        "currentlyexperiencinghighdemand",
        "modelisatcapacity",
        "ratelimit",
        "toomanyrequests",
        "serviceunavailable",
        "serviceisunavailable",
        "servicetemporarilyunavailable",
        "serviceistemporarilyunavailable",
        "servererror",
        "internalerror",
        "providerreturnederror",
        "providerreturnedanerror",
        "exceededrequestbufferlimitwhileretryingupstream",
        "unabletoforwardupstream",
        "upstreamrequestfailed",
        "upstreamstreamreaderror",
        "streamwasinterrupted",
        "http2streamfailed",
        "networkerror",
        "connectionerror",
        "connectionrefused",
        "connectionlost",
        "connectionclosed",
        "connectionresetbypeer",
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
        "streamdisconnectedbeforecompletion",
        "cutshortafterthereplyhadbegun",
        "pendingstreamhasbeencanceled",
        "retrydelay",
        "youcanretryyourrequest",
        "tryyourrequestagain",
        "pleaseretryyourrequest",
        "resourceexhausted",
        "subscriptionsharingusageunavailable",
        "subscriptionsharinguserunavailable",
    ]
    .iter()
    .any(|pattern| compact.contains(pattern))
}

// Pi's ZAI plan-limit rule matches the JSON error code, not arbitrary numbers
// elsewhere in the provider message. Both numeric and string codes are valid.
fn has_json_error_code(message: &str, code: &str) -> bool {
    message
        .to_ascii_lowercase()
        .split("\"code\"")
        .skip(1)
        .any(|tail| {
            let Some(value) = tail.trim_start().strip_prefix(':') else {
                return false;
            };
            let value = value.trim_start();
            let value = value.strip_prefix('"').unwrap_or(value);
            value
                .strip_prefix(code)
                .is_some_and(|tail| !tail.starts_with(|character: char| character.is_ascii_digit()))
        })
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

    #[test]
    fn retry_classifier_does_not_read_status_codes_inside_identifiers() {
        for message in [
            "HTTP 401 Unauthorized\n{\"error\":\"invalid_api_key\",\"request_id\":\"req500abc\"}",
            "400 Bad Request: maximum size is 524288 bytes",
            "invalid request abc429 def502 ghi503 jkl504 mno520 pqr524",
        ] {
            assert!(!is_retryable_provider_error_message(message), "{message}");
        }
        for message in ["HTTP 500 Internal Server Error", "upstream returned (429)"] {
            assert!(is_retryable_provider_error_message(message), "{message}");
        }
    }

    #[test]
    fn retry_classifier_treats_subscription_and_plan_quotas_as_terminal() {
        for message in [
            "HTTP 429 Too Many Requests\n{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}",
            "429 subscription_sharing_workplace_individual_usage_limit_exceeded",
            "HTTP 429 Too Many Requests\n{\"error\":{\"code\":\"1308\",\"message\":\"limit reached\"}}",
            "429 {\"error\":{\"code\" : 1308}}",
        ] {
            assert!(!is_retryable_provider_error_message(message), "{message}");
        }
        for message in ["429 {\"code\":13080}", "429 {\"request_id\":\"1308\"}"] {
            assert!(is_retryable_provider_error_message(message), "{message}");
        }
    }

    #[test]
    fn retry_classifier_handles_current_pi_transient_errors() {
        for message in [
            "{\"type\":\"api_error\",\"message\":\"Connection closed.\"}",
            "read ECONNRESET",
            "write EPIPE",
            "connect ETIMEDOUT",
            "ECONNABORTED",
            "ECONNREFUSED",
            "HTTP 520 proxy failure",
            "server_busy",
            "servers are currently busy",
            "currently experiencing high demand",
            "model is at capacity",
            "The service is temporarily unavailable",
            "The upstream provider returned an error",
            "unable to forward upstream",
            "upstream_request_failed",
            "upstream_stream_read_error",
            "stream was interrupted",
            "HTTP/2 stream failed",
            "connection reset by peer",
            "request to https://example.test failed, reason:",
            "stream disconnected before completion",
            "cut short after the reply had begun",
            "pending stream has been canceled",
            "subscription_sharing_usage_unavailable",
            "subscription_sharing_user_unavailable",
        ] {
            assert!(is_retryable_provider_error_message(message), "{message}");
        }
        for message in ["bad field MY_EPIPE_FIELD", "invalid ID abcECONNRESET123"] {
            assert!(!is_retryable_provider_error_message(message), "{message}");
        }
    }
}
