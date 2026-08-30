use std::sync::Arc;

use pi_plugin_anthropic::AnthropicPlugin;
use pi_plugin_azure_openai::AzureOpenAiPlugin;
use pi_plugin_bedrock::AmazonBedrockPlugin;
use pi_plugin_copilot::{GitHubCopilotPlugin, GitHubCopilotStoredCredential};
use pi_plugin_google::{GooglePlugin, GoogleVertexPlugin};
use pi_plugin_mistral::MistralPlugin;
use pi_plugin_openai::{
    CodexCredentials, CodexTransportOptions, OpenAiCodexPlugin, OpenAiCompatibleConfig,
    OpenAiCompatiblePlugin,
};
use pi_plugin_openrouter::OpenRouterPlugin;
use pi_plugin_xai::XAiPlugin;
use pi_provider::HttpTransport;
use pi_runtime::{PiRuntimeBuilder, RuntimeError};

use crate::auth::{StoredCredential, read_stored_credential};
use crate::config::AppConfig;

pub(crate) struct BuiltinProviderSet {
    effective_api_key: Option<String>,
    provider_config: OpenAiCompatibleConfig,
    stored_anthropic: Option<StoredCredential>,
    stored_google: Option<StoredCredential>,
    stored_google_vertex: Option<StoredCredential>,
    stored_xai: Option<StoredCredential>,
    stored_mistral: Option<StoredCredential>,
    stored_azure: Option<StoredCredential>,
    stored_openrouter: Option<StoredCredential>,
    stored_copilot: Option<StoredCredential>,
    stored_bedrock: Option<StoredCredential>,
    codex_credentials: CodexCredentials,
}

impl BuiltinProviderSet {
    pub(crate) fn load(
        config: &AppConfig,
        codex_override: Option<CodexCredentials>,
    ) -> Result<Self, RuntimeError> {
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
        let stored_copilot = load("github-copilot")?;
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

    pub(crate) fn effective_api_key(&self) -> Option<&str> {
        self.effective_api_key.as_deref()
    }

    pub(crate) fn register(
        self,
        builder: PiRuntimeBuilder,
        config: &AppConfig,
        transport: Arc<dyn HttpTransport>,
        codex_transport_options: CodexTransportOptions,
    ) -> PiRuntimeBuilder {
        let Self {
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
        let builder = if config.provider == "anthropic" {
            let api_key = config.api_key.clone();
            let transport = Arc::clone(&transport);
            builder.provider_plugin_factory(move || match &api_key {
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
            })
        } else {
            let stored_anthropic = stored_anthropic.clone();
            let transport = Arc::clone(&transport);
            builder.provider_plugin_factory(move || {
                AnthropicPlugin::from_stored_with_transport(
                    stored_anthropic.as_ref().and_then(|credential| {
                        credential
                            .secret()
                            .map(|secret| (secret, credential.is_oauth()))
                    }),
                    Arc::clone(&transport),
                )
            })
        };
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
        let builder = if config.provider == "google" {
            let explicit_api_key = config.api_key.clone();
            let stored_google = stored_google.clone();
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || match &explicit_api_key {
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
            })
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                GooglePlugin::from_stored_with_transport(
                    stored_google
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                )
            })
        };
        let builder = if config.provider == "google-vertex" {
            let explicit_api_key = config.api_key.clone();
            let stored_google_vertex = stored_google_vertex.clone();
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || match &explicit_api_key {
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
            })
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                GoogleVertexPlugin::from_stored_with_environment_and_transport(
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
                )
            })
        };
        let builder = if config.provider == "mistral" {
            let explicit_api_key = config.api_key.clone();
            let stored_mistral = stored_mistral.clone();
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || match &explicit_api_key {
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
            })
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                MistralPlugin::from_stored_with_transport(
                    stored_mistral
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                )
            })
        };
        let builder = if config.provider == "azure-openai-responses" {
            let explicit_api_key = config.api_key.clone();
            let stored_azure = stored_azure.clone();
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || match &explicit_api_key {
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
            })
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                AzureOpenAiPlugin::from_stored_with_transport(
                    stored_azure
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                )
            })
        };
        let builder = if config.provider == "openrouter" {
            let explicit_api_key = config.api_key.clone();
            let stored_openrouter = stored_openrouter.clone();
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || match &explicit_api_key {
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
            })
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                OpenRouterPlugin::from_stored_with_transport(
                    stored_openrouter
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    Arc::clone(&transport),
                )
            })
        };
        let builder = if config.provider == "github-copilot" {
            let explicit_token = config.api_key.clone();
            let stored_copilot = stored_copilot.clone();
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || match &explicit_token {
                Some(token) => GitHubCopilotPlugin::new_with_transport(
                    Some(token.clone()),
                    None,
                    Arc::clone(&transport),
                ),
                None => {
                    let available_models = stored_copilot
                        .as_ref()
                        .filter(|credential| credential.is_oauth())
                        .and_then(|credential| credential.extra_strings("availableModelIds"));
                    GitHubCopilotPlugin::from_stored_catalog_with_transport(
                        stored_copilot.as_ref().and_then(|credential| {
                            credential
                                .secret()
                                .map(|token| GitHubCopilotStoredCredential {
                                    token,
                                    enterprise_domain: credential.extra_string("enterpriseUrl"),
                                    available_model_ids: available_models.as_deref(),
                                })
                        }),
                        Arc::clone(&transport),
                    )
                }
            })
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                let available_models = stored_copilot
                    .as_ref()
                    .filter(|credential| credential.is_oauth())
                    .and_then(|credential| credential.extra_strings("availableModelIds"));
                GitHubCopilotPlugin::from_stored_catalog_with_transport(
                    stored_copilot.as_ref().and_then(|credential| {
                        credential
                            .secret()
                            .map(|token| GitHubCopilotStoredCredential {
                                token,
                                enterprise_domain: credential.extra_string("enterpriseUrl"),
                                available_model_ids: available_models.as_deref(),
                            })
                    }),
                    Arc::clone(&transport),
                )
            })
        };
        if config.provider == "amazon-bedrock" {
            let explicit_token = config.api_key.clone();
            let stored_bedrock = stored_bedrock.clone();
            let transport = Arc::clone(&transport);
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
        } else {
            let transport = Arc::clone(&transport);
            builder.try_provider_plugin_factory(move || {
                AmazonBedrockPlugin::from_stored_with_transport(
                    stored_bedrock
                        .as_ref()
                        .and_then(StoredCredential::secret)
                        .map(str::to_owned),
                    stored_bedrock
                        .as_ref()
                        .and_then(StoredCredential::environment)
                        .cloned()
                        .unwrap_or_default(),
                    Arc::clone(&transport),
                )
            })
        }
    }
}
