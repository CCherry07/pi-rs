//! Headless Pi product integration for CLI, desktop, and embedded adapters.

mod builtin_providers;
mod credentials;
pub mod desktop_extensions;
mod dynamic_providers;
mod host;
pub mod mcp;
pub mod plugins;
mod project_trust;
mod session_factory;
pub mod skills;

pub use credentials::{StoredCredential, read_credentials, read_stored_credential};
pub use host::{Pi, PiBuilder};
pub use pi_plugin_memory_hermes::curator;
pub use project_trust::{
    ProjectTrustError, ProjectTrustEvaluation, ProjectTrustOption, ProjectTrustPromptRequest,
    ProjectTrustService,
};
pub use session_factory::ProductSessionFactory;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use pi_core::ThinkingLevel;
use pi_js_package_manager::ResolveRequest as JsResolveRequest;

/// Product configuration shared by every Pi presentation adapter.
#[derive(Debug, Clone)]
pub struct ProductConfig {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_path: PathBuf,
    pub model: Option<String>,
    pub thinking: Option<ThinkingLevel>,
    pub fallback_model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub provider: String,
    pub requested_provider: Option<String>,
    pub trust_override: Option<bool>,
    pub native_plugins: Vec<PathBuf>,
    pub extensions: Vec<String>,
    pub discover_extensions: bool,
    /// ACP clients supply their own transient MCP servers instead of local files.
    pub load_mcp_config: bool,
    pub extension_flag_values: BTreeMap<String, serde_json::Value>,
    pub runtime_settings: pi_settings::SettingsValues,
    pub settings_skill_paths: Vec<PathBuf>,
    pub settings_prompt_paths: Vec<PathBuf>,
    pub settings_diagnostics: Vec<pi_resources::ResourceDiagnostic>,
}

impl ProductConfig {
    pub fn new(cwd: PathBuf, agent_dir: PathBuf) -> Self {
        let provider = "openai-compatible".to_string();
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty());
        let fallback_model = std::env::var("OPENAI_MODEL")
            .ok()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());
        let base_url = std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let session_path = agent_dir
            .join("sessions")
            .join(format!("{}.jsonl", uuid::Uuid::now_v7()));
        Self {
            cwd,
            agent_dir,
            session_path,
            model: None,
            thinking: None,
            fallback_model,
            base_url,
            api_key,
            provider,
            requested_provider: None,
            trust_override: None,
            native_plugins: Vec::new(),
            extensions: Vec::new(),
            discover_extensions: true,
            load_mcp_config: true,
            extension_flag_values: BTreeMap::new(),
            runtime_settings: pi_settings::SettingsValues::default(),
            settings_skill_paths: Vec::new(),
            settings_prompt_paths: Vec::new(),
            settings_diagnostics: Vec::new(),
        }
    }

    pub fn javascript_resolve_request(&self, project_trusted: bool) -> JsResolveRequest {
        JsResolveRequest {
            cwd: self.cwd.clone(),
            agent_dir: self.agent_dir.clone(),
            project_trusted,
            explicit_sources: self.extensions.clone(),
            discover_extensions: self.discover_extensions,
        }
    }

    /// Applies a startup-only global session directory unless the adapter
    /// already selected an explicit session path.
    pub fn apply_session_dir_setting(&mut self, session_dir: Option<&str>, explicit: bool) {
        if explicit {
            return;
        }
        let Some(session_dir) = session_dir.filter(|path| !path.trim().is_empty()) else {
            return;
        };
        let directory = expand_tilde_path(session_dir);
        let file_name = self.session_path.file_name().map_or_else(
            || OsString::from(format!("{}.jsonl", uuid::Uuid::now_v7())),
            OsString::from,
        );
        self.session_path = directory.join(file_name);
    }
}

pub fn default_agent_dir() -> Option<PathBuf> {
    std::env::var_os("PI_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi").join("agent"))
        })
}

fn expand_tilde_path(path: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        if path == "~" {
            return PathBuf::from(home);
        }
        if let Some(relative) = path.strip_prefix("~/") {
            return PathBuf::from(home).join(relative);
        }
    }
    PathBuf::from(path)
}
