//! Deterministic SDK configuration shared by composition and generation tests.
use crate::Config;

pub(crate) fn app_config(agent_dir: &std::path::Path, model: Option<&str>) -> Config {
    Config {
        workspace: None,
        features: crate::Features::default(),
        cwd: agent_dir.to_path_buf(),
        agent_dir: agent_dir.to_path_buf(),
        session_path: agent_dir.join("session.jsonl"),
        model: model.map(str::to_string),
        thinking: None,
        fallback_model: "gpt-4o-mini".to_string(),
        base_url: "https://fallback.example/v1".to_string(),
        api_key: None,
        provider: "openai-compatible".to_string(),
        requested_provider: None,
        trust_override: None,
        native_plugins: Vec::new(),
        extensions: Vec::new(),
        discover_extensions: true,
        load_mcp_config: true,
        extension_flag_values: std::collections::BTreeMap::new(),
        runtime_settings: pi_settings::SettingsValues::default(),
        settings_skill_paths: Vec::new(),
        settings_prompt_paths: Vec::new(),
        settings_diagnostics: Vec::new(),
    }
}
