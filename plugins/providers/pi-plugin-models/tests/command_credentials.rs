//! Exercise command-backed credentials through the public models.json/provider seam.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, AbortSignal, ModelId, ProviderId, ThinkingLevel};
use pi_plugin::{
    Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderRequest,
    RegistriesBuilder,
};
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use pi_provider::{HttpResponse, HttpTransport, TransportError};
use serde_json::{Value, json};

#[derive(Default)]
struct Capture {
    requests: Mutex<Vec<(String, BTreeMap<String, String>)>>,
}

#[async_trait]
impl HttpTransport for Capture {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        _: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        self.requests
            .lock()
            .unwrap()
            .push((url.to_owned(), headers.clone()));
        Ok(HttpResponse {
            status: 200,
            content_type: Some("text/event-stream".into()),
            headers: Vec::new(),
            body: Box::pin(futures::stream::empty()),
        })
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    plugin: ModelsPlugin,
    provider: Arc<dyn Provider>,
    capture: Arc<Capture>,
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

impl Fixture {
    fn new(api: &str, runtime_key: Option<&str>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let helper = root.join("credential helper.sh");
        std::fs::write(&helper, "printf 'called\\n' >> \"$2\"\ncat \"$1\"\n").unwrap();
        let command = |file: &str| {
            format!(
                "!sh {} {} {}",
                quote(&helper),
                quote(&root.join(file)),
                quote(&root.join("calls")),
            )
        };
        let path = root.join("models.json");
        std::fs::write(
            &path,
            json!({"providers": {"helper-gateway": {
                "api": api,
                "baseUrl": "https://gateway.example/v1",
                "apiKey": command("key"),
                "headers": {"X-Provider-Credential": command("provider-header")},
                "models": [{
                    "id": "test-model",
                    "headers": {"X-Model-Credential": command("model-header")}
                }]
            }}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(root.join("provider-header"), "provider-initial\n").unwrap();
        std::fs::write(root.join("model-header"), "model-initial\n").unwrap();
        // The key does not exist yet. Loading and availability checks must not run the helper.
        let mut options = ModelsPluginOptions::new(path);
        if let Some(key) = runtime_key {
            options = options.runtime_api_key("helper-gateway", key);
        }
        let capture = Arc::new(Capture::default());
        let plugin = ModelsPlugin::load_with_transport(options, capture.clone()).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(Vec::new(), vec![Arc::new(plugin.clone())])
            .unwrap();
        let provider = registries
            .provider(&ProviderId::new("helper-gateway"))
            .unwrap();
        assert_eq!(provider.availability(), ProviderAvailability::Available);
        assert!(!root.join("calls").exists());
        Self {
            directory,
            plugin,
            provider,
            capture,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.path(name), value).unwrap();
    }

    async fn request(&self) -> Result<(), ProviderError> {
        self.request_in_session("same-session").await
    }

    async fn request_in_session(&self, session_id: &str) -> Result<(), ProviderError> {
        let (_, signal) = AbortHandle::new();
        self.provider
            .stream(
                ProviderRequest {
                    model: ModelId::new("test-model"),
                    model_spec: None,
                    system_prompt: String::new(),
                    messages: Vec::new(),
                    tools: Vec::new(),
                    thinking_level: ThinkingLevel::Off,
                    thinking_budgets: None,
                    max_output_tokens: None,
                    headers: BTreeMap::new(),
                    sampling_params: BTreeMap::new(),
                    session_id: Some(session_id.into()),
                },
                ProviderCallContext::without_plugins(
                    self.directory.path(),
                    "helper-gateway".into(),
                    "test-model".into(),
                ),
                signal,
            )
            .await
            .map(|_| ())
    }
}

#[tokio::test]
async fn command_credentials_are_lazy_and_rotate_without_reload() {
    for (api, suffix) in [
        ("openai-responses", "responses"),
        ("openai-completions", "chat/completions"),
    ] {
        let fixture = Fixture::new(api, None);
        for version in ["first", "rotated"] {
            fixture.set("key", &format!("  {version}-key\n"));
            fixture.set("provider-header", &format!("{version}-provider\n"));
            fixture.set("model-header", &format!("{version}-model\n"));
            fixture.request().await.unwrap();
            let requests = fixture.capture.requests.lock().unwrap();
            let (url, headers) = requests.last().unwrap();
            assert_eq!(url, &format!("https://gateway.example/v1/{suffix}"));
            assert_eq!(headers["Authorization"], format!("Bearer {version}-key"));
            assert_eq!(
                headers["X-Provider-Credential"],
                format!("{version}-provider")
            );
            assert_eq!(headers["X-Model-Credential"], format!("{version}-model"));
        }
        assert_eq!(fixture.capture.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn command_failure_recovers_and_does_not_reuse_a_stale_credential() {
    let fixture = Fixture::new("openai-responses", None);
    assert!(fixture.request().await.is_err());
    assert!(fixture.capture.requests.lock().unwrap().is_empty());

    fixture.set("key", "first-key");
    fixture.request().await.unwrap();
    std::fs::remove_file(fixture.path("key")).unwrap();
    assert!(fixture.request().await.is_err());
    assert_eq!(fixture.capture.requests.lock().unwrap().len(), 1);

    fixture.set("key", "recovered-key");
    fixture.request().await.unwrap();
    let requests = fixture.capture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].1["Authorization"], "Bearer recovered-key");
}

#[tokio::test]
async fn catalog_credentials_do_not_freeze_later_inference_credentials() {
    let fixture = Fixture::new("openai-responses", None);
    fixture.set("key", "catalog-key");
    let (_, signal) = AbortHandle::new();
    let route = fixture
        .plugin
        .catalog_request_config(&"helper-gateway".into(), &signal)
        .await
        .unwrap();
    assert_eq!(route.api_key.as_deref(), Some("catalog-key"));
    assert_eq!(route.headers["X-Provider-Credential"], "provider-initial");

    fixture.set("key", "inference-key");
    fixture.set("provider-header", "provider-rotated");
    fixture.request().await.unwrap();
    let requests = fixture.capture.requests.lock().unwrap();
    assert_eq!(requests[0].1["Authorization"], "Bearer inference-key");
    assert_eq!(requests[0].1["X-Provider-Credential"], "provider-rotated");
}

#[tokio::test]
async fn responses_gateway_gets_session_thread_identity_while_credentials_rotate() {
    let fixture = Fixture::new("openai-responses", None);
    for (session, key) in [
        ("first-session", "first-key"),
        ("first-session", "rotated-key"),
        ("second-session", "rotated-key"),
    ] {
        fixture.set("key", key);
        fixture.request_in_session(session).await.unwrap();
        let requests = fixture.capture.requests.lock().unwrap();
        let (_, headers) = requests.last().unwrap();
        assert_eq!(headers.get("thread-id").map(String::as_str), Some(session));
        assert_eq!(headers["Authorization"], format!("Bearer {key}"));
    }
}

#[tokio::test]
async fn explicit_runtime_key_bypasses_the_key_command() {
    let fixture = Fixture::new("openai-responses", Some("explicit-key"));
    // The missing key file would fail the request if the key command ran.
    fixture.request().await.unwrap();
    let requests = fixture.capture.requests.lock().unwrap();
    assert_eq!(requests[0].1["Authorization"], "Bearer explicit-key");
    assert_eq!(
        std::fs::read_to_string(fixture.path("calls"))
            .unwrap()
            .lines()
            .count(),
        2, // Only the provider and model header commands.
    );
}
