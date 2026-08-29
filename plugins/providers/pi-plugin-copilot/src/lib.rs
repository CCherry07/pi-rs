#![forbid(unsafe_code)]

mod catalog;
mod oauth;

pub use catalog::github_copilot_models;
pub use oauth::{
    DeviceAuthorization as GitHubCopilotDeviceAuthorization,
    OAuthCredential as GitHubCopilotOAuthCredential, normalize_enterprise_domain,
    poll_device_authorization as poll_github_copilot_device_authorization,
    refresh as refresh_github_copilot_oauth,
    start_device_authorization as start_github_copilot_device_authorization,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, ContentBlock, Message, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream,
};
use pi_plugin_anthropic::{
    ANTHROPIC_MESSAGES_API, AnthropicCompatibleConfig, AnthropicCompatibleProvider,
};
use pi_plugin_openai::{
    OPENAI_RESPONSES_API, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    OpenAiResponsesCompatibleProvider,
};
use pi_provider::{HttpTransport, ReqwestTransport};
use reqwest::Url;

pub const GITHUB_COPILOT_PROVIDER_ID: &str = "github-copilot";
pub const COPILOT_BASE_URL: &str = "https://api.individual.githubcopilot.com";
const OPENAI_COMPLETIONS_API: &str = "openai-completions";

#[derive(Clone, Copy)]
pub struct GitHubCopilotStoredCredential<'a> {
    pub token: &'a str,
    pub enterprise_domain: Option<&'a str>,
    pub available_model_ids: Option<&'a [String]>,
}

pub struct GitHubCopilotPlugin {
    provider: Arc<GitHubCopilotProvider>,
    available_model_ids: Option<BTreeSet<String>>,
}

impl GitHubCopilotPlugin {
    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None)
    }

    pub fn from_stored(stored: Option<(&str, Option<&str>)>) -> Result<Self, ProviderError> {
        Self::from_stored_with_transport(stored, Arc::new(ReqwestTransport::new()))
    }

    pub fn from_stored_with_transport(
        stored: Option<(&str, Option<&str>)>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::from_stored_catalog_with_transport(
            stored.map(|(token, enterprise_domain)| GitHubCopilotStoredCredential {
                token,
                enterprise_domain,
                available_model_ids: None,
            }),
            transport,
        )
    }

    pub fn from_stored_catalog_with_transport(
        stored: Option<GitHubCopilotStoredCredential<'_>>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        let stored_token = stored.map(|stored| stored.token);
        let stored_domain = stored.and_then(|stored| stored.enterprise_domain);
        let stored_models = stored.and_then(|stored| stored.available_model_ids);
        let environment_token = env("COPILOT_GITHUB_TOKEN");
        let token = environment_token
            .clone()
            .or_else(|| stored_token.map(str::to_string));
        let enterprise_domain =
            env("GITHUB_COPILOT_ENTERPRISE_DOMAIN").or_else(|| stored_domain.map(str::to_string));
        let available_model_ids = environment_token
            .is_none()
            .then(|| stored_models.map(|models| models.iter().cloned().collect::<BTreeSet<_>>()));
        Self::new_with_catalog_and_transport(
            token,
            enterprise_domain,
            available_model_ids.flatten(),
            transport,
        )
    }

    pub fn new_with_transport(
        token: Option<String>,
        enterprise_domain: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_catalog_and_transport(token, enterprise_domain, None, transport)
    }

    pub fn new_with_catalog_and_transport(
        token: Option<String>,
        enterprise_domain: Option<String>,
        available_model_ids: Option<BTreeSet<String>>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(GitHubCopilotProvider::with_transport(
                token,
                enterprise_domain,
                transport,
            )?),
            available_model_ids,
        })
    }

    fn models(&self) -> Vec<pi_core::ModelSpec> {
        github_copilot_models()
            .into_iter()
            .filter(|model| {
                self.available_model_ids
                    .as_ref()
                    .is_none_or(|available| available.contains(model.id.as_str()))
            })
            .collect()
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for GitHubCopilotPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("github-copilot-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in self.models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

pub struct GitHubCopilotProvider {
    token: Option<String>,
    enterprise_domain: Option<String>,
    transport: Arc<dyn HttpTransport>,
}

impl GitHubCopilotProvider {
    pub fn new(
        token: Option<String>,
        enterprise_domain: Option<String>,
    ) -> Result<Self, ProviderError> {
        Self::with_transport(token, enterprise_domain, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        token: Option<String>,
        enterprise_domain: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        if token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty() || token.contains(['\r', '\n']))
        {
            return Err(ProviderError::Failure(
                "invalid GitHub Copilot token".to_string(),
            ));
        }
        let enterprise_domain = enterprise_domain
            .as_deref()
            .map(normalize_enterprise_domain)
            .transpose()
            .map_err(ProviderError::Failure)?
            .flatten();
        Ok(Self {
            token,
            enterprise_domain,
            transport,
        })
    }

    fn base_url(&self, token: &str) -> String {
        base_url_from_token(token)
            .or_else(|| {
                self.enterprise_domain
                    .as_ref()
                    .map(|domain| format!("https://copilot-api.{domain}"))
            })
            .unwrap_or_else(|| COPILOT_BASE_URL.to_string())
    }
}

#[async_trait]
impl Provider for GitHubCopilotProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(GITHUB_COPILOT_PROVIDER_ID)
    }

    fn name(&self) -> String {
        "GitHub Copilot".to_string()
    }

    fn availability(&self) -> ProviderAvailability {
        if self.token.is_some() {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn stream(
        &self,
        mut request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let token = self.token.as_deref().ok_or_else(|| {
            ProviderError::Failure(
                "GitHub Copilot requires COPILOT_GITHUB_TOKEN or stored OAuth credentials"
                    .to_string(),
            )
        })?;
        let mut model = request
            .model_spec
            .clone()
            .or_else(|| {
                github_copilot_models()
                    .into_iter()
                    .find(|model| model.id == request.model)
            })
            .ok_or_else(|| {
                ProviderError::Failure(format!(
                    "GitHub Copilot has no protocol metadata for model {}",
                    request.model
                ))
            })?;
        let base_url = self.base_url(token);
        model.base_url = Some(base_url.clone());
        let api = model.api.clone();
        request.model_spec = Some(model);
        apply_copilot_headers(&mut request, token);

        match api.as_str() {
            OPENAI_COMPLETIONS_API => {
                let provider = OpenAiCompatibleProvider::with_transport(
                    OpenAiCompatibleConfig::without_api_key(base_url)
                        .provider_id(GITHUB_COPILOT_PROVIDER_ID),
                    Arc::clone(&self.transport),
                )?;
                provider.stream(request, context, signal).await
            }
            OPENAI_RESPONSES_API => {
                let provider = OpenAiResponsesCompatibleProvider::with_transport(
                    OpenAiCompatibleConfig::without_api_key(base_url)
                        .provider_id(GITHUB_COPILOT_PROVIDER_ID),
                    Arc::clone(&self.transport),
                )?;
                provider.stream(request, context, signal).await
            }
            ANTHROPIC_MESSAGES_API => {
                let provider = AnthropicCompatibleProvider::with_transport(
                    AnthropicCompatibleConfig::without_api_key(base_url)
                        .provider_id(GITHUB_COPILOT_PROVIDER_ID),
                    Arc::clone(&self.transport),
                )?;
                provider.stream(request, context, signal).await
            }
            _ => Err(ProviderError::Failure(format!(
                "GitHub Copilot does not implement model API {api:?}"
            ))),
        }
    }
}

fn apply_copilot_headers(request: &mut ProviderRequest, token: &str) {
    let initiator = match request.messages.last() {
        Some(Message::User(_) | Message::Custom(_)) | None => "user",
        Some(Message::Assistant(_) | Message::ToolResult(_)) => "agent",
    };
    let has_images = request.messages.iter().any(|message| match message {
        Message::User(message) => message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_))),
        Message::ToolResult(message) => message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_))),
        Message::Custom(message) => message
            .content
            .to_blocks()
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_))),
        Message::Assistant(_) => false,
    });
    for (name, value) in [
        ("Authorization", format!("Bearer {token}")),
        ("User-Agent", "GitHubCopilotChat/0.35.0".to_string()),
        ("Editor-Version", "vscode/1.107.0".to_string()),
        ("Editor-Plugin-Version", "copilot-chat/0.35.0".to_string()),
        ("Copilot-Integration-Id", "vscode-chat".to_string()),
        ("X-Initiator", initiator.to_string()),
        ("Openai-Intent", "conversation-edits".to_string()),
    ] {
        insert_header(&mut request.headers, name, value);
    }
    if has_images {
        insert_header(&mut request.headers, "Copilot-Vision-Request", "true");
    }
}

fn base_url_from_token(token: &str) -> Option<String> {
    let proxy_host = token
        .split(';')
        .find_map(|part| part.trim().strip_prefix("proxy-ep="))?;
    let api_host = proxy_host.strip_prefix("proxy.").unwrap_or(proxy_host);
    let url = Url::parse(&format!("https://api.{api_host}")).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "https"
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !host.ends_with(".githubcopilot.com")
    {
        return None;
    }
    Some(format!("https://{host}"))
}

fn insert_header(
    headers: &mut BTreeMap<String, String>,
    name: impl AsRef<str>,
    value: impl Into<String>,
) {
    let name = name.as_ref();
    if let Some(existing) = headers
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.into());
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, ModelId, ThinkingLevel};
    use pi_provider::{HttpResponse, TransportError};
    use serde_json::Value;

    use super::*;

    #[test]
    fn token_proxy_endpoint_is_validated_and_normalized() {
        assert_eq!(
            base_url_from_token("tid=1;proxy-ep=proxy.individual.githubcopilot.com;exp=1"),
            Some("https://api.individual.githubcopilot.com".to_string())
        );
        assert_eq!(base_url_from_token("proxy-ep=evil.example"), None);
    }

    #[test]
    fn oauth_account_catalog_filters_registered_models() {
        let plugin = GitHubCopilotPlugin::new_with_catalog_and_transport(
            Some("token".to_string()),
            None,
            Some(BTreeSet::from(["gpt-4.1".to_string()])),
            Arc::new(ReqwestTransport::new()),
        )
        .unwrap();
        let models = plugin.models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_str(), "gpt-4.1");
    }

    #[derive(Default)]
    struct Capture {
        url: String,
        headers: BTreeMap<String, String>,
        body: Value,
    }

    struct CapturingTransport(Arc<Mutex<Capture>>);

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self.0.lock().unwrap() = Capture {
                url: url.to_string(),
                headers: headers.clone(),
                body: body.clone(),
            };
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/event-stream".to_string()),
                headers: Vec::new(),
                body: Box::pin(stream::iter([Ok(
                    b"data: {\"id\":\"response-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                        .to_vec(),
                )])),
            })
        }
    }

    #[tokio::test]
    async fn wrapper_routes_protocol_and_adds_required_dynamic_headers() {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let provider = GitHubCopilotProvider::with_transport(
            Some("tid=1;proxy-ep=proxy.individual.githubcopilot.com;exp=1".to_string()),
            None,
            Arc::new(CapturingTransport(Arc::clone(&capture))),
        )
        .unwrap();
        let model = github_copilot_models()
            .into_iter()
            .find(|model| model.id.as_str() == "gpt-4.1")
            .unwrap();
        let request = ProviderRequest {
            model: ModelId::new("gpt-4.1"),
            model_spec: Some(model),
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Off,
            thinking_budgets: None,
            max_output_tokens: Some(100),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let (_, signal) = AbortHandle::new();
        let events = provider
            .stream(
                request,
                ProviderCallContext::without_plugins(
                    ".",
                    ProviderId::new(GITHUB_COPILOT_PROVIDER_ID),
                    ModelId::new("gpt-4.1"),
                ),
                signal,
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok));
        let capture = capture.lock().unwrap();
        assert_eq!(capture.url, format!("{COPILOT_BASE_URL}/chat/completions"));
        assert_eq!(capture.headers["X-Initiator"], "user");
        assert_eq!(capture.headers["Openai-Intent"], "conversation-edits");
        assert!(capture.headers["Authorization"].starts_with("Bearer tid=1"));
        assert_eq!(capture.body["model"], "gpt-4.1");
    }
}
