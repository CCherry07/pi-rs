use std::sync::Arc;

use pi_plugin_anthropic::AnthropicPlugin;
use pi_plugin_azure_openai::AzureOpenAiPlugin;
use pi_plugin_bedrock::AmazonBedrockPlugin;
use pi_plugin_copilot::{GitHubCopilotPlugin, GitHubCopilotStoredMetadata};
use pi_plugin_google::{GooglePlugin, GoogleVertexPlugin};
use pi_plugin_mistral::MistralPlugin;
use pi_plugin_openai::{
    CodexCredentials, CodexTransportOptions, OpenAiCodexPlugin, OpenAiCompatibleConfig,
    OpenAiCompatiblePlugin,
};
use pi_plugin_openrouter::OpenRouterPlugin;
use pi_plugin_xai::XAiPlugin;
use pi_provider::{HttpTransport, ReqwestTransport};
use pi_runtime::{PiRuntimeBuilder, RuntimeError};

use crate::Config;
use crate::configuration::{codex_transport_options, provider_transport_config};
use crate::credentials::{StoredCredential, read_stored_credential};

pub(crate) struct BuiltinProviderSet {
    transport: Arc<dyn HttpTransport>,
    codex_transport_options: CodexTransportOptions,
    effective_api_key: Option<String>,
    provider_config: OpenAiCompatibleConfig,
    stored_anthropic: Option<StoredCredential>,
    stored_google: Option<StoredCredential>,
    stored_google_vertex: Option<StoredCredential>,
    stored_xai: Option<StoredCredential>,
    stored_mistral: Option<StoredCredential>,
    stored_azure: Option<StoredCredential>,
    stored_openrouter: Option<StoredCredential>,
    stored_copilot: Option<(String, GitHubCopilotStoredMetadata)>,
    stored_bedrock: Option<StoredCredential>,
    codex_credentials: CodexCredentials,
}

impl BuiltinProviderSet {
    pub(crate) fn prepare(config: &Config) -> Result<Self, RuntimeError> {
        Self::load(config, None, configured_transport(config)?)
    }

    #[cfg(test)]
    pub(crate) fn prepare_for_test(
        config: &Config,
        codex_override: Option<CodexCredentials>,
        transport: Option<Arc<dyn HttpTransport>>,
    ) -> Result<Self, RuntimeError> {
        let transport = match transport {
            Some(transport) => transport,
            None => configured_transport(config)?,
        };
        Self::load(config, codex_override, transport)
    }

    fn load(
        config: &Config,
        codex_override: Option<CodexCredentials>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, RuntimeError> {
        let codex_transport_options = codex_transport_options(&config.runtime_settings);
        let load = |provider| {
            read_stored_credential(&config.agent_dir, provider).map_err(RuntimeError::Build)
        };
        let stored_anthropic = load("anthropic")?;
        let stored_google = load("google")?;
        let stored_google_vertex = load("google-vertex")?;
        let stored_xai = load("xai")?;
        let stored_mistral = load("mistral")?;
        let stored_azure = load("azure-openai-responses")?;
        let stored_openrouter = load("openrouter")?;
        let stored_copilot = load("github-copilot")?.and_then(|credential| {
            let token = credential.secret()?.to_owned();
            let (StoredCredential::ApiKey { extra, .. } | StoredCredential::Oauth { extra, .. }) =
                &credential;
            Some((
                token,
                GitHubCopilotStoredMetadata::from_extensions(credential.is_oauth(), extra),
            ))
        });
        let stored_bedrock = load("amazon-bedrock")?;
        let stored_codex = load("openai-codex")?;
        let selected = load(&config.provider)?;
        let effective_api_key = config.api_key.clone().or_else(|| {
            selected
                .as_ref()
                .and_then(StoredCredential::secret)
                .map(str::to_string)
        });
        let provider_config = effective_api_key
            .as_ref()
            .map_or_else(
                || OpenAiCompatibleConfig::without_api_key(&config.base_url),
                |api_key| OpenAiCompatibleConfig::new(&config.base_url, api_key),
            )
            .provider_id(config.provider.clone());
        let codex_credentials = codex_override.unwrap_or_else(|| {
            stored_codex
                .as_ref()
                .and_then(StoredCredential::secret)
                .map(CodexCredentials::from_access_token)
                .unwrap_or_else(CodexCredentials::discover)
        });
        Ok(Self {
            transport,
            codex_transport_options,
            effective_api_key,
            provider_config,
            stored_anthropic,
            stored_google,
            stored_google_vertex,
            stored_xai,
            stored_mistral,
            stored_azure,
            stored_openrouter,
            stored_copilot,
            stored_bedrock,
            codex_credentials,
        })
    }

    pub(crate) fn transport(&self) -> Arc<dyn HttpTransport> {
        Arc::clone(&self.transport)
    }

    pub(crate) fn effective_api_key(&self) -> Option<&str> {
        self.effective_api_key.as_deref()
    }

    pub(crate) fn register(self, builder: PiRuntimeBuilder, config: &Config) -> PiRuntimeBuilder {
        let Self {
            transport,
            codex_transport_options,
            provider_config,
            stored_anthropic,
            stored_google,
            stored_google_vertex,
            stored_xai,
            stored_mistral,
            stored_azure,
            stored_openrouter,
            stored_copilot,
            stored_bedrock,
            codex_credentials,
            ..
        } = self;
        let builder = if config.provider == "openai-codex" {
            let credentials = codex_credentials.clone();
            let transport = Arc::clone(&transport);
            let transport_options = codex_transport_options.clone();
            builder.provider_plugin_factory(move || {
                OpenAiCodexPlugin::with_transport_options(
                    credentials.clone(),
                    Arc::clone(&transport),
                    transport_options.clone(),
                )
            })
        } else if config.provider == "xai" {
            let api_key = config.api_key.clone();
            let selected_xai = stored_xai.clone();
            let transport = Arc::clone(&transport);
            builder.provider_plugin_factory(move || match &api_key {
                Some(api_key) => {
                    XAiPlugin::new_with_transport(Some(api_key.clone()), Arc::clone(&transport))
                }
                None => XAiPlugin::from_stored_with_transport(
                    selected_xai
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_string),
                    Arc::clone(&transport),
                ),
            })
        } else if matches!(
            config.provider.as_str(),
            "amazon-bedrock"
                | "anthropic"
                | "google"
                | "google-vertex"
                | "github-copilot"
                | "mistral"
                | "azure-openai-responses"
                | "openrouter"
        ) {
            builder
        } else {
            builder.try_provider_plugin_factory({
                let provider_config = provider_config.clone();
                let transport = Arc::clone(&transport);
                move || {
                    OpenAiCompatiblePlugin::with_transport(
                        provider_config.clone(),
                        Arc::clone(&transport),
                    )
                }
            })
        };
        let explicit_api_key = selected_api_key(config, "anthropic");
        let builder = builder.provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_api_key {
                Some(api_key) => AnthropicPlugin::with_api_key_and_transport(
                    api_key.clone(),
                    Arc::clone(&transport),
                ),
                None => AnthropicPlugin::from_stored_with_transport(
                    stored_anthropic.as_ref().and_then(|credential| {
                        credential
                            .secret()
                            .map(|secret| (secret, credential.is_oauth()))
                    }),
                    Arc::clone(&transport),
                ),
            }
        });
        let builder = if config.provider == "openai-codex" {
            builder
        } else {
            let transport = Arc::clone(&transport);
            let transport_options = codex_transport_options;
            builder.provider_plugin_factory(move || {
                OpenAiCodexPlugin::with_transport_options(
                    codex_credentials.clone(),
                    Arc::clone(&transport),
                    transport_options.clone(),
                )
            })
        };
        let builder = if config.provider == "xai" {
            builder
        } else {
            let stored_xai = stored_xai.clone();
            let transport = Arc::clone(&transport);
            builder.provider_plugin_factory(move || {
                XAiPlugin::from_stored_with_transport(
                    stored_xai
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_string),
                    Arc::clone(&transport),
                )
            })
        };
        let explicit_api_key = selected_api_key(config, "google");
        let builder = builder.try_provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_api_key {
                Some(api_key) => {
                    GooglePlugin::new_with_transport(Some(api_key.clone()), Arc::clone(&transport))
                }
                None => GooglePlugin::from_stored_with_transport(
                    stored_google
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                ),
            }
        });
        let explicit_api_key = selected_api_key(config, "google-vertex");
        let builder = builder.try_provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_api_key {
                Some(api_key) => GoogleVertexPlugin::new_with_transport(
                    Some(api_key.clone()),
                    Arc::clone(&transport),
                ),
                None => GoogleVertexPlugin::from_stored_with_environment_and_transport(
                    stored_google_vertex
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    stored_google_vertex
                        .as_ref()
                        .and_then(StoredCredential::environment)
                        .cloned()
                        .unwrap_or_default(),
                    Arc::clone(&transport),
                ),
            }
        });
        let explicit_api_key = selected_api_key(config, "mistral");
        let builder = builder.try_provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_api_key {
                Some(api_key) => {
                    MistralPlugin::new_with_transport(Some(api_key.clone()), Arc::clone(&transport))
                }
                None => MistralPlugin::from_stored_with_transport(
                    stored_mistral
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                ),
            }
        });
        let explicit_api_key = selected_api_key(config, "azure-openai-responses");
        let builder = builder.try_provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_api_key {
                Some(api_key) => AzureOpenAiPlugin::new_with_transport(
                    Some(api_key.clone()),
                    Arc::clone(&transport),
                ),
                None => AzureOpenAiPlugin::from_stored_with_transport(
                    stored_azure
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                ),
            }
        });
        let explicit_api_key = selected_api_key(config, "openrouter");
        let builder = builder.try_provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_api_key {
                Some(api_key) => OpenRouterPlugin::new_with_transport(
                    Some(api_key.clone()),
                    Arc::clone(&transport),
                ),
                None => OpenRouterPlugin::from_stored_with_transport(
                    stored_openrouter
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                ),
            }
        });
        let explicit_token = selected_api_key(config, "github-copilot");
        let builder = builder.try_provider_plugin_factory({
            let transport = Arc::clone(&transport);
            move || match &explicit_token {
                Some(token) => GitHubCopilotPlugin::new_with_transport(
                    Some(token.clone()),
                    None,
                    Arc::clone(&transport),
                ),
                None => GitHubCopilotPlugin::from_stored_catalog_with_transport(
                    stored_copilot
                        .as_ref()
                        .map(|(token, metadata)| metadata.credential(token)),
                    Arc::clone(&transport),
                ),
            }
        });
        let explicit_token = selected_api_key(config, "amazon-bedrock");
        builder.try_provider_plugin_factory(move || {
            let token = explicit_token.clone().or_else(|| {
                stored_bedrock
                    .as_ref()
                    .and_then(StoredCredential::secret)
                    .map(str::to_owned)
            });
            let environment = stored_bedrock
                .as_ref()
                .and_then(StoredCredential::environment)
                .cloned()
                .unwrap_or_default();
            AmazonBedrockPlugin::from_stored_with_transport(
                token,
                environment,
                Arc::clone(&transport),
            )
        })
    }
}

fn configured_transport(config: &Config) -> Result<Arc<dyn HttpTransport>, RuntimeError> {
    let transport =
        ReqwestTransport::with_config(provider_transport_config(&config.runtime_settings))
            .map_err(|error| RuntimeError::Build(error.to_string()))?;
    Ok(Arc::new(transport))
}

fn selected_api_key(config: &Config, provider: &str) -> Option<String> {
    (config.provider == provider)
        .then(|| config.api_key.clone())
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use pi_core::{AbortHandle, ModelSelection, ProviderId, ThinkingLevel};
    use pi_provider::{HttpResponse, TransportError};
    use pi_runtime::{PiRuntime, RuntimeCompletionRequest};
    use serde_json::{Value, json};

    use super::*;

    #[derive(Default)]
    struct Capture(Mutex<Option<(String, BTreeMap<String, String>)>>);

    #[async_trait]
    impl HttpTransport for Capture {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            _body: &Value,
            _signal: pi_core::AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self.0.lock().unwrap() = Some((url.to_string(), headers.clone()));
            Err(TransportError::Request("captured without network".into()))
        }
    }

    #[test]
    fn invalid_transport_precedes_malformed_credentials_during_preparation() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("auth.json"), "not json").unwrap();
        let mut config = crate::test_support::app_config(directory.path(), None);
        config.runtime_settings.http_proxy = Some("http://[".into());
        for result in [
            BuiltinProviderSet::prepare(&config),
            BuiltinProviderSet::prepare_for_test(&config, Some(CodexCredentials::default()), None),
        ] {
            let error = match result {
                Ok(_) => panic!("invalid proxy must reject preparation"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains("invalid HTTP proxy"), "{error}");
            assert!(!error.contains("auth.json"), "{error}");
        }
        config.runtime_settings.http_proxy = None;
        let error = match BuiltinProviderSet::prepare(&config) {
            Ok(_) => panic!("malformed credentials must reject preparation"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("auth.json"), "{error}");
    }

    #[tokio::test]
    async fn builtin_provider_factories_preserve_order_credentials_and_reload() {
        const CHILD: &str = "PI_PROVIDER_FACTORY_TEST";
        let Ok(mode) = std::env::var(CHILD) else {
            // Environment precedence is process-global; isolate it without unsafe
            // mutation or a lock that unrelated provider tests do not acquire.
            let temporary = tempfile::tempdir().unwrap();
            for mode in ["stored", "environment"] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .arg("--exact")
                    .arg("builtin_providers::tests::builtin_provider_factories_preserve_order_credentials_and_reload")
                    .arg("--nocapture")
                    .env_clear()
                    .env("TMPDIR", temporary.path())
                    .env("TMP", temporary.path())
                    .env("TEMP", temporary.path())
                    .env(CHILD, mode)
                    .env("AZURE_OPENAI_BASE_URL", "https://azure.example/openai/v1")
                    .env("AWS_REGION", "us-east-1")
                    .env("AWS_EC2_METADATA_DISABLED", "true");
                // Windows still needs its OS location, not the caller's provider environment.
                if let Some(system_root) = std::env::var_os("SystemRoot") {
                    command.env("SystemRoot", system_root);
                }
                if mode == "environment" {
                    for name in [
                        "ANTHROPIC_API_KEY",
                        "GEMINI_API_KEY",
                        "GOOGLE_CLOUD_API_KEY",
                        "XAI_API_KEY",
                        "MISTRAL_API_KEY",
                        "AZURE_OPENAI_API_KEY",
                        "OPENROUTER_API_KEY",
                        "COPILOT_GITHUB_TOKEN",
                        "AWS_BEARER_TOKEN_BEDROCK",
                    ] {
                        command.env(name, "environment-key");
                    }
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let providers = [
            ("anthropic", "x-api-key"),
            ("google", "x-goog-api-key"),
            ("google-vertex", "x-goog-api-key"),
            ("xai", "Authorization"),
            ("mistral", "Authorization"),
            ("azure-openai-responses", "api-key"),
            ("openrouter", "Authorization"),
            ("github-copilot", "Authorization"),
            ("amazon-bedrock", "Authorization"),
        ];
        let mut credentials = providers
            .iter()
            .map(|(id, _)| (*id, json!({"type":"api_key", "key":"stored-key"})))
            .collect::<BTreeMap<_, _>>();
        credentials.insert(
            "github-copilot",
            json!({
                "type":"oauth", "access":"stored-key", "refresh":"refresh", "expires":0,
                "enterpriseUrl":"github.example.com", "availableModelIds":["gpt-4.1"],
            }),
        );
        std::fs::write(
            directory.path().join("auth.json"),
            serde_json::to_vec(&credentials).unwrap(),
        )
        .unwrap();
        for selected in providers
            .iter()
            .map(|(id, _)| *id)
            .chain(["openai-compatible", "openai-codex"])
        {
            for explicit in [None, Some("explicit-key".to_string())] {
                let mut config = Config::new(directory.path().into(), directory.path().into());
                config.provider = selected.into();
                config.api_key = explicit.clone();
                let capture = Arc::new(Capture::default());
                let transport: Arc<dyn HttpTransport> = capture.clone();
                let prepared = BuiltinProviderSet::prepare_for_test(
                    &config,
                    Some(CodexCredentials::default()),
                    Some(transport.clone()),
                )
                .unwrap();
                assert!(Arc::ptr_eq(&prepared.transport(), &transport));
                let runtime = prepared
                    .register(
                        PiRuntime::builder().agent_options(pi_agent::AgentOptions {
                            provider_id: ProviderId::new("anthropic"),
                            model_id: pi_core::ModelId::new("claude-sonnet-4-5"),
                            ..pi_agent::AgentOptions::default()
                        }),
                        &config,
                    )
                    .build()
                    .unwrap();
                let mut expected = vec![
                    "anthropic-provider",
                    "openai-codex-provider",
                    "xai-provider",
                    "google-provider",
                    "google-vertex-provider",
                    "mistral-provider",
                    "azure-openai-provider",
                    "openrouter-provider",
                    "github-copilot-provider",
                    "amazon-bedrock-provider",
                ];
                match selected {
                    "openai-compatible" => expected.insert(0, "openai-compatible-provider"),
                    "openai-codex" | "xai" => {
                        let id = if selected == "xai" {
                            "xai-provider"
                        } else {
                            "openai-codex-provider"
                        };
                        expected.retain(|existing| *existing != id);
                        expected.insert(0, id);
                    }
                    _ => {}
                }
                for reload in [false, true] {
                    if reload {
                        runtime.reload().await.unwrap();
                    }
                    assert_eq!(
                        runtime
                            .provider_plugin_order()
                            .iter()
                            .map(|id| id.as_str())
                            .collect::<Vec<_>>(),
                        expected
                    );
                    for (provider, header) in providers {
                        assert!(runtime.provider_is_available(&ProviderId::new(provider)));
                        let model = runtime
                            .models()
                            .into_iter()
                            .find(|model| model.provider.as_str() == provider)
                            .unwrap();
                        let request = RuntimeCompletionRequest {
                            system_prompt: "test".into(),
                            messages: Vec::new(),
                            model: Some(ModelSelection {
                                provider: model.provider,
                                model_id: model.id,
                            }),
                            thinking_level: ThinkingLevel::Off,
                            thinking_budgets: None,
                            max_output_tokens: Some(16),
                        };
                        *capture.0.lock().unwrap() = None;
                        let (_, signal) = AbortHandle::new();
                        assert!(runtime.complete(request, signal).await.is_err());
                        let (url, headers) =
                            capture.0.lock().unwrap().take().unwrap_or_else(|| {
                                panic!("{provider} did not reach the transport")
                            });
                        let overridden = provider == selected && explicit.is_some();
                        let key = if overridden {
                            "explicit-key"
                        } else if mode == "environment"
                            && !matches!(provider, "google-vertex" | "amazon-bedrock")
                        {
                            "environment-key"
                        } else {
                            "stored-key"
                        };
                        let expected_header = if header == "Authorization" {
                            format!("Bearer {key}")
                        } else {
                            key.into()
                        };
                        assert_eq!(
                            headers.get(header),
                            Some(&expected_header),
                            "{provider}, selected {selected}, reload {reload}"
                        );
                        if provider == "github-copilot" {
                            let models = runtime
                                .models()
                                .into_iter()
                                .filter(|model| model.provider.as_str() == provider)
                                .count();
                            assert_eq!(models == 1, mode == "stored" && !overridden);
                            assert_eq!(
                                url.starts_with("https://copilot-api.github.example.com/"),
                                !overridden
                            );
                        }
                    }
                }
            }
        }
    }
}
