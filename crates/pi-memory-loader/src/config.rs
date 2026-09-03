//! Parsing and host-owned validation for `memory.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::MemoryRecallOptions;

pub(crate) const CONFIG_FILE: &str = "memory.json";
const CONFIG_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum MemoryConfigError {
    #[error("failed to read memory.json at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse memory.json at {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid memory.json at {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct MemoryRecallDocument {
    max_records: Option<usize>,
    token_budget: Option<usize>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(crate) struct MemoryConfigDocument {
    version: u32,
    enabled: bool,
    provider: String,
    providers: BTreeMap<String, Value>,
    recall: MemoryRecallDocument,
}

impl Default for MemoryConfigDocument {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            enabled: true,
            provider: "hermes".to_string(),
            providers: BTreeMap::new(),
            recall: MemoryRecallDocument::default(),
        }
    }
}

impl MemoryConfigDocument {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn selected_config(&self) -> Option<Value> {
        self.providers.get(&self.provider).cloned()
    }

    pub(crate) fn recall_options(&self, defaults: MemoryRecallOptions) -> MemoryRecallOptions {
        MemoryRecallOptions {
            max_records: self.recall.max_records.unwrap_or(defaults.max_records),
            token_budget: self.recall.token_budget.unwrap_or(defaults.token_budget),
            timeout: self
                .recall
                .timeout_ms
                .map_or(defaults.timeout, Duration::from_millis),
        }
    }
}

pub(crate) fn read_document(
    agent_dir: &Path,
) -> Result<(PathBuf, MemoryConfigDocument), MemoryConfigError> {
    let path = agent_dir.join(CONFIG_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((path, MemoryConfigDocument::default()));
        }
        Err(source) => {
            return Err(MemoryConfigError::Read { path, source });
        }
    };
    if raw.len() > MAX_CONFIG_BYTES {
        return Err(MemoryConfigError::Invalid {
            path,
            message: format!("configuration exceeds {MAX_CONFIG_BYTES} bytes"),
        });
    }
    let document = serde_json::from_str(&raw).map_err(|error| MemoryConfigError::Parse {
        path: path.clone(),
        message: error.to_string(),
    })?;
    validate_document(&path, &document)?;
    Ok((path, document))
}

fn validate_document(
    path: &Path,
    document: &MemoryConfigDocument,
) -> Result<(), MemoryConfigError> {
    let invalid = |message: String| MemoryConfigError::Invalid {
        path: path.to_path_buf(),
        message,
    };
    if document.version != CONFIG_VERSION {
        return Err(invalid(format!(
            "unsupported version {}; expected {CONFIG_VERSION}",
            document.version
        )));
    }
    if document.provider.trim().is_empty() {
        return Err(invalid("provider must not be empty".to_string()));
    }
    if document.recall.max_records == Some(0) {
        return Err(invalid("recall.maxRecords must be positive".to_string()));
    }
    if document.recall.token_budget == Some(0) {
        return Err(invalid("recall.tokenBudget must be positive".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(agent_dir: &Path, raw: &str) {
        std::fs::create_dir_all(agent_dir).unwrap();
        std::fs::write(agent_dir.join(CONFIG_FILE), raw).unwrap();
    }

    #[test]
    fn missing_memory_json_uses_host_defaults_without_creating_files() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");

        let (path, document) = read_document(&agent_dir).unwrap();

        assert_eq!(path, agent_dir.join(CONFIG_FILE));
        assert!(document.enabled());
        assert_eq!(document.provider(), "hermes");
        assert_eq!(
            document.recall_options(MemoryRecallOptions::default()),
            MemoryRecallOptions::default()
        );
        assert!(document.selected_config().is_none());
        assert!(!agent_dir.exists());
    }

    #[test]
    fn selected_provider_configuration_remains_opaque() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        write_config(
            &agent_dir,
            r#"{
                "version": 1,
                "provider": "remote",
                "providers": {
                    "local": ["this", "shape", "is", "irrelevant"],
                    "remote": {
                        "endpoint": "https://memory.example",
                        "ranking": {"weights": [0.25, 0.75]},
                        "vendorExtension": true
                    }
                },
                "recall": {"maxRecords": 12, "tokenBudget": 2400, "timeoutMs": 90}
            }"#,
        );

        let (_, document) = read_document(&agent_dir).unwrap();

        assert_eq!(document.provider(), "remote");
        assert_eq!(
            document.selected_config().unwrap()["ranking"]["weights"][1],
            0.75
        );
        let recall = document.recall_options(MemoryRecallOptions::default());
        assert_eq!(recall.max_records, 12);
        assert_eq!(recall.token_budget, 2_400);
        assert_eq!(recall.timeout, Duration::from_millis(90));
    }

    #[test]
    fn memory_json_recall_fields_override_host_defaults_individually() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        write_config(&agent_dir, r#"{"version": 1, "recall": {"maxRecords": 3}}"#);
        let defaults = MemoryRecallOptions {
            max_records: 9,
            token_budget: 777,
            timeout: Duration::from_millis(123),
        };

        let recall = read_document(&agent_dir)
            .unwrap()
            .1
            .recall_options(defaults);

        assert_eq!(recall.max_records, 3);
        assert_eq!(recall.token_budget, 777);
        assert_eq!(recall.timeout, Duration::from_millis(123));
    }

    #[test]
    fn disabled_and_settings_json_are_independent() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            r#"{"memory": {"enabled": false}}"#,
        )
        .unwrap();

        assert!(read_document(&agent_dir).unwrap().1.enabled());

        write_config(&agent_dir, r#"{"version": 1, "enabled": false}"#);
        assert!(!read_document(&agent_dir).unwrap().1.enabled());
    }

    #[test]
    fn invalid_host_configuration_fails_with_its_path() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        write_config(&agent_dir, r#"{"version": 2}"#);

        let error = read_document(&agent_dir).unwrap_err();

        assert!(error.to_string().contains("unsupported version 2"));
        assert!(error.to_string().contains("memory.json"));

        write_config(&agent_dir, r#"{"version": 1, "hostGuess": true}"#);
        assert!(read_document(&agent_dir).is_err());
    }
}
