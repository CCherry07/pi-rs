#![forbid(unsafe_code)]

//! Pi-compatible current-format settings loading and persistence.
//!
//! The manager keeps raw JSON documents so newer and UI-only fields survive
//! writes, while callers consume an immutable, typed snapshot for one runtime
//! generation. Historical setting names and migrations are intentionally not
//! supported.

mod document;
mod store;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

const CONFIG_DIRECTORY: &str = ".pi";

/// One settings file's product scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsScope {
    Global,
    Project,
}

/// The cwd and trust decision used to build one immutable settings snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsContext {
    pub cwd: PathBuf,
    pub project_trusted: bool,
}

impl SettingsContext {
    pub fn new(cwd: impl Into<PathBuf>, project_trusted: bool) -> Self {
        Self {
            cwd: cwd.into(),
            project_trusted,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DefaultProjectTrust {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueueModeSetting {
    All,
    #[default]
    OneAtATime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransportSetting {
    Sse,
    Websocket,
    WebsocketCached,
    #[default]
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevelSetting {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchSummarySettings {
    pub reserve_tokens: u64,
    pub skip_prompt: bool,
}

impl Default for BranchSummarySettings {
    fn default() -> Self {
        Self {
            reserve_tokens: 16_384,
            skip_prompt: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRetrySettings {
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: u64,
}

impl Default for ProviderRetrySettings {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySettings {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub provider: ProviderRetrySettings,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2_000,
            provider: ProviderRetrySettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSettings {
    pub auto_resize: bool,
    pub block_images: bool,
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            auto_resize: true,
            block_images: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkingBudgetsSettings {
    pub minimal: Option<u64>,
    pub low: Option<u64>,
    pub medium: Option<u64>,
    pub high: Option<u64>,
}

/// Current Pi package source syntax. Resource filters not consumed by the
/// current frontend are retained for non-UI resource resolution and round trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackageSource {
    String(String),
    Filter(PackageFilter),
}

impl PackageSource {
    pub fn source(&self) -> &str {
        match self {
            Self::String(source) => source,
            Self::Filter(filter) => &filter.source,
        }
    }

    pub fn filter(&self) -> Option<&PackageFilter> {
        match self {
            Self::String(_) => None,
            Self::Filter(filter) => Some(filter),
        }
    }

    pub fn set_source(&mut self, source: String) {
        match self {
            Self::String(existing) => *existing = source,
            Self::Filter(filter) => filter.source = source,
        }
    }

    pub fn is_autoload_delta(&self) -> bool {
        matches!(self, Self::Filter(filter) if filter.autoload == Some(false))
    }

    pub fn may_enable_extensions(&self) -> bool {
        match self {
            Self::String(_) => true,
            Self::Filter(filter) => filter.extensions.as_ref().map_or_else(
                || filter.autoload != Some(false),
                |extensions| !extensions.is_empty(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFilter {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoload: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Validated values from one raw document or the effective deep merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsValues {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<ThinkingLevelSetting>,
    pub transport: TransportSetting,
    pub steering_mode: QueueModeSetting,
    pub follow_up_mode: QueueModeSetting,
    pub compaction: CompactionSettings,
    pub branch_summary: BranchSummarySettings,
    pub retry: RetrySettings,
    pub default_project_trust: DefaultProjectTrust,
    pub shell_path: Option<String>,
    pub shell_command_prefix: Option<String>,
    pub npm_command: Option<Vec<String>>,
    pub packages: Vec<PackageSource>,
    pub extensions: Vec<String>,
    pub skills: Vec<String>,
    pub prompts: Vec<String>,
    pub themes: Vec<String>,
    pub enable_skill_commands: bool,
    pub enabled_models: Option<Vec<String>>,
    pub default_tools: Option<Vec<String>>,
    pub thinking_budgets: Option<ThinkingBudgetsSettings>,
    pub session_dir: Option<String>,
    pub http_proxy: Option<String>,
    pub http_idle_timeout_ms: u64,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub images: ImageSettings,
}

impl Default for SettingsValues {
    fn default() -> Self {
        Self {
            default_provider: None,
            default_model: None,
            default_thinking_level: None,
            transport: TransportSetting::Auto,
            steering_mode: QueueModeSetting::OneAtATime,
            follow_up_mode: QueueModeSetting::OneAtATime,
            compaction: CompactionSettings::default(),
            branch_summary: BranchSummarySettings::default(),
            retry: RetrySettings::default(),
            default_project_trust: DefaultProjectTrust::Ask,
            shell_path: None,
            shell_command_prefix: None,
            npm_command: None,
            packages: Vec::new(),
            extensions: Vec::new(),
            skills: Vec::new(),
            prompts: Vec::new(),
            themes: Vec::new(),
            enable_skill_commands: true,
            enabled_models: None,
            default_tools: None,
            thinking_budgets: None,
            session_dir: None,
            http_proxy: None,
            http_idle_timeout_ms: 300_000,
            websocket_connect_timeout_ms: None,
            images: ImageSettings::default(),
        }
    }
}

/// One immutable settings view used by a runtime generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSnapshot {
    global: SettingsValues,
    project: SettingsValues,
    effective: SettingsValues,
    raw_global: Map<String, Value>,
    raw_project: Map<String, Value>,
    raw_effective: Map<String, Value>,
    diagnostics: Vec<SettingsDiagnostic>,
    project_trusted: bool,
}

impl SettingsSnapshot {
    pub fn global(&self) -> &SettingsValues {
        &self.global
    }

    pub fn project(&self) -> &SettingsValues {
        &self.project
    }

    pub fn effective(&self) -> &SettingsValues {
        &self.effective
    }

    pub fn raw_global(&self) -> &Map<String, Value> {
        &self.raw_global
    }

    pub fn raw_project(&self) -> &Map<String, Value> {
        &self.raw_project
    }

    pub fn raw_effective(&self) -> &Map<String, Value> {
        &self.raw_effective
    }

    pub fn diagnostics(&self) -> &[SettingsDiagnostic] {
        &self.diagnostics
    }

    pub fn project_trusted(&self) -> bool {
        self.project_trusted
    }

    /// Project settings never influence the default trust policy.
    pub fn default_project_trust(&self) -> DefaultProjectTrust {
        self.global.default_project_trust
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsDiagnosticKind {
    Read,
    Parse,
    InvalidValue,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDiagnostic {
    pub scope: SettingsScope,
    pub path: PathBuf,
    pub kind: SettingsDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("project is not trusted; refusing to write project settings")]
    ProjectNotTrusted,
    #[error("cannot {operation} settings {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid settings {path}: {message}")]
    InvalidDocument { path: PathBuf, message: String },
    #[error("cannot encode settings {path}: {message}")]
    Encode { path: PathBuf, message: String },
}

#[derive(Default)]
struct ManagerState {
    last_valid: HashMap<PathBuf, Map<String, Value>>,
    diagnostics: Vec<SettingsDiagnostic>,
}

/// Deep settings Module shared by trust, package, and runtime adapters.
#[derive(Clone)]
pub struct SettingsManager {
    agent_dir: PathBuf,
    state: Arc<Mutex<ManagerState>>,
}

impl SettingsManager {
    pub fn new(agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            state: Arc::new(Mutex::new(ManagerState::default())),
        }
    }

    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    /// Reloads both scopes and returns a generation-local immutable snapshot.
    /// A malformed scope retains that path's last valid document.
    pub fn load(&self, context: &SettingsContext) -> SettingsSnapshot {
        let global_path = self.global_path();
        let (raw_global, mut diagnostics) = self.load_scope(SettingsScope::Global, &global_path);
        let project_path = self.project_path(&context.cwd);
        let raw_project = if context.project_trusted {
            let (document, project_diagnostics) =
                self.load_scope(SettingsScope::Project, &project_path);
            diagnostics.extend(project_diagnostics);
            document
        } else {
            Map::new()
        };
        let raw_effective = document::deep_merge(&raw_global, &raw_project);

        let global = document::decode_values(
            &raw_global,
            Some((SettingsScope::Global, &global_path)),
            &mut diagnostics,
        );
        let project = document::decode_values(
            &raw_project,
            context
                .project_trusted
                .then_some((SettingsScope::Project, project_path.as_path())),
            &mut diagnostics,
        );
        let mut effective = document::decode_values(&raw_effective, None, &mut diagnostics);
        effective.default_project_trust = global.default_project_trust;

        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diagnostics
            .extend(diagnostics.iter().cloned());

        SettingsSnapshot {
            global,
            project,
            effective,
            raw_global,
            raw_project,
            raw_effective,
            diagnostics,
            project_trusted: context.project_trusted,
        }
    }

    /// Replaces the configured package list in one scope. The write is locked,
    /// based on the latest disk document, and preserves every unrelated field.
    pub fn replace_packages(
        &self,
        context: &SettingsContext,
        scope: SettingsScope,
        packages: &[PackageSource],
    ) -> Result<SettingsSnapshot, SettingsError> {
        if scope == SettingsScope::Project && !context.project_trusted {
            return Err(SettingsError::ProjectNotTrusted);
        }
        let path = match scope {
            SettingsScope::Global => self.global_path(),
            SettingsScope::Project => self.project_path(&context.cwd),
        };
        let value = serde_json::to_value(packages).map_err(|error| SettingsError::Encode {
            path: path.clone(),
            message: error.to_string(),
        })?;
        match store::replace_top_level(&path, "packages", value) {
            Ok(document) => {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .last_valid
                    .insert(path, document);
                Ok(self.load(context))
            }
            Err(error) => {
                self.record_write_error(scope, &path, &error);
                Err(error)
            }
        }
    }

    pub fn drain_errors(&self) -> Vec<SettingsDiagnostic> {
        std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .diagnostics,
        )
    }

    fn load_scope(
        &self,
        scope: SettingsScope,
        path: &Path,
    ) -> (Map<String, Value>, Vec<SettingsDiagnostic>) {
        match document::read(path) {
            Ok(document) => {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .last_valid
                    .insert(path.to_path_buf(), document.clone());
                (document, Vec::new())
            }
            Err(error) => {
                let diagnostic = SettingsDiagnostic {
                    scope,
                    path: path.to_path_buf(),
                    kind: match error {
                        SettingsError::InvalidDocument { .. } => SettingsDiagnosticKind::Parse,
                        _ => SettingsDiagnosticKind::Read,
                    },
                    message: error.to_string(),
                };
                let fallback = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .last_valid
                    .get(path)
                    .cloned()
                    .unwrap_or_default();
                (fallback, vec![diagnostic])
            }
        }
    }

    fn record_write_error(&self, scope: SettingsScope, path: &Path, error: &SettingsError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diagnostics
            .push(SettingsDiagnostic {
                scope,
                path: path.to_path_buf(),
                kind: SettingsDiagnosticKind::Write,
                message: error.to_string(),
            });
    }

    fn global_path(&self) -> PathBuf {
        self.agent_dir.join("settings.json")
    }

    fn project_path(&self, cwd: &Path) -> PathBuf {
        cwd.join(CONFIG_DIRECTORY).join("settings.json")
    }
}
