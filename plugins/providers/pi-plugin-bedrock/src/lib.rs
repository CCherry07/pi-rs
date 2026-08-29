#![forbid(unsafe_code)]

mod catalog;
mod credentials;
mod eventstream;
mod sigv4;
mod wire;

pub use catalog::amazon_bedrock_models;
pub use credentials::AwsCredentialSettings;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use pi_core::{
    AbortSignal, PluginId, Provider, ProviderAvailability, ProviderCallContext, ProviderError,
    ProviderId, ProviderPlugin, ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_provider::{HttpTransport, ReqwestTransport, TransportError, collect_body_limited};
use time::OffsetDateTime;
use url::Url;

use crate::credentials::AwsCredentialResolver;
use crate::sigv4::sign_request;
use crate::wire::{request_body, stream_response};

pub const BEDROCK_CONVERSE_STREAM_API: &str = "bedrock-converse-stream";
pub const AMAZON_BEDROCK_PROVIDER_ID: &str = "amazon-bedrock";
pub const DEFAULT_BEDROCK_BASE_URL: &str = "https://bedrock-runtime.us-east-1.amazonaws.com";

const MODEL_ID_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'=')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub struct AmazonBedrockPlugin {
    provider: Arc<AmazonBedrockProvider>,
}

impl AmazonBedrockPlugin {
    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None, BTreeMap::new())
    }

    pub fn from_stored(
        bearer_token: Option<String>,
        overrides: BTreeMap<String, String>,
    ) -> Result<Self, ProviderError> {
        Self::from_stored_with_transport(bearer_token, overrides, Arc::new(ReqwestTransport::new()))
    }

    pub fn from_stored_with_transport(
        bearer_token: Option<String>,
        overrides: BTreeMap<String, String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        let config = BedrockConfig::from_environment(bearer_token, overrides)?;
        Ok(Self {
            provider: Arc::new(AmazonBedrockProvider::with_transport(config, transport)?),
        })
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for AmazonBedrockPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("amazon-bedrock-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in amazon_bedrock_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BedrockConfig {
    pub provider_id: ProviderId,
    pub base_url: Option<String>,
    pub region: Option<String>,
    pub bearer_token: Option<String>,
    pub skip_auth: bool,
    pub credential_settings: AwsCredentialSettings,
    pub headers: BTreeMap<String, String>,
}

impl BedrockConfig {
    pub fn from_environment(
        bearer_token: Option<String>,
        overrides: BTreeMap<String, String>,
    ) -> Result<Self, ProviderError> {
        let credential_settings = AwsCredentialSettings::from_environment(overrides.clone());
        let region = value(&overrides, "AWS_REGION")
            .or_else(|| value(&overrides, "AWS_DEFAULT_REGION"))
            .or_else(|| env("AWS_REGION"))
            .or_else(|| env("AWS_DEFAULT_REGION"))
            .or(credential_settings.profile_region()?);
        let config = Self {
            provider_id: ProviderId::new(AMAZON_BEDROCK_PROVIDER_ID),
            base_url: value(&overrides, "AWS_BEDROCK_BASE_URL")
                .or_else(|| env("AWS_BEDROCK_BASE_URL")),
            region,
            bearer_token: bearer_token
                .or_else(|| value(&overrides, "AWS_BEARER_TOKEN_BEDROCK"))
                .or_else(|| env("AWS_BEARER_TOKEN_BEDROCK")),
            skip_auth: value(&overrides, "AWS_BEDROCK_SKIP_AUTH")
                .or_else(|| env("AWS_BEDROCK_SKIP_AUTH"))
                .is_some_and(|value| value == "1"),
            credential_settings,
            headers: BTreeMap::new(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_bearer_token(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new(AMAZON_BEDROCK_PROVIDER_ID),
            base_url: Some(base_url.into()),
            region: None,
            bearer_token: Some(token.into()),
            skip_auth: false,
            credential_settings: AwsCredentialSettings {
                values: BTreeMap::new(),
                prefer_profile: false,
            },
            headers: BTreeMap::new(),
        }
    }

    pub fn provider_id(mut self, provider_id: impl Into<ProviderId>) -> Self {
        self.provider_id = provider_id.into();
        self
    }

    fn validate(&self) -> Result<(), ProviderError> {
        if self
            .bearer_token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty() || token.contains(['\r', '\n']))
        {
            return Err(ProviderError::Failure(
                "invalid Amazon Bedrock bearer token".to_string(),
            ));
        }
        if self.region.as_deref().is_some_and(|region| {
            region.trim().is_empty()
                || region.len() > 64
                || !region
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }) {
            return Err(ProviderError::Failure(
                "invalid Amazon Bedrock region".to_string(),
            ));
        }
        if let Some(base_url) = &self.base_url {
            validate_base_url(base_url)?;
        }
        Ok(())
    }
}

pub struct AmazonBedrockProvider {
    config: BedrockConfig,
    credentials: AwsCredentialResolver,
    transport: Arc<dyn HttpTransport>,
}

impl AmazonBedrockProvider {
    pub fn new(config: BedrockConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: BedrockConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        config.validate()?;
        Ok(Self {
            credentials: AwsCredentialResolver::new(config.credential_settings.clone()),
            config,
            transport,
        })
    }

    fn target(&self, request: &ProviderRequest) -> Result<(String, String), ProviderError> {
        let arn_region = arn_region(request.model.as_str());
        let configured_region = arn_region.as_deref().or(self.config.region.as_deref());
        let model_base = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref());
        let candidate = self.config.base_url.as_deref().or(model_base);
        let inferred_region = candidate.and_then(standard_endpoint_region);
        let region = configured_region
            .or(inferred_region.as_deref())
            .unwrap_or("us-east-1");
        let base = match candidate {
            Some(base) if standard_endpoint_region(base).is_none() => base.to_string(),
            _ => standard_base_url(region),
        };
        let base = base.replace("{region}", region);
        validate_base_url(&base)?;
        let model = utf8_percent_encode(request.model.as_str(), MODEL_ID_ENCODE_SET);
        Ok((
            format!(
                "{}/model/{model}/converse-stream",
                base.trim_end_matches('/')
            ),
            region.to_string(),
        ))
    }
}

#[async_trait]
impl Provider for AmazonBedrockProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id.clone()
    }

    fn name(&self) -> String {
        if self.config.provider_id.as_str() == AMAZON_BEDROCK_PROVIDER_ID {
            "Amazon Bedrock".to_string()
        } else {
            self.config.provider_id.to_string()
        }
    }

    fn availability(&self) -> ProviderAvailability {
        if self.config.skip_auth
            || self.config.bearer_token.is_some()
            || self.credentials.has_source()
        {
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
        let (endpoint, region) = self.target(&request)?;
        let payload = context
            .before_provider_request(&signal, request_body(&request))
            .await?;
        let encoded = serde_json::to_vec(&payload).map_err(|error| {
            ProviderError::Failure(format!("failed to encode Bedrock request: {error}"))
        })?;
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Content-Type", "application/json");
        insert_header(
            &mut headers,
            "x-amzn-bedrock-accept",
            "application/vnd.amazon.eventstream",
        );
        for (name, value) in &request.headers {
            if !is_reserved_header(name) {
                insert_header(&mut headers, name, value);
            }
        }
        headers = context.before_provider_headers(&signal, headers).await;
        remove_reserved_auth_headers(&mut headers);
        if !self.config.skip_auth {
            if let Some(token) = &self.config.bearer_token {
                insert_header(&mut headers, "Authorization", format!("Bearer {token}"));
            } else {
                let credentials = self.credentials.resolve(&signal).await?;
                sign_request(
                    &mut headers,
                    &endpoint,
                    &encoded,
                    &region,
                    &credentials,
                    OffsetDateTime::now_utc(),
                )
                .map_err(ProviderError::Failure)?;
            }
        }
        let response = self
            .transport
            .post_json(&endpoint, &headers, &payload, signal.clone())
            .await
            .map_err(map_transport_error)?;
        context
            .after_provider_response(
                &signal,
                response.status,
                response.headers.iter().cloned().collect(),
            )
            .await;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!(
                "Amazon Bedrock API error ({status}): {body}"
            )));
        }
        if !response.content_type.as_deref().is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .contains("application/vnd.amazon.eventstream")
        }) {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected application/vnd.amazon.eventstream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }
        let cost = request
            .model_spec
            .as_ref()
            .map(|model| model.cost.clone())
            .unwrap_or_default();
        Ok(stream_response(
            self.config.provider_id.clone(),
            request.model,
            cost,
            response.body,
            signal,
        ))
    }
}

fn validate_base_url(base_url: &str) -> Result<(), ProviderError> {
    let base_url = base_url.replace("{region}", "us-east-1");
    let url = Url::parse(base_url.trim()).map_err(|error| {
        ProviderError::Failure(format!("invalid Amazon Bedrock base URL: {error}"))
    })?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.username() != ""
        || url.password().is_some()
    {
        return Err(ProviderError::Failure(
            "invalid Amazon Bedrock base URL".to_string(),
        ));
    }
    Ok(())
}

fn arn_region(model: &str) -> Option<String> {
    let mut fields = model.split(':');
    let arn = fields.next()?;
    let partition = fields.next()?;
    let service = fields.next()?;
    let region = fields.next()?;
    if arn == "arn" && partition.starts_with("aws") && service == "bedrock" && !region.is_empty() {
        Some(region.to_string())
    } else {
        None
    }
}

fn standard_endpoint_region(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let prefix = host
        .strip_prefix("bedrock-runtime.")
        .or_else(|| host.strip_prefix("bedrock-runtime-fips."))?;
    prefix
        .strip_suffix(".amazonaws.com")
        .or_else(|| prefix.strip_suffix(".amazonaws.com.cn"))
        .map(str::to_string)
}

fn standard_base_url(region: &str) -> String {
    let suffix = if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    format!("https://bedrock-runtime.{region}.{suffix}")
}

fn is_reserved_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "authorization" || name == "host" || name.starts_with("x-amz-")
}

fn remove_reserved_auth_headers(headers: &mut BTreeMap<String, String>) {
    let reserved = headers
        .keys()
        .filter(|name| is_reserved_header(name))
        .cloned()
        .collect::<Vec<_>>();
    for name in reserved {
        headers.remove(&name);
    }
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

fn value(values: &BTreeMap<String, String>, name: &str) -> Option<String> {
    values
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        TransportError::InvalidConfiguration(message) | TransportError::InvalidSse(message) => {
            ProviderError::Protocol(message)
        }
        error => ProviderError::Failure(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crc32fast::hash;
    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, ModelId, ThinkingLevel};
    use pi_provider::HttpResponse;
    use serde_json::{Value, json};

    use super::*;

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
            let frames = [
                event_frame("messageStart", json!({"role":"assistant"})),
                event_frame(
                    "contentBlockDelta",
                    json!({
                        "contentBlockIndex": 0,
                        "delta": {"text": "ok"}
                    }),
                ),
                event_frame("contentBlockStop", json!({"contentBlockIndex":0})),
                event_frame("messageStop", json!({"stopReason":"end_turn"})),
                event_frame(
                    "metadata",
                    json!({
                        "usage": {"inputTokens":2,"outputTokens":1,"totalTokens":3}
                    }),
                ),
            ]
            .concat();
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/vnd.amazon.eventstream".to_string()),
                headers: Vec::new(),
                body: Box::pin(stream::iter([Ok(frames)])),
            })
        }
    }

    #[tokio::test]
    async fn bearer_mode_sends_converse_request_and_decodes_eventstream() {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let provider = AmazonBedrockProvider::with_transport(
            BedrockConfig::with_bearer_token(DEFAULT_BEDROCK_BASE_URL, "bedrock-token"),
            Arc::new(CapturingTransport(Arc::clone(&capture))),
        )
        .unwrap();
        let model = amazon_bedrock_models()
            .into_iter()
            .find(|model| model.id.as_str() == "us.anthropic.claude-sonnet-4-6")
            .unwrap();
        let request = ProviderRequest {
            model: ModelId::new("us.anthropic.claude-sonnet-4-6"),
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
                    ProviderId::new(AMAZON_BEDROCK_PROVIDER_ID),
                    ModelId::new("us.anthropic.claude-sonnet-4-6"),
                ),
                signal,
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        assert!(events.iter().any(|event| matches!(event, Ok(pi_core::StreamEvent::TextDelta { delta, .. }) if delta == "ok")));
        let capture = capture.lock().unwrap();
        assert!(
            capture
                .url
                .ends_with("/model/us.anthropic.claude-sonnet-4-6/converse-stream")
        );
        assert_eq!(capture.headers["Authorization"], "Bearer bedrock-token");
        assert_eq!(capture.body["messages"], json!([]));
    }

    #[test]
    fn target_uses_arn_region_and_encodes_model_path() {
        let provider = AmazonBedrockProvider::new(BedrockConfig::with_bearer_token(
            DEFAULT_BEDROCK_BASE_URL,
            "token",
        ))
        .unwrap();
        let mut request = empty_request("arn:aws:bedrock:eu-west-1:123:inference-profile/example");
        request.model_spec.as_mut().unwrap().base_url = None;
        let (url, region) = provider.target(&request).unwrap();
        assert_eq!(region, "eu-west-1");
        assert!(url.starts_with("https://bedrock-runtime.eu-west-1.amazonaws.com/model/"));
        assert!(
            url.contains("arn%3Aaws%3Abedrock%3Aeu-west-1%3A123%3Ainference-profile%2Fexample")
        );
    }

    fn empty_request(model: &str) -> ProviderRequest {
        ProviderRequest {
            model: ModelId::new(model),
            model_spec: Some(pi_core::ModelSpec::new(
                AMAZON_BEDROCK_PROVIDER_ID,
                model,
                model,
                BEDROCK_CONVERSE_STREAM_API,
            )),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Off,
            thinking_budgets: None,
            max_output_tokens: None,
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        }
    }

    fn event_frame(event_type: &str, payload: Value) -> Vec<u8> {
        let payload = serde_json::to_vec(&payload).unwrap();
        let mut headers = Vec::new();
        for (name, value) in [
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let total_len = 12 + headers.len() + payload.len() + 4;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(total_len as u32).to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&hash(&frame).to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&hash(&frame).to_be_bytes());
        frame
    }
}
