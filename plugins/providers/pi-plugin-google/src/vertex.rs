use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use gcp_auth::TokenProvider;
use pi_core::{AbortSignal, ModelSpec, PluginId, ProviderId};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderPlugin,
    ProviderRegisterContext, ProviderRequest, ProviderStream,
};
use pi_provider::{
    HttpTransport, ReqwestTransport, TransportError, insert_header, post_json_with_provider_hooks,
    read_error_response,
};
use tokio::sync::OnceCell;

use crate::{request_body, stream_response};

pub const GOOGLE_VERTEX_API: &str = "google-vertex";
const GOOGLE_VERTEX_PROVIDER_ID: &str = "google-vertex";
const VERTEX_EXPRESS_BASE_URL: &str = "https://aiplatform.googleapis.com";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

pub struct GoogleVertexPlugin {
    provider: Arc<dyn Provider>,
}

impl GoogleVertexPlugin {
    /// Rebuild only the request adapter when credentials change. The supplied
    /// source is authoritative; it can therefore be deterministic in embedders
    /// and tests without consulting process-global ADC state.
    pub fn with_credential_source(
        mut self,
        source: Arc<dyn pi_provider::CredentialSource>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        self.provider = Arc::new(pi_provider::CredentialProvider::new(
            self.provider,
            source,
            move |credential| {
                let (api_key, environment) = credential_parts(credential);
                let config = GoogleVertexCompatibleConfig::from_values(api_key, environment)?;
                Ok(Arc::new(GoogleVertexCompatibleProvider::with_transport(
                    config,
                    Arc::clone(&transport),
                )?))
            },
        ));
        self
    }

    /// Coding uses current Application Default Credentials in addition to its
    /// stored credential source. This wrapper only inspects environment values
    /// and credential-file presence; it never loads or refreshes an ADC token.
    pub fn with_ambient_credential_source(
        self,
        source: Arc<dyn pi_provider::CredentialSource>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        self.with_credential_source(
            Arc::new(GoogleVertexCredentialSource { inner: source }),
            transport,
        )
    }

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

#[pi_plugin::provider_plugin]
impl ProviderPlugin for GoogleVertexPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("google-vertex-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
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
        let mut values = ambient_environment();
        values.extend(overrides);
        Self::from_values(api_key, values)
    }

    fn from_values(
        api_key: Option<String>,
        values: BTreeMap<String, String>,
    ) -> Result<Self, ProviderError> {
        let config = Self {
            provider_id: ProviderId::new(GOOGLE_VERTEX_PROVIDER_ID),
            base_url: None,
            api_key: api_key
                .and_then(normalize_api_key)
                .or_else(|| value(&values, "GOOGLE_CLOUD_API_KEY")),
            access_token: value(&values, "GOOGLE_CLOUD_ACCESS_TOKEN")
                .or_else(|| value(&values, "GOOGLE_OAUTH_ACCESS_TOKEN")),
            project: value(&values, "GOOGLE_CLOUD_PROJECT")
                .or_else(|| value(&values, "GCLOUD_PROJECT")),
            location: value(&values, "GOOGLE_CLOUD_LOCATION"),
            application_credentials: value(&values, "GOOGLE_APPLICATION_CREDENTIALS")
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
            .before_provider_request(&signal, request_body(&request)?)
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
            let message = read_error_response(response)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(message));
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
    serde_json::from_str(include_str!("data/google-vertex.json"))
        .expect("checked-in Vertex catalog")
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

struct GoogleVertexCredentialSource {
    inner: Arc<dyn pi_provider::CredentialSource>,
}

#[async_trait]
impl pi_provider::CredentialSource for GoogleVertexCredentialSource {
    async fn check(
        &self,
        signal: &AbortSignal,
    ) -> Result<Option<pi_provider::ProviderCredential>, ProviderError> {
        let credential = self.inner.check(signal).await?;
        if signal.is_aborted() {
            return Err(ProviderError::Aborted);
        }
        Ok(with_ambient_vertex_credential(credential))
    }

    async fn resolve(
        &self,
        signal: &AbortSignal,
    ) -> Result<Option<pi_provider::ProviderCredential>, ProviderError> {
        let credential = self.inner.resolve(signal).await?;
        if signal.is_aborted() {
            return Err(ProviderError::Aborted);
        }
        Ok(with_ambient_vertex_credential(credential))
    }
}

fn with_ambient_vertex_credential(
    credential: Option<pi_provider::ProviderCredential>,
) -> Option<pi_provider::ProviderCredential> {
    let ambient = ambient_environment();
    if let Some(mut credential) = credential {
        let mut environment = ambient;
        environment.extend(credential_environment(Some(&credential)));
        credential
            .metadata
            .insert("env".to_string(), serde_json::json!(environment));
        return Some(credential);
    }
    let config = GoogleVertexCompatibleConfig::from_values(None, ambient.clone()).ok()?;
    let ready = config.api_key.is_some()
        || (config.project.is_some()
            && config.location.is_some()
            && (config.access_token.is_some()
                || adc_file_exists(config.application_credentials.as_deref())));
    ready.then(|| pi_provider::ProviderCredential {
        secret: config.api_key.unwrap_or_default(),
        kind: pi_provider::CredentialKind::ApiKey,
        metadata: BTreeMap::from([("env".to_string(), serde_json::json!(ambient))]),
    })
}

fn credential_parts(
    credential: Option<&pi_provider::ProviderCredential>,
) -> (Option<String>, BTreeMap<String, String>) {
    let api_key = credential
        .map(|credential| credential.secret.trim())
        .filter(|secret| !secret.is_empty())
        .map(str::to_owned);
    (api_key, credential_environment(credential))
}

fn credential_environment(
    credential: Option<&pi_provider::ProviderCredential>,
) -> BTreeMap<String, String> {
    credential
        .and_then(|credential| credential.metadata.get("env"))
        .and_then(|environment| serde_json::from_value(environment.clone()).ok())
        .unwrap_or_default()
}

fn ambient_environment() -> BTreeMap<String, String> {
    [
        "GOOGLE_CLOUD_API_KEY",
        "GOOGLE_CLOUD_ACCESS_TOKEN",
        "GOOGLE_OAUTH_ACCESS_TOKEN",
        "GOOGLE_CLOUD_PROJECT",
        "GCLOUD_PROJECT",
        "GOOGLE_CLOUD_LOCATION",
        "GOOGLE_APPLICATION_CREDENTIALS",
    ]
    .into_iter()
    .filter_map(|name| env(name).map(|value| (name.to_string(), value)))
    .collect()
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

fn map_transport_error(error: TransportError) -> ProviderError {
    error.into_provider_error()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, AnyModelSpec, ModelId, StreamEvent, ThinkingLevel};
    use pi_plugin::ProviderCallContext;
    use pi_provider::{
        CredentialKind, CredentialSource, HttpResponse, ProviderCredential, TransportError,
    };
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
    struct RotatingSource {
        value: StdMutex<Option<ProviderCredential>>,
        checks: AtomicUsize,
        resolves: AtomicUsize,
    }

    #[async_trait]
    impl CredentialSource for RotatingSource {
        async fn check(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<ProviderCredential>, ProviderError> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            Ok(self.value.lock().unwrap().clone())
        }

        async fn resolve(
            &self,
            _: &AbortSignal,
        ) -> Result<Option<ProviderCredential>, ProviderError> {
            self.resolves.fetch_add(1, Ordering::SeqCst);
            Ok(self.value.lock().unwrap().clone())
        }
    }

    #[derive(Default)]
    struct NoRequestTransport(AtomicUsize);

    #[async_trait]
    impl HttpTransport for NoRequestTransport {
        async fn post_json(
            &self,
            _: &str,
            _: &BTreeMap<String, String>,
            _: &Value,
            _: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(TransportError::Request("unexpected request".into()))
        }
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
    async fn availability_rechecks_vertex_adc_without_requests_or_token_refresh() {
        let credentials_path = std::env::temp_dir().join(format!(
            "pi-rs-vertex-availability-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&credentials_path, "{}").unwrap();
        let source = Arc::new(RotatingSource::default());
        let transport = Arc::new(NoRequestTransport::default());
        let plugin = GoogleVertexPlugin::new_with_transport(None, transport.clone())
            .unwrap()
            .with_credential_source(source.clone(), transport.clone());
        let models = vec![AnyModelSpec::Chat(google_vertex_models().remove(0))];
        let present = ProviderCredential {
            secret: String::new(),
            kind: CredentialKind::ApiKey,
            metadata: BTreeMap::from([(
                "env".into(),
                serde_json::json!({
                    "GOOGLE_CLOUD_PROJECT": "project",
                    "GOOGLE_CLOUD_LOCATION": "us-central1",
                    "GOOGLE_APPLICATION_CREDENTIALS": credentials_path,
                }),
            )]),
        };

        for (credential, expected) in [(None, 0), (Some(present), 1), (None, 0)] {
            *source.value.lock().unwrap() = credential;
            let (_, signal) = AbortHandle::new();
            assert_eq!(
                plugin
                    .provider
                    .filter_available_models(models.clone(), signal)
                    .await
                    .unwrap()
                    .len(),
                expected
            );
        }

        assert_eq!(source.checks.load(Ordering::SeqCst), 3);
        assert_eq!(source.resolves.load(Ordering::SeqCst), 0);
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
        std::fs::remove_file(credentials_path).unwrap();
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
            cache_retention: None,
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
