use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use gcp_auth::TokenProvider;
use pi_core::{
    AbortSignal, ModelCost, ModelInput, ModelSpec, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream,
};
use pi_provider::{
    HttpTransport, ReqwestTransport, TransportError, collect_body_limited,
    post_json_with_provider_hooks,
};
use tokio::sync::OnceCell;

use crate::{request_body, stream_response};

pub const GOOGLE_VERTEX_API: &str = "google-vertex";
const GOOGLE_VERTEX_PROVIDER_ID: &str = "google-vertex";
const VERTEX_EXPRESS_BASE_URL: &str = "https://aiplatform.googleapis.com";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

pub struct GoogleVertexPlugin {
    provider: Arc<GoogleVertexCompatibleProvider>,
}

impl GoogleVertexPlugin {
    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::from_stored_with_environment(api_key, BTreeMap::new())
    }

    pub fn from_stored_with_environment(
        api_key: Option<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<Self, ProviderError> {
        Self::from_stored_with_environment_and_transport(
            api_key,
            environment,
            Arc::new(ReqwestTransport::new()),
        )
    }

    pub fn from_stored_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::from_stored_with_environment_and_transport(api_key, BTreeMap::new(), transport)
    }

    pub fn from_stored_with_environment_and_transport(
        api_key: Option<String>,
        environment: BTreeMap<String, String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_environment_and_transport(
            api_key.or_else(|| env("GOOGLE_CLOUD_API_KEY")),
            environment,
            transport,
        )
    }

    pub fn new_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_environment_and_transport(api_key, BTreeMap::new(), transport)
    }

    pub fn new_with_environment_and_transport(
        api_key: Option<String>,
        environment: BTreeMap<String, String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider: Arc::new(GoogleVertexCompatibleProvider::with_transport(
                GoogleVertexCompatibleConfig::from_environment_with_overrides(
                    api_key,
                    environment,
                )?,
                transport,
            )?),
        })
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for GoogleVertexPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("google-vertex-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in google_vertex_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GoogleVertexCompatibleConfig {
    pub provider_id: ProviderId,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub application_credentials: Option<PathBuf>,
    pub headers: BTreeMap<String, String>,
    pub allow_adc: bool,
}

impl GoogleVertexCompatibleConfig {
    pub fn with_api_key(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new(GOOGLE_VERTEX_PROVIDER_ID),
            base_url: Some(base_url.into()),
            api_key: Some(api_key.into()),
            access_token: None,
            project: None,
            location: None,
            application_credentials: None,
            headers: BTreeMap::new(),
            allow_adc: false,
        }
    }

    pub fn without_api_key(base_url: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new(GOOGLE_VERTEX_PROVIDER_ID),
            base_url: Some(base_url.into()),
            api_key: None,
            access_token: None,
            project: None,
            location: None,
            application_credentials: None,
            headers: BTreeMap::new(),
            allow_adc: false,
        }
    }

    pub fn from_environment(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::from_environment_with_overrides(api_key, BTreeMap::new())
    }

    pub fn from_environment_with_overrides(
        api_key: Option<String>,
        overrides: BTreeMap<String, String>,
    ) -> Result<Self, ProviderError> {
        let config = Self {
            provider_id: ProviderId::new(GOOGLE_VERTEX_PROVIDER_ID),
            base_url: None,
            api_key: api_key.and_then(normalize_api_key),
            access_token: value(&overrides, "GOOGLE_CLOUD_ACCESS_TOKEN")
                .or_else(|| value(&overrides, "GOOGLE_OAUTH_ACCESS_TOKEN"))
                .or_else(|| env("GOOGLE_CLOUD_ACCESS_TOKEN"))
                .or_else(|| env("GOOGLE_OAUTH_ACCESS_TOKEN")),
            project: value(&overrides, "GOOGLE_CLOUD_PROJECT")
                .or_else(|| value(&overrides, "GCLOUD_PROJECT"))
                .or_else(|| env("GOOGLE_CLOUD_PROJECT"))
                .or_else(|| env("GCLOUD_PROJECT")),
            location: value(&overrides, "GOOGLE_CLOUD_LOCATION")
                .or_else(|| env("GOOGLE_CLOUD_LOCATION")),
            application_credentials: value(&overrides, "GOOGLE_APPLICATION_CREDENTIALS")
                .or_else(|| env("GOOGLE_APPLICATION_CREDENTIALS"))
                .map(expand_user_path),
            headers: BTreeMap::new(),
            allow_adc: true,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn provider_id(mut self, provider_id: impl Into<ProviderId>) -> Self {
        self.provider_id = provider_id.into();
        self
    }

    pub fn access_token(mut self, access_token: impl Into<String>) -> Self {
        self.access_token = Some(access_token.into());
        self
    }

    pub fn project_location(
        mut self,
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        self.project = Some(project.into());
        self.location = Some(location.into());
        self
    }

    fn validate(&self) -> Result<(), ProviderError> {
        for (name, value) in [
            ("API key", self.api_key.as_deref()),
            ("access token", self.access_token.as_deref()),
        ] {
            if value.is_some_and(|value| value.contains(['\r', '\n'])) {
                return Err(ProviderError::Failure(format!(
                    "invalid Google Vertex {name}"
                )));
            }
        }
        for (name, value) in [
            ("project", self.project.as_deref()),
            ("location", self.location.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty() || value.contains(['/', '?', '#']))
            {
                return Err(ProviderError::Failure(format!(
                    "invalid Google Vertex {name}"
                )));
            }
        }
        Ok(())
    }
}

pub struct GoogleVertexCompatibleProvider {
    config: GoogleVertexCompatibleConfig,
    transport: Arc<dyn HttpTransport>,
    token_provider: OnceCell<Arc<dyn TokenProvider>>,
}

impl GoogleVertexCompatibleProvider {
    pub fn new(config: GoogleVertexCompatibleConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: GoogleVertexCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        config.validate()?;
        Ok(Self {
            config,
            transport,
            token_provider: OnceCell::new(),
        })
    }

    async fn credentials(&self, signal: &AbortSignal) -> Result<VertexCredentials, ProviderError> {
        if let Some(api_key) = &self.config.api_key {
            return Ok(VertexCredentials::ApiKey(api_key.clone()));
        }
        if let Some(token) = &self.config.access_token {
            return Ok(VertexCredentials::Bearer(token.clone()));
        }
        if !self.config.allow_adc {
            return Err(ProviderError::Failure(
                "Google Vertex credentials are missing".to_string(),
            ));
        }
        let token_provider = self
            .token_provider
            .get_or_try_init(|| async {
                if let Some(path) = &self.config.application_credentials {
                    let provider = gcp_auth::CustomServiceAccount::from_file(path).map_err(
                        |error| {
                            ProviderError::Failure(format!(
                                "failed to load Google Vertex service-account credentials {}: {error}",
                                path.display()
                            ))
                        },
                    )?;
                    Ok::<Arc<dyn TokenProvider>, ProviderError>(Arc::new(provider))
                } else {
                    gcp_auth::provider().await.map_err(|error| {
                        ProviderError::Failure(format!(
                            "failed to resolve Google Vertex Application Default Credentials: {error}"
                        ))
                    })
                }
            })
            .await?;
        let token = tokio::select! {
            _ = signal.wait() => return Err(ProviderError::Aborted),
            token = token_provider.token(&[CLOUD_PLATFORM_SCOPE]) => token.map_err(|error| {
                ProviderError::Failure(format!("failed to refresh Google Vertex access token: {error}"))
            })?,
        };
        Ok(VertexCredentials::Bearer(token.as_str().to_string()))
    }

    fn endpoint(
        &self,
        request: &ProviderRequest,
        credentials: &VertexCredentials,
    ) -> Result<String, ProviderError> {
        let model_base = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref());
        match credentials {
            VertexCredentials::ApiKey(_) => vertex_express_endpoint(
                self.config
                    .base_url
                    .as_deref()
                    .filter(|base| !base.contains("{location}"))
                    .or(model_base.filter(|base| !base.contains("{location}")))
                    .unwrap_or(VERTEX_EXPRESS_BASE_URL),
                request.model.as_str(),
            ),
            VertexCredentials::Bearer(_) => {
                let project = self.config.project.as_deref().ok_or_else(|| {
                    ProviderError::Failure(
                        "Google Vertex ADC requires GOOGLE_CLOUD_PROJECT or GCLOUD_PROJECT"
                            .to_string(),
                    )
                })?;
                let location = self.config.location.as_deref().ok_or_else(|| {
                    ProviderError::Failure(
                        "Google Vertex ADC requires GOOGLE_CLOUD_LOCATION".to_string(),
                    )
                })?;
                let default_base = if location == "global" {
                    VERTEX_EXPRESS_BASE_URL.to_string()
                } else {
                    format!("https://{location}-aiplatform.googleapis.com")
                };
                let base = self
                    .config
                    .base_url
                    .as_deref()
                    .or(model_base)
                    .map(|base| base.replace("{location}", location))
                    .unwrap_or(default_base);
                vertex_project_endpoint(&base, project, location, request.model.as_str())
            }
        }
    }

    fn headers(
        &self,
        request: &ProviderRequest,
        credentials: &VertexCredentials,
    ) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        insert_header(&mut headers, "User-Agent", "pi-rs");
        match credentials {
            VertexCredentials::ApiKey(api_key) => {
                insert_header(&mut headers, "x-goog-api-key", api_key);
            }
            VertexCredentials::Bearer(token) => {
                insert_header(&mut headers, "Authorization", format!("Bearer {token}"));
            }
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }
}

#[async_trait]
impl Provider for GoogleVertexCompatibleProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id.clone()
    }

    fn name(&self) -> String {
        if self.config.provider_id.as_str() == GOOGLE_VERTEX_PROVIDER_ID {
            "Google Vertex AI".to_string()
        } else {
            self.config.provider_id.to_string()
        }
    }

    fn availability(&self) -> ProviderAvailability {
        let adc_ready = self.config.project.is_some()
            && self.config.location.is_some()
            && (self.config.access_token.is_some()
                || adc_file_exists(self.config.application_credentials.as_deref()));
        if self.config.api_key.is_some() || adc_ready {
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
        let credentials = self.credentials(&signal).await?;
        let endpoint = self.endpoint(&request, &credentials)?;
        let headers = self.headers(&request, &credentials);
        let payload = context
            .before_provider_request(&signal, request_body(&request))
            .await?;
        let response = post_json_with_provider_hooks(
            self.transport.as_ref(),
            &context,
            &endpoint,
            headers,
            &payload,
            signal.clone(),
        )
        .await
        .map_err(map_transport_error)?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!(
                "Google Vertex API error ({status}): {body}"
            )));
        }
        if !response
            .content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected text/event-stream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }
        Ok(stream_response(
            self.config.provider_id.clone(),
            request.model,
            GOOGLE_VERTEX_API,
            response.body,
            signal,
        ))
    }
}

enum VertexCredentials {
    ApiKey(String),
    Bearer(String),
}

pub fn google_vertex_models() -> Vec<ModelSpec> {
    vec![
        model("gemini-2.5-flash", "Gemini 2.5 Flash", 0.3, 2.5, 0.03),
        model(
            "gemini-2.5-flash-lite",
            "Gemini 2.5 Flash-Lite",
            0.1,
            0.4,
            0.01,
        ),
        model("gemini-2.5-pro", "Gemini 2.5 Pro", 1.25, 10.0, 0.125),
        model(
            "gemini-3-flash-preview",
            "Gemini 3 Flash Preview",
            0.5,
            3.0,
            0.05,
        ),
        pro_model("gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview"),
        pro_model(
            "gemini-3.1-pro-preview-customtools",
            "Gemini 3.1 Pro Preview Custom Tools",
        ),
        model("gemini-3.5-flash", "Gemini 3.5 Flash", 1.5, 9.0, 0.15),
        model("gemini-flash-latest", "Gemini Flash Latest", 1.5, 9.0, 0.15),
    ]
}

fn model(id: &str, name: &str, input: f64, output: f64, cache_read: f64) -> ModelSpec {
    let mut model = ModelSpec::new(GOOGLE_VERTEX_PROVIDER_ID, id, name, GOOGLE_VERTEX_API);
    model.base_url = Some("https://{location}-aiplatform.googleapis.com".to_string());
    model.reasoning = true;
    model.input = vec![ModelInput::Text, ModelInput::Image];
    model.cost = ModelCost {
        input,
        output,
        cache_read,
        cache_write: 0.0,
        tiers: Vec::new(),
    };
    model.context_window = 1_048_576;
    model.max_tokens = 65_536;
    if id.starts_with("gemini-3") || id.starts_with("gemini-flash") {
        model.thinking_level_map.insert("off".to_string(), None);
    }
    model
}

fn pro_model(id: &str, name: &str) -> ModelSpec {
    let mut model = model(id, name, 2.0, 12.0, 0.2);
    model.thinking_level_map.insert("minimal".to_string(), None);
    model
        .thinking_level_map
        .insert("low".to_string(), Some("LOW".to_string()));
    model.thinking_level_map.insert("medium".to_string(), None);
    model
        .thinking_level_map
        .insert("high".to_string(), Some("HIGH".to_string()));
    model
}

fn vertex_express_endpoint(base: &str, model: &str) -> Result<String, ProviderError> {
    if base.contains(":streamGenerateContent") {
        return validated_stream_endpoint(base);
    }
    let base = versioned_base(base)?;
    Ok(with_alt_sse(format!(
        "{base}/publishers/google/models/{model}:streamGenerateContent"
    )))
}

fn vertex_project_endpoint(
    base: &str,
    project: &str,
    location: &str,
    model: &str,
) -> Result<String, ProviderError> {
    if base.contains(":streamGenerateContent") {
        return validated_stream_endpoint(base);
    }
    let base = versioned_base(base)?;
    Ok(with_alt_sse(format!(
        "{base}/projects/{project}/locations/{location}/publishers/google/models/{model}:streamGenerateContent"
    )))
}

fn versioned_base(base: &str) -> Result<String, ProviderError> {
    let base = base.trim().trim_end_matches('/');
    if !(base.starts_with("https://") || base.starts_with("http://")) || base.contains(['?', '#']) {
        return Err(ProviderError::Failure(format!(
            "invalid Google Vertex base URL: {base}"
        )));
    }
    let last = base.rsplit('/').next().unwrap_or_default();
    if is_api_version(last) {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/v1"))
    }
}

fn is_api_version(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let suffix = &rest[digits..];
    suffix.is_empty()
        || suffix
            .strip_prefix("beta")
            .is_some_and(|tail| tail.chars().all(|character| character.is_ascii_digit()))
}

fn validated_stream_endpoint(base: &str) -> Result<String, ProviderError> {
    let base = base.trim();
    if !(base.starts_with("https://") || base.starts_with("http://")) || base.contains('#') {
        return Err(ProviderError::Failure(format!(
            "invalid Google Vertex stream URL: {base}"
        )));
    }
    Ok(with_alt_sse(base.to_string()))
}

fn with_alt_sse(url: String) -> String {
    if url
        .split_once('?')
        .is_some_and(|(_, query)| query.split('&').any(|item| item == "alt=sse"))
    {
        return url;
    }
    if url.contains('?') {
        format!("{url}&alt=sse")
    } else {
        format!("{url}?alt=sse")
    }
}

fn normalize_api_key(api_key: String) -> Option<String> {
    let api_key = api_key.trim();
    if api_key.is_empty()
        || api_key == "gcp-vertex-credentials"
        || (api_key.starts_with('<') && api_key.ends_with('>'))
    {
        None
    } else {
        Some(api_key.to_string())
    }
}

fn adc_file_exists(explicit: Option<&Path>) -> bool {
    let default = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/gcloud/application_default_credentials.json"));
    explicit
        .map(Path::to_path_buf)
        .into_iter()
        .chain(default)
        .any(|path| path.is_file())
}

fn expand_user_path(value: String) -> PathBuf {
    if value == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(relative) = value.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(relative);
    }
    PathBuf::from(value)
}

fn value(values: &BTreeMap<String, String>, name: &str) -> Option<String> {
    values
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
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
    use std::sync::Mutex as StdMutex;

    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, ModelId, ProviderCallContext, StreamEvent, ThinkingLevel};
    use pi_provider::{HttpResponse, TransportError};
    use serde_json::Value;

    use super::*;

    #[test]
    fn endpoints_cover_express_api_keys_and_project_adc() {
        assert_eq!(
            vertex_express_endpoint(VERTEX_EXPRESS_BASE_URL, "gemini-2.5-flash").unwrap(),
            "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            vertex_express_endpoint("https://proxy.example/v1beta1", "gemini-2.5-flash").unwrap(),
            "https://proxy.example/v1beta1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            vertex_express_endpoint(
                "https://proxy.example/custom:streamGenerateContent?alt=sse",
                "ignored"
            )
            .unwrap(),
            "https://proxy.example/custom:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            vertex_project_endpoint(
                "https://us-central1-aiplatform.googleapis.com",
                "project-1",
                "us-central1",
                "gemini-2.5-flash"
            )
            .unwrap(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/project-1/locations/us-central1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn stored_vertex_environment_configures_adc_without_mutating_process_env() {
        let config = GoogleVertexCompatibleConfig::from_environment_with_overrides(
            Some("gcp-vertex-credentials".to_string()),
            BTreeMap::from([
                (
                    "GOOGLE_CLOUD_PROJECT".to_string(),
                    "stored-project".to_string(),
                ),
                (
                    "GOOGLE_CLOUD_LOCATION".to_string(),
                    "europe-west1".to_string(),
                ),
                (
                    "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
                    "/credentials/service-account.json".to_string(),
                ),
            ]),
        )
        .unwrap();

        assert!(config.api_key.is_none());
        assert_eq!(config.project.as_deref(), Some("stored-project"));
        assert_eq!(config.location.as_deref(), Some("europe-west1"));
        assert_eq!(
            config.application_credentials.as_deref(),
            Some(Path::new("/credentials/service-account.json"))
        );
        assert!(config.allow_adc);
    }

    #[derive(Default)]
    struct Capture {
        url: String,
        headers: BTreeMap<String, String>,
        body: Value,
    }

    struct CapturingTransport(Arc<StdMutex<Capture>>);

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
                    b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n"
                        .to_vec(),
                )])),
            })
        }
    }

    #[tokio::test]
    async fn api_key_mode_uses_express_endpoint_and_google_wire_stream() {
        let capture = Arc::new(StdMutex::new(Capture::default()));
        let provider = GoogleVertexCompatibleProvider::with_transport(
            GoogleVertexCompatibleConfig::with_api_key(VERTEX_EXPRESS_BASE_URL, "vertex-key"),
            Arc::new(CapturingTransport(Arc::clone(&capture))),
        )
        .unwrap();
        let mut models = google_vertex_models();
        let mut spec = models.remove(0);
        spec.base_url = None;
        let request = ProviderRequest {
            model: ModelId::new("gemini-2.5-flash"),
            model_spec: Some(spec),
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::High,
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
                    ProviderId::new(GOOGLE_VERTEX_PROVIDER_ID),
                    ModelId::new("gemini-2.5-flash"),
                ),
                signal,
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            events.first(),
            Some(Ok(StreamEvent::Start { metadata })) if metadata.api == GOOGLE_VERTEX_API
        ));
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::TextDelta { delta, .. }) if delta == "ok")
        ));
        let capture = capture.lock().unwrap();
        assert_eq!(capture.headers["x-goog-api-key"], "vertex-key");
        assert!(
            capture
                .url
                .contains("/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent")
        );
        assert_eq!(
            capture.body["systemInstruction"]["parts"][0]["text"],
            "system"
        );
    }
}
