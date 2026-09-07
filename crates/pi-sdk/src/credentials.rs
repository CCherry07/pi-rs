use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredCredential {
    ApiKey {
        #[serde(default)]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    Oauth {
        access: String,
        refresh: String,
        expires: f64,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

impl StoredCredential {
    pub fn secret(&self) -> Option<&str> {
        match self {
            Self::ApiKey { key, .. } => key.as_deref(),
            Self::Oauth { access, .. } => Some(access.as_str()),
        }
        .map(str::trim)
        .filter(|value| !value.is_empty())
    }

    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::Oauth { .. })
    }

    pub fn extra_string(&self, name: &str) -> Option<&str> {
        let extra = match self {
            Self::ApiKey { extra, .. } | Self::Oauth { extra, .. } => extra,
        };
        extra.get(name).and_then(serde_json::Value::as_str)
    }

    pub fn extra_strings(&self, name: &str) -> Option<Vec<String>> {
        let extra = match self {
            Self::ApiKey { extra, .. } | Self::Oauth { extra, .. } => extra,
        };
        extra
            .get(name)?
            .as_array()?
            .iter()
            .map(|value| value.as_str().map(str::to_string))
            .collect()
    }

    pub fn environment(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::ApiKey { env, .. } => Some(env),
            Self::Oauth { .. } => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        if self.is_oauth() { "oauth" } else { "api_key" }
    }
}

pub fn read_stored_credential(
    agent_dir: &Path,
    provider: &str,
) -> Result<Option<StoredCredential>, String> {
    Ok(read_credentials(&agent_dir.join("auth.json"))?.remove(provider))
}

pub fn read_credentials(path: &Path) -> Result<BTreeMap<String, StoredCredential>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    serde_json::from_str(&content).map_err(|error| format!("invalid {}: {error}", path.display()))
}
