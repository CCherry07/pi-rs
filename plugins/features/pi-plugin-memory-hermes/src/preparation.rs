//! Plugin-owned construction and compatibility with the existing `memory.json` profile.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use pi_plugin::{PrepareContext, PrepareError};
use serde::Deserialize;
use serde_json::Value;

use crate::store::HermesMemoryStore;
use crate::{HermesMemoryPlugin, config::HermesMemoryConfig, execution::HermesRuns};

const PROVIDER_ID: &str = "hermes";
const MAX_CONFIG_BYTES: usize = 256 * 1024;

/// User configuration overrides. Hermes reads its own profile when an override is
/// absent; `PrepareContext` supplies host environment and resolved project trust.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct HermesMemoryOptions {
    /// Overrides configured historical-session roots, beyond `<agent_dir>/sessions`.
    /// Relative paths resolve from the agent directory. `Some([])` clears extra roots.
    pub session_roots: Option<Vec<PathBuf>>,
}

pub(super) fn prepare(
    context: &PrepareContext,
    options: HermesMemoryOptions,
) -> Result<Option<HermesMemoryPlugin>, PrepareError> {
    let agent_dir = context.agent_dir();
    let document = read_document(agent_dir)?;
    if !document.enabled {
        return Ok(None);
    }
    if document.provider != PROVIDER_ID {
        return Err(PrepareError::InvalidOptions(format!(
            "memory.json selects unknown provider {}; registered providers: {PROVIDER_ID}",
            document.provider
        )));
    }
    let provider_document =
        HermesMemoryConfig::load_document(agent_dir, document.providers.get(PROVIDER_ID));
    let config = HermesMemoryConfig::from_document(agent_dir, provider_document.as_ref());
    let session_roots = session_roots(agent_dir, options, provider_document.as_ref())?;
    let store = HermesMemoryStore::load(
        agent_dir,
        context.cwd(),
        config.clone(),
        session_roots,
        context.project_trusted(),
    )
    .map_err(|error| {
        PrepareError::initialization(format!(
            "memory provider {PROVIDER_ID} failed to initialize: {error}"
        ))
    })?;
    Ok(Some(HermesMemoryPlugin {
        store: Arc::new(store),
        config,
        runs: Arc::new(HermesRuns::default()),
        foreground_runs: Mutex::new(Default::default()),
        activity: Arc::new(Mutex::new(Default::default())),
        live_index: Mutex::new(None),
        backfill: Mutex::new(None),
        config_warning_emitted: AtomicBool::new(false),
        curator_worker: Mutex::new(None),
    }))
}

fn session_roots(
    agent_dir: &Path,
    options: HermesMemoryOptions,
    document: Option<&Value>,
) -> Result<Vec<PathBuf>, PrepareError> {
    let configured = match options.session_roots {
        Some(roots) => roots,
        None => document
            .and_then(|value| value.get("sessionRoots"))
            .map(|value| serde_json::from_value::<Option<Vec<PathBuf>>>(value.clone()))
            .transpose()
            .map_err(|error| {
                PrepareError::InvalidOptions(format!("invalid Hermes sessionRoots: {error}"))
            })?
            .flatten()
            .unwrap_or_default(),
    };
    let mut roots = vec![agent_dir.join("sessions")];
    for path in configured {
        if path.as_os_str().is_empty() {
            return Err(PrepareError::InvalidOptions(
                "invalid Hermes sessionRoots: paths must not be empty".to_string(),
            ));
        }
        let path = agent_dir.join(path);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    Ok(roots)
}

impl HermesMemoryPlugin {
    /// Reads only profile selection for skill management. It never opens stores,
    /// migrates files or starts background work. Unknown providers are not selected.
    pub fn configured_enabled(agent_dir: &Path) -> Result<bool, PrepareError> {
        let document = read_document(agent_dir)?;
        Ok(document.enabled && document.provider == PROVIDER_ID)
    }
}

// Retain the old document's validation without a provider registry or unused
// runtime recall options. Hermes has never consumed these legacy recall budgets.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct MemoryRecallDocument {
    max_records: Option<usize>,
    token_budget: Option<usize>,
    #[serde(rename = "timeoutMs")]
    _timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct MemoryConfigDocument {
    version: u32,
    enabled: bool,
    provider: String,
    providers: BTreeMap<String, Value>,
    recall: MemoryRecallDocument,
}

impl Default for MemoryConfigDocument {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            provider: PROVIDER_ID.to_string(),
            providers: BTreeMap::new(),
            recall: MemoryRecallDocument::default(),
        }
    }
}

fn read_document(agent_dir: &Path) -> Result<MemoryConfigDocument, PrepareError> {
    let path = agent_dir.join("memory.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MemoryConfigDocument::default());
        }
        Err(error) => {
            return Err(PrepareError::InvalidOptions(format!(
                "failed to read memory.json at {}: {error}",
                path.display()
            )));
        }
    };
    let invalid = |message: String| {
        PrepareError::InvalidOptions(format!(
            "invalid memory.json at {}: {message}",
            path.display()
        ))
    };
    if raw.len() > MAX_CONFIG_BYTES {
        return Err(invalid(format!(
            "configuration exceeds {MAX_CONFIG_BYTES} bytes"
        )));
    }
    let document: MemoryConfigDocument = serde_json::from_str(&raw).map_err(|error| {
        PrepareError::InvalidOptions(format!(
            "failed to parse memory.json at {}: {error}",
            path.display()
        ))
    })?;
    if document.version != 1 {
        return Err(invalid(format!(
            "unsupported version {}; expected 1",
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
    Ok(document)
}
