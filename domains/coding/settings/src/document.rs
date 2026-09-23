use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{
    BranchSummarySettings, CompactionSettings, DefaultProjectTrust, ImageSettings, PackageSource,
    ProviderRetrySettings, QueueModeSetting, RetrySettings, SettingsDiagnostic,
    SettingsDiagnosticKind, SettingsError, SettingsScope, SettingsValues, ThinkingBudgetsSettings,
    ThinkingLevelSetting, TransportSetting,
};

pub(crate) fn read(path: &Path) -> Result<Map<String, Value>, SettingsError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(source) => {
            return Err(SettingsError::Io {
                operation: "read",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value: Value =
        serde_json::from_str(&content).map_err(|error| SettingsError::InvalidDocument {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| SettingsError::InvalidDocument {
            path: path.to_path_buf(),
            message: "top-level value must be an object".to_string(),
        })
}

pub(crate) fn deep_merge(
    base: &Map<String, Value>,
    overrides: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        let value = match (base.get(key), value) {
            (Some(Value::Object(base)), Value::Object(overrides)) => {
                Value::Object(deep_merge(base, overrides))
            }
            _ => value.clone(),
        };
        merged.insert(key.clone(), value);
    }
    merged
}

pub(crate) fn decode_values(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> SettingsValues {
    SettingsValues {
        default_provider: optional_string(document, "defaultProvider", origin, diagnostics),
        default_model: optional_string(document, "defaultModel", origin, diagnostics),
        default_thinking_level: optional_enum(
            document,
            "defaultThinkingLevel",
            origin,
            diagnostics,
            |value| match value {
                "off" => Some(ThinkingLevelSetting::Off),
                "minimal" => Some(ThinkingLevelSetting::Minimal),
                "low" => Some(ThinkingLevelSetting::Low),
                "medium" => Some(ThinkingLevelSetting::Medium),
                "high" => Some(ThinkingLevelSetting::High),
                "xhigh" => Some(ThinkingLevelSetting::XHigh),
                "max" => Some(ThinkingLevelSetting::Max),
                _ => None,
            },
        ),
        transport: optional_enum(
            document,
            "transport",
            origin,
            diagnostics,
            |value| match value {
                "sse" => Some(TransportSetting::Sse),
                "websocket" => Some(TransportSetting::Websocket),
                "websocket-cached" => Some(TransportSetting::WebsocketCached),
                "auto" => Some(TransportSetting::Auto),
                _ => None,
            },
        )
        .unwrap_or_default(),
        steering_mode: queue_mode(document, "steeringMode", origin, diagnostics),
        follow_up_mode: queue_mode(document, "followUpMode", origin, diagnostics),
        compaction: decode_compaction(document, origin, diagnostics),
        branch_summary: decode_branch_summary(document, origin, diagnostics),
        retry: decode_retry(document, origin, diagnostics),
        default_project_trust: optional_enum(
            document,
            "defaultProjectTrust",
            origin,
            diagnostics,
            |value| match value {
                "ask" => Some(DefaultProjectTrust::Ask),
                "always" => Some(DefaultProjectTrust::Always),
                "never" => Some(DefaultProjectTrust::Never),
                _ => None,
            },
        )
        .unwrap_or_default(),
        shell_path: optional_string(document, "shellPath", origin, diagnostics),
        shell_command_prefix: optional_string(document, "shellCommandPrefix", origin, diagnostics),
        npm_command: optional_string_list(document, "npmCommand", origin, diagnostics),
        packages: package_list(document, origin, diagnostics),
        extensions: string_list(document, "extensions", origin, diagnostics),
        skills: string_list(document, "skills", origin, diagnostics),
        prompts: string_list(document, "prompts", origin, diagnostics),
        themes: string_list(document, "themes", origin, diagnostics),
        enable_skill_commands: optional_bool(document, "enableSkillCommands", origin, diagnostics)
            .unwrap_or(true),
        enabled_models: optional_string_list(document, "enabledModels", origin, diagnostics),
        default_tools: optional_string_list(document, "defaultTools", origin, diagnostics),
        thinking_budgets: decode_thinking_budgets(document, origin, diagnostics),
        session_dir: optional_string(document, "sessionDir", origin, diagnostics),
        http_proxy: optional_string(document, "httpProxy", origin, diagnostics),
        http_idle_timeout_ms: timeout_ms(
            document,
            "httpIdleTimeoutMs",
            300_000,
            origin,
            diagnostics,
        ),
        websocket_connect_timeout_ms: optional_timeout_ms(
            document,
            "websocketConnectTimeoutMs",
            origin,
            diagnostics,
        ),
        images: decode_images(document, origin, diagnostics),
    }
}

fn decode_compaction(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> CompactionSettings {
    let defaults = CompactionSettings::default();
    let Some(object) = optional_object(document, "compaction", origin, diagnostics) else {
        return defaults;
    };
    CompactionSettings {
        enabled: optional_bool(object, "enabled", origin, diagnostics).unwrap_or(defaults.enabled),
        reserve_tokens: optional_u64(object, "reserveTokens", origin, diagnostics)
            .unwrap_or(defaults.reserve_tokens),
        keep_recent_tokens: optional_u64(object, "keepRecentTokens", origin, diagnostics)
            .unwrap_or(defaults.keep_recent_tokens),
    }
}

fn decode_branch_summary(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> BranchSummarySettings {
    let defaults = BranchSummarySettings::default();
    let Some(object) = optional_object(document, "branchSummary", origin, diagnostics) else {
        return defaults;
    };
    BranchSummarySettings {
        reserve_tokens: optional_u64(object, "reserveTokens", origin, diagnostics)
            .unwrap_or(defaults.reserve_tokens),
        skip_prompt: optional_bool(object, "skipPrompt", origin, diagnostics)
            .unwrap_or(defaults.skip_prompt),
    }
}

fn decode_retry(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> RetrySettings {
    let defaults = RetrySettings::default();
    let Some(object) = optional_object(document, "retry", origin, diagnostics) else {
        return defaults;
    };
    let provider = optional_object(object, "provider", origin, diagnostics).map_or(
        defaults.provider,
        |provider| ProviderRetrySettings {
            timeout_ms: optional_u64(provider, "timeoutMs", origin, diagnostics),
            max_retries: optional_u32(provider, "maxRetries", origin, diagnostics),
            max_retry_delay_ms: optional_u64(provider, "maxRetryDelayMs", origin, diagnostics)
                .unwrap_or(defaults.provider.max_retry_delay_ms),
        },
    );
    RetrySettings {
        enabled: optional_bool(object, "enabled", origin, diagnostics).unwrap_or(defaults.enabled),
        max_retries: optional_u32(object, "maxRetries", origin, diagnostics)
            .unwrap_or(defaults.max_retries),
        base_delay_ms: optional_u64(object, "baseDelayMs", origin, diagnostics)
            .unwrap_or(defaults.base_delay_ms),
        provider,
    }
}

fn decode_thinking_budgets(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<ThinkingBudgetsSettings> {
    optional_object(document, "thinkingBudgets", origin, diagnostics).map(|object| {
        ThinkingBudgetsSettings {
            minimal: optional_u64(object, "minimal", origin, diagnostics),
            low: optional_u64(object, "low", origin, diagnostics),
            medium: optional_u64(object, "medium", origin, diagnostics),
            high: optional_u64(object, "high", origin, diagnostics),
        }
    })
}

fn decode_images(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> ImageSettings {
    let defaults = ImageSettings::default();
    let Some(object) = optional_object(document, "images", origin, diagnostics) else {
        return defaults;
    };
    ImageSettings {
        auto_resize: optional_bool(object, "autoResize", origin, diagnostics)
            .unwrap_or(defaults.auto_resize),
        block_images: optional_bool(object, "blockImages", origin, diagnostics)
            .unwrap_or(defaults.block_images),
    }
}

fn queue_mode(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> QueueModeSetting {
    optional_enum(document, key, origin, diagnostics, |value| match value {
        "all" => Some(QueueModeSetting::All),
        "one-at-a-time" => Some(QueueModeSetting::OneAtATime),
        _ => None,
    })
    .unwrap_or_default()
}

fn package_list(
    document: &Map<String, Value>,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Vec<PackageSource> {
    let Some(value) = document.get("packages") else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        invalid(origin, "packages must be an array", diagnostics);
        return Vec::new();
    };
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, value)| match decode::<PackageSource>(value) {
            Some(package) if !package.source().trim().is_empty() => Some(package),
            _ => {
                invalid(
                    origin,
                    format!("packages[{index}] must be a non-empty package source"),
                    diagnostics,
                );
                None
            }
        })
        .collect()
}

fn string_list(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Vec<String> {
    optional_string_list(document, key, origin, diagnostics).unwrap_or_default()
}

fn optional_string_list(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<Vec<String>> {
    let value = document.get(key)?;
    let Some(entries) = value.as_array() else {
        invalid(
            origin,
            format!("{key} must be an array of strings"),
            diagnostics,
        );
        return None;
    };
    let mut strings = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(entry) = entry.as_str() else {
            invalid(
                origin,
                format!("{key} must contain only strings"),
                diagnostics,
            );
            return None;
        };
        strings.push(entry.to_string());
    }
    Some(strings)
}

fn optional_string(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<String> {
    let value = document.get(key)?;
    match value.as_str() {
        Some(value) => Some(value.to_string()),
        None => {
            invalid(origin, format!("{key} must be a string"), diagnostics);
            None
        }
    }
}

fn optional_bool(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<bool> {
    let value = document.get(key)?;
    match value.as_bool() {
        Some(value) => Some(value),
        None => {
            invalid(origin, format!("{key} must be a boolean"), diagnostics);
            None
        }
    }
}

fn optional_u64(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<u64> {
    let value = document.get(key)?;
    match value.as_u64() {
        Some(value) => Some(value),
        None => {
            invalid(
                origin,
                format!("{key} must be a non-negative integer"),
                diagnostics,
            );
            None
        }
    }
}

fn optional_u32(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<u32> {
    optional_u64(document, key, origin, diagnostics).and_then(|value| {
        u32::try_from(value).ok().or_else(|| {
            invalid(
                origin,
                format!("{key} exceeds the supported integer range"),
                diagnostics,
            );
            None
        })
    })
}

fn optional_object<'a>(
    document: &'a Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<&'a Map<String, Value>> {
    let value = document.get(key)?;
    match value.as_object() {
        Some(value) => Some(value),
        None => {
            invalid(origin, format!("{key} must be an object"), diagnostics);
            None
        }
    }
}

fn optional_enum<T>(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Option<T> {
    let value = document.get(key)?;
    let Some(value) = value.as_str() else {
        invalid(origin, format!("{key} must be a string"), diagnostics);
        return None;
    };
    let parsed = parse(value);
    if parsed.is_none() {
        invalid(
            origin,
            format!("unsupported {key} value {value:?}"),
            diagnostics,
        );
    }
    parsed
}

fn timeout_ms(
    document: &Map<String, Value>,
    key: &str,
    default: u64,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> u64 {
    optional_timeout_ms(document, key, origin, diagnostics).unwrap_or(default)
}

fn optional_timeout_ms(
    document: &Map<String, Value>,
    key: &str,
    origin: Option<(SettingsScope, &Path)>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) -> Option<u64> {
    let value = document.get(key)?;
    if value
        .as_str()
        .is_some_and(|value| value.eq_ignore_ascii_case("disabled"))
    {
        return Some(0);
    }
    if let Some(value) = value.as_u64() {
        return Some(value);
    }
    if let Some(value) = value.as_f64().and_then(normalize_timeout_number) {
        return Some(value);
    }
    if let Some(value) = value
        .as_str()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .and_then(normalize_timeout_number)
    {
        return Some(value);
    }
    invalid(
        origin,
        format!("{key} must be a non-negative integer or \"disabled\""),
        diagnostics,
    );
    None
}

fn normalize_timeout_number(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value <= u64::MAX as f64).then(|| value.floor() as u64)
}

fn decode<T: DeserializeOwned>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

fn invalid(
    origin: Option<(SettingsScope, &Path)>,
    message: impl Into<String>,
    diagnostics: &mut Vec<SettingsDiagnostic>,
) {
    let Some((scope, path)) = origin else {
        return;
    };
    diagnostics.push(SettingsDiagnostic {
        scope,
        path: path.to_path_buf(),
        kind: SettingsDiagnosticKind::InvalidValue,
        message: message.into(),
    });
}
