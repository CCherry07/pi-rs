#![forbid(unsafe_code)]

mod messages;
mod oauth;

pub use oauth::{OAuthCredential, OAuthStart, complete_oauth, refresh, start_oauth};

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use messages::{AnthropicMode, request_body, stream as messages_stream};
use pi_core::{
    AbortSignal, ModelCost, ModelInput, ModelSpec, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream,
};
use pi_provider::{HttpTransport, ReqwestTransport, TransportError, collect_body_limited};

const PROVIDER_ID: &str = "anthropic";
const API_NAME: &str = "anthropic-messages";
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

#[derive(Clone)]
enum AnthropicCredential {
    ApiKey(String),
    Bearer(String),
    ClaudeCodeOAuth(String),
}

/// Independent Anthropic provider plugin with Claude Code OAuth-token request shaping.
pub struct AnthropicPlugin {
    provider: Arc<AnthropicProvider>,
}

impl AnthropicPlugin {
    pub fn discover() -> Self {
        Self::from_stored(None)
    }

    pub fn from_stored(stored: Option<(&str, bool)>) -> Self {
        let credential = env("ANTHROPIC_AUTH_TOKEN")
            .map(AnthropicCredential::Bearer)
            .or_else(|| env("ANTHROPIC_OAUTH_TOKEN").map(AnthropicCredential::ClaudeCodeOAuth))
            .or_else(|| env("ANTHROPIC_API_KEY").map(AnthropicCredential::ApiKey))
            .or_else(|| {
                stored.map(|(secret, oauth)| {
                    if oauth {
                        AnthropicCredential::ClaudeCodeOAuth(secret.to_string())
                    } else {
                        AnthropicCredential::ApiKey(secret.to_string())
                    }
                })
            });
        Self {
            provider: Arc::new(AnthropicProvider::new(credential)),
        }
    }

    pub fn with_api_key(api_key: impl Into<String>) -> Self {
        Self {
            provider: Arc::new(AnthropicProvider::new(Some(AnthropicCredential::ApiKey(
                api_key.into(),
            )))),
        }
    }
}

impl ProviderPlugin for AnthropicPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("anthropic-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in anthropic_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

pub struct AnthropicProvider {
    credential: Option<AnthropicCredential>,
    transport: Arc<dyn HttpTransport>,
}

impl AnthropicProvider {
    fn new(credential: Option<AnthropicCredential>) -> Self {
        Self::with_transport(credential, Arc::new(ReqwestTransport::new()))
    }

    fn with_transport(
        credential: Option<AnthropicCredential>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            credential,
            transport,
        }
    }

    fn is_oauth(&self) -> bool {
        matches!(
            self.credential,
            Some(AnthropicCredential::ClaudeCodeOAuth(_))
        )
    }

    fn headers(
        &self,
        request: &ProviderRequest,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ProviderError::Failure(
                "Anthropic requires ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, or ANTHROPIC_OAUTH_TOKEN"
                    .to_string(),
            )
        })?;
        let mut headers = BTreeMap::from([
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]);
        match credential {
            AnthropicCredential::ApiKey(key) => {
                headers.insert("x-api-key".to_string(), key.clone());
            }
            AnthropicCredential::Bearer(token) => {
                headers.insert("Authorization".to_string(), format!("Bearer {token}"));
            }
            AnthropicCredential::ClaudeCodeOAuth(token) => {
                headers.insert("Authorization".to_string(), format!("Bearer {token}"));
                headers.insert(
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14".to_string(),
                );
                headers.insert("user-agent".to_string(), "claude-cli/2.1.75".to_string());
                headers.insert("x-app".to_string(), "cli".to_string());
                headers.insert(
                    "anthropic-dangerous-direct-browser-access".to_string(),
                    "true".to_string(),
                );
            }
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        Ok(headers)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn availability(&self) -> ProviderAvailability {
        if self.credential.is_some() {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let headers = self.headers(&request)?;
        let payload = context
            .before_provider_request(
                &signal,
                request_body(
                    &request,
                    if self.is_oauth() {
                        AnthropicMode::ClaudeCode
                    } else {
                        AnthropicMode::Standard
                    },
                ),
            )
            .await?;
        let response = self
            .transport
            .post_json(ENDPOINT, &headers, &payload, signal.clone())
            .await
            .map_err(map_transport_error)?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!("HTTP {status}: {body}")));
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().contains("text/event-stream"))
        {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected text/event-stream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }
        Ok(messages_stream(
            ProviderId::new(PROVIDER_ID),
            request.model,
            API_NAME,
            response.body,
            signal,
            if self.is_oauth() {
                AnthropicMode::ClaudeCode
            } else {
                AnthropicMode::Standard
            },
        ))
    }
}

pub fn anthropic_models() -> Vec<ModelSpec> {
    vec![
        model(
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            200_000,
            64_000,
            1.0,
            5.0,
            0.1,
            1.25,
        ),
        model(
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            1_000_000,
            128_000,
            3.0,
            15.0,
            0.3,
            3.75,
        ),
        model(
            "claude-opus-4-6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            5.0,
            25.0,
            0.5,
            6.25,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn model(
    id: &str,
    name: &str,
    context: u64,
    output: u64,
    input_cost: f64,
    output_cost: f64,
    cache_read: f64,
    cache_write: f64,
) -> ModelSpec {
    let mut model = ModelSpec::new(PROVIDER_ID, id, name, API_NAME);
    model.base_url = Some("https://api.anthropic.com".to_string());
    model.reasoning = true;
    model.input = vec![ModelInput::Text, ModelInput::Image];
    model.cost = ModelCost {
        input: input_cost,
        output: output_cost,
        cache_read,
        cache_write,
        tiers: Vec::new(),
    };
    model.context_window = context;
    model.max_tokens = output;
    model
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn insert_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if let Some(existing) = headers
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.to_string());
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        other => ProviderError::Failure(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::ModelId;

    #[test]
    fn catalog_contains_current_claude_models() {
        let models = anthropic_models();
        assert!(
            models
                .iter()
                .any(|model| model.id == ModelId::new("claude-sonnet-4-6")
                    && model.context_window == 1_000_000)
        );
    }
}
