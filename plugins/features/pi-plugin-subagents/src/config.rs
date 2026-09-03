use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::catalog::{SubagentCatalogError, SubagentLoaderOptions};
use crate::runtime::DEFAULT_MAX_DEPTH;

const MAX_CONFIG_BYTES: usize = 256 * 1024;
const MAX_DEPTH_ENV: &str = "PI_SUBAGENT_MAX_DEPTH";

pub(crate) fn load_max_depth(
    options: &SubagentLoaderOptions,
) -> Result<usize, SubagentCatalogError> {
    let configured = read_config_max_depth(&options.agent_dir)?;
    Ok(resolve_max_depth(
        std::env::var_os(MAX_DEPTH_ENV).as_deref(),
        configured,
    ))
}

fn config_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("extensions/subagent/config.json")
}

fn read_config_max_depth(agent_dir: &Path) -> Result<Option<usize>, SubagentCatalogError> {
    let path = config_path(agent_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(config_error(path, error.to_string())),
    };
    if raw.len() > MAX_CONFIG_BYTES {
        return Err(config_error(
            path,
            format!("configuration exceeds {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| config_error(&path, error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| config_error(&path, "top-level value must be an object".to_string()))?;
    let Some(value) = object.get("maxSubagentDepth") else {
        return Ok(None);
    };
    normalize_json_max_depth(value).map(Some).ok_or_else(|| {
        config_error(
            path,
            "maxSubagentDepth must be a non-negative integer".to_string(),
        )
    })
}

fn resolve_max_depth(environment: Option<&OsStr>, configured: Option<usize>) -> usize {
    environment
        .and_then(normalize_environment_max_depth)
        .or(configured)
        .unwrap_or(DEFAULT_MAX_DEPTH)
}

fn normalize_environment_max_depth(value: &OsStr) -> Option<usize> {
    value.to_str()?.trim().parse().ok()
}

fn normalize_json_max_depth(value: &Value) -> Option<usize> {
    match value {
        Value::Number(value) => usize::try_from(value.as_u64()?).ok(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn config_error(path: impl Into<PathBuf>, message: String) -> SubagentCatalogError {
    SubagentCatalogError::Configuration {
        path: path.into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_default_and_valid_environment_wins() {
        assert_eq!(resolve_max_depth(None, None), DEFAULT_MAX_DEPTH);
        assert_eq!(resolve_max_depth(Some(OsStr::new("4")), Some(2)), 4);
        assert_eq!(resolve_max_depth(Some(OsStr::new("invalid")), Some(2)), 2);
    }

    #[test]
    fn feature_config_accepts_zero_and_non_negative_integer_strings() {
        let directory = tempfile::tempdir().unwrap();
        let config = config_path(directory.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, r#"{"maxSubagentDepth": 0}"#).unwrap();
        assert_eq!(read_config_max_depth(directory.path()).unwrap(), Some(0));

        std::fs::write(&config, r#"{"maxSubagentDepth": "6"}"#).unwrap();
        assert_eq!(read_config_max_depth(directory.path()).unwrap(), Some(6));
    }

    #[test]
    fn invalid_feature_config_fails_transactionally() {
        let directory = tempfile::tempdir().unwrap();
        let config = config_path(directory.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, r#"{"maxSubagentDepth": -1}"#).unwrap();
        let error = read_config_max_depth(directory.path()).unwrap_err();
        assert!(error.to_string().contains("non-negative integer"));
        assert!(error.to_string().contains(config.to_str().unwrap()));
    }
}
