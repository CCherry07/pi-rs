use std::collections::BTreeMap;

use pi_core::{ProviderError, ProviderId};

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    pub provider_id: ProviderId,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl OpenAiCompatibleConfig {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("openai-compatible"),
            base_url: base_url.into(),
            api_key: Some(api_key.into()),
            headers: BTreeMap::new(),
        }
    }

    pub fn without_api_key(base_url: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("openai-compatible"),
            base_url: base_url.into(),
            api_key: None,
            headers: BTreeMap::new(),
        }
    }

    pub fn provider_id(mut self, id: impl Into<ProviderId>) -> Self {
        self.provider_id = id.into();
        self
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: String,
    pub organization: Option<String>,
    pub project: Option<String>,
}

impl OpenAiConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".to_string(),
            organization: None,
            project: None,
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn organization(mut self, value: impl Into<String>) -> Self {
        self.organization = Some(value.into());
        self
    }

    pub fn project(mut self, value: impl Into<String>) -> Self {
        self.project = Some(value.into());
        self
    }

    pub(crate) fn compatible_config(self) -> OpenAiCompatibleConfig {
        let mut config = OpenAiCompatibleConfig::new(self.base_url, self.api_key)
            .provider_id(ProviderId::new("openai"));
        if let Some(value) = self.organization {
            config = config.header("OpenAI-Organization", value);
        }
        if let Some(value) = self.project {
            config = config.header("OpenAI-Project", value);
        }
        config
    }
}

pub(crate) fn validate_config(config: &OpenAiCompatibleConfig) -> Result<(), ProviderError> {
    if config.base_url.trim().is_empty() {
        return Err(ProviderError::Failure(
            "base URL cannot be empty".to_string(),
        ));
    }
    if config
        .api_key
        .as_ref()
        .is_some_and(|key| key.contains(['\r', '\n']))
    {
        return Err(ProviderError::Failure("invalid API key".to_string()));
    }
    Ok(())
}
