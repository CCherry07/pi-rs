//! Hermes configuration and prompt constants.
//!
//! Runtime configuration uses Pi JSON naming; behavioral defaults follow
//! NousResearch/hermes-agent e629c900a87622ddcc31f67a4b4a756b239fbaf0.

use std::path::{Path, PathBuf};

use pi_core::ThinkingLevel;
use serde_json::{Map, Value};

pub(crate) const ENTRY_DELIMITER: &str = "\n§\n";
pub(crate) const MEMORY_FILE: &str = "MEMORY.md";
pub(crate) const USER_FILE: &str = "USER.md";
pub(crate) const FAILURE_FILE: &str = "failures.md";
pub(crate) const STANDING_FILE: &str = "STANDING.md";
pub(crate) const DEFAULT_PROJECTS_MEMORY_DIR: &str = "projects-memory";
pub(crate) const DEFAULT_MEMORY_CHAR_LIMIT: usize = 2_200;
pub(crate) const DEFAULT_USER_CHAR_LIMIT: usize = 1_375;
pub(crate) const DEFAULT_PROJECT_CHAR_LIMIT: usize = 5_000;
pub(crate) const DEFAULT_NUDGE_INTERVAL: u64 = 10;
pub(crate) const DEFAULT_FLUSH_MIN_TURNS: u64 = 6;
pub(crate) const DEFAULT_CONSOLIDATION_TIMEOUT_MS: u64 = 180_000;
pub(crate) const DEFAULT_FAILURE_INJECTION_MAX_AGE_DAYS: i64 = 7;
pub(crate) const DEFAULT_FAILURE_INJECTION_MAX_ENTRIES: usize = 5;
pub(crate) const STANDING_MAX_ENTRIES: usize = 20;
pub(crate) const STANDING_MAX_CHARS: usize = 2_000;
pub(crate) fn char_len(value: &str) -> usize {
    value.chars().count()
}
pub(crate) fn char_prefix(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}
pub(crate) fn char_suffix(value: &str, maximum: usize) -> String {
    value
        .chars()
        .skip(value.chars().count().saturating_sub(maximum))
        .collect()
}

pub(crate) const MEMORY_TOOL_DESCRIPTION: &str = "Save durable user preferences and environment facts across sessions. Use action add, replace, remove, or an atomic operations array. Target is memory (default) or user. replace/remove identify an entry by a short unique old_text substring. If capacity is exceeded, consolidate overlapping entries and retry in the same turn. Do not store task progress or secrets. Successful writes are durable but do not change this session's frozen memory prompt.";

pub(crate) const SKILL_TOOL_DESCRIPTION: &str = "Maintain reusable class-level procedures as Pi-native SKILL.md files. Use skills_list to discover and skill_view to read. Prefer improving existing skills to creating narrow task-specific ones. create accepts name, description, content (Markdown or a complete SKILL.md), and optional scope (global default; project for repo-specific procedures). edit/update rewrites the body; patch accepts old_string/new_string or section plus content. write_file/remove_file maintain relative supporting file_path paths. Background review may modify only agent-created, unpinned skills and must read the exact existing file in the current review before changing it. Autonomous delete requires absorbed_into naming an existing skill and archives the source. Never store task progress or secrets.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MemoryOverflowStrategy {
    #[default]
    Reject,
    FifoEvict,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SessionSearchVariant {
    #[default]
    Legacy,
    Anchors,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesMemoryConfig {
    pub(crate) memory_char_limit: usize,
    pub(crate) user_char_limit: usize,
    pub(crate) project_char_limit: usize,
    pub(crate) nudge_interval: u64,
    pub(crate) review_enabled: bool,
    pub(crate) skill_nudge_interval: u64,
    pub(crate) review_extra_tools: Vec<String>,
    pub(crate) review_max_input_tokens: Option<u64>,
    pub(crate) flush_on_compact: bool,
    pub(crate) flush_on_shutdown: bool,
    pub(crate) flush_min_turns: u64,
    pub(crate) memory_dir: Option<PathBuf>,
    pub(crate) projects_memory_dir: String,
    pub(crate) session_search_variant: SessionSearchVariant,
    pub(crate) llm_model_override: Option<String>,
    pub(crate) llm_thinking_override: Option<ThinkingLevel>,
    pub(crate) memory_overflow_strategy: MemoryOverflowStrategy,
    pub(crate) failure_injection_enabled: bool,
    pub(crate) failure_injection_max_age_days: i64,
    pub(crate) failure_injection_max_entries: usize,
    pub(crate) consolidation_timeout_ms: u64,
    pub(crate) standing_instructions_enabled: bool,
}

impl Default for HermesMemoryConfig {
    fn default() -> Self {
        Self {
            memory_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
            user_char_limit: DEFAULT_USER_CHAR_LIMIT,
            project_char_limit: DEFAULT_PROJECT_CHAR_LIMIT,
            nudge_interval: DEFAULT_NUDGE_INTERVAL,
            review_enabled: true,
            skill_nudge_interval: 10,
            review_extra_tools: Vec::new(),
            review_max_input_tokens: Some(600_000),
            flush_on_compact: false,
            flush_on_shutdown: false,
            flush_min_turns: DEFAULT_FLUSH_MIN_TURNS,
            memory_dir: None,
            projects_memory_dir: DEFAULT_PROJECTS_MEMORY_DIR.to_string(),
            session_search_variant: SessionSearchVariant::Legacy,
            llm_model_override: None,
            llm_thinking_override: None,
            memory_overflow_strategy: MemoryOverflowStrategy::Reject,
            failure_injection_enabled: false,
            failure_injection_max_age_days: DEFAULT_FAILURE_INJECTION_MAX_AGE_DAYS,
            failure_injection_max_entries: DEFAULT_FAILURE_INJECTION_MAX_ENTRIES,
            consolidation_timeout_ms: DEFAULT_CONSOLIDATION_TIMEOUT_MS,
            standing_instructions_enabled: false,
        }
    }
}

impl HermesMemoryConfig {
    pub(crate) fn load(agent_dir: &Path, provider_value: Option<&Value>) -> Self {
        let path = agent_dir.join("hermes-memory-config.json");
        let value = match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str::<Value>(&raw).ok(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => provider_value.cloned(),
            Err(_) => None,
        };
        value
            .as_ref()
            .and_then(Value::as_object)
            .map_or_else(Self::default, |object| Self::from_object(object, agent_dir))
    }

    fn from_object(object: &Map<String, Value>, agent_dir: &Path) -> Self {
        let mut config = Self::default();
        set_usize(object, "memoryCharLimit", &mut config.memory_char_limit);
        set_usize(object, "userCharLimit", &mut config.user_char_limit);
        set_usize(object, "projectCharLimit", &mut config.project_char_limit);
        set_u64(object, "nudgeInterval", &mut config.nudge_interval);
        set_bool(object, "reviewEnabled", &mut config.review_enabled);
        set_u64(
            object,
            "skillNudgeInterval",
            &mut config.skill_nudge_interval,
        );
        config.review_extra_tools = string_array(object, "reviewExtraTools").unwrap_or_default();
        if let Some(value) = object.get("reviewMaxInputTokens").and_then(Value::as_i64) {
            config.review_max_input_tokens = (value > 0).then_some(value as u64);
        }
        set_bool(object, "flushOnCompact", &mut config.flush_on_compact);
        set_bool(object, "flushOnShutdown", &mut config.flush_on_shutdown);
        set_u64(object, "flushMinTurns", &mut config.flush_min_turns);
        config.memory_overflow_strategy = match string(object, "memoryOverflowStrategy") {
            Some("fifo-evict") => MemoryOverflowStrategy::FifoEvict,
            _ => MemoryOverflowStrategy::Reject,
        };
        set_bool(
            object,
            "failureInjectionEnabled",
            &mut config.failure_injection_enabled,
        );
        set_i64(
            object,
            "failureInjectionMaxAgeDays",
            &mut config.failure_injection_max_age_days,
        );
        set_usize(
            object,
            "failureInjectionMaxEntries",
            &mut config.failure_injection_max_entries,
        );
        set_u64(
            object,
            "consolidationTimeoutMs",
            &mut config.consolidation_timeout_ms,
        );
        set_bool(
            object,
            "standingInstructionsEnabled",
            &mut config.standing_instructions_enabled,
        );
        if let Some(memory_dir) = owned_string(object, "memoryDir", true) {
            config.memory_dir = normalize_memory_dir(&memory_dir);
        }
        if let Some(projects) = owned_string(object, "projectsMemoryDir", true)
            && let Some(projects) = normalize_projects_memory_dir(&projects, agent_dir)
        {
            config.projects_memory_dir = projects;
        }
        if let Some(search) = object.get("sessionSearch").and_then(Value::as_object) {
            config.session_search_variant = match string(search, "variant") {
                Some("anchors") => SessionSearchVariant::Anchors,
                _ => SessionSearchVariant::Legacy,
            };
        }
        config.llm_model_override = owned_string(object, "llmModelOverride", true);
        config.llm_thinking_override =
            string(object, "llmThinkingOverride").and_then(parse_thinking);
        config
    }

    pub(crate) fn global_dir(&self, agent_dir: &Path) -> PathBuf {
        let legacy = agent_dir.join("memory");
        match self.memory_dir.as_ref() {
            None => agent_dir.join("pi-hermes-memory"),
            Some(configured) if absolutize(agent_dir, configured) == legacy => {
                agent_dir.join("pi-hermes-memory")
            }
            Some(configured) => absolutize(agent_dir, configured),
        }
    }

    pub(crate) fn should_migrate_extension_root(&self, agent_dir: &Path) -> bool {
        self.memory_dir
            .as_ref()
            .is_none_or(|configured| absolutize(agent_dir, configured) == agent_dir.join("memory"))
    }

    pub(crate) fn projects_root(&self, agent_dir: &Path) -> PathBuf {
        agent_dir.join(&self.projects_memory_dir)
    }

    pub(crate) fn consolidation_timeout_warning(&self) -> Option<String> {
        (self.consolidation_timeout_ms < DEFAULT_CONSOLIDATION_TIMEOUT_MS).then(|| {
            format!(
                "⚠️ consolidationTimeoutMs is set to {}ms, below the {}ms default. Consolidation spawns a child agent turn and is routinely killed mid-run at lower values.",
                self.consolidation_timeout_ms, DEFAULT_CONSOLIDATION_TIMEOUT_MS
            )
        })
    }
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn owned_string(object: &Map<String, Value>, key: &str, trim: bool) -> Option<String> {
    string(object, key).and_then(|value| {
        let value = if trim { value.trim() } else { value };
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn string_array(object: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    let values = object.get(key)?.as_array()?;
    values
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect()
}

fn set_bool(object: &Map<String, Value>, key: &str, target: &mut bool) {
    if let Some(value) = object.get(key).and_then(Value::as_bool) {
        *target = value;
    }
}

fn set_u64(object: &Map<String, Value>, key: &str, target: &mut u64) {
    if let Some(value) = object.get(key).and_then(Value::as_u64) {
        *target = value;
    }
}

fn set_i64(object: &Map<String, Value>, key: &str, target: &mut i64) {
    if let Some(value) = object.get(key).and_then(Value::as_i64) {
        *target = value;
    }
}

fn set_usize(object: &Map<String, Value>, key: &str, target: &mut usize) {
    if let Some(value) = object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
    {
        *target = value;
    }
}

fn normalize_memory_dir(value: &str) -> Option<PathBuf> {
    let path = if value == "~" {
        std::env::var_os("HOME").map(PathBuf::from)?
    } else if let Some(suffix) = value.strip_prefix("~/") {
        std::env::var_os("HOME").map(PathBuf::from)?.join(suffix)
    } else {
        PathBuf::from(value)
    };
    Some(path)
}

fn absolutize(agent_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        agent_dir.join(path)
    }
}

fn normalize_projects_memory_dir(value: &str, agent_dir: &Path) -> Option<String> {
    let expanded = normalize_memory_dir(value)?;
    let relative = if expanded.is_absolute() {
        expanded.strip_prefix(agent_dir).ok()?.to_path_buf()
    } else {
        expanded
    };
    let mut components = relative.components();
    let component = components.next()?;
    if components.next().is_some() {
        return None;
    }
    let value = component.as_os_str().to_str()?;
    (!matches!(value, "" | "." | "..")).then(|| value.to_string())
}

fn parse_thinking(value: &str) -> Option<ThinkingLevel> {
    match value {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::XHigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_upstream() {
        let config = HermesMemoryConfig::default();
        assert_eq!(config.memory_char_limit, 2_200);
        assert_eq!(config.user_char_limit, 1_375);
        assert_eq!(config.review_max_input_tokens, Some(600_000));
        assert_eq!(config.nudge_interval, 10);
        assert_eq!(config.skill_nudge_interval, 10);
        assert_eq!(config.consolidation_timeout_ms, 180_000);
    }

    #[test]
    fn invalid_and_unknown_values_are_ignored_individually() {
        let config = HermesMemoryConfig::from_object(
            serde_json::json!({
                "unknown": true,
                "memoryMode": "wrong",
                "reviewRecentMessages": -1,
                "memoryCharLimit": 1234,
                "correctionWeakPatterns": []
            })
            .as_object()
            .unwrap(),
            Path::new("/tmp/agent"),
        );
        assert_eq!(config.memory_char_limit, 1_234);
    }

    #[test]
    fn warns_only_when_consolidation_timeout_is_below_upstream_default() {
        let mut config = HermesMemoryConfig {
            consolidation_timeout_ms: 60_000,
            ..HermesMemoryConfig::default()
        };
        assert_eq!(
            config.consolidation_timeout_warning().as_deref(),
            Some(
                "⚠️ consolidationTimeoutMs is set to 60000ms, below the 180000ms default. Consolidation spawns a child agent turn and is routinely killed mid-run at lower values."
            )
        );

        config.consolidation_timeout_ms = DEFAULT_CONSOLIDATION_TIMEOUT_MS;
        assert!(config.consolidation_timeout_warning().is_none());
        config.consolidation_timeout_ms = 300_000;
        assert!(config.consolidation_timeout_warning().is_none());
    }
}
