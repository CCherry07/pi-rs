//! Model-specific compaction token resolution, after scope/trust deep merging.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::CompactionSettings;

/// A token setting is validated when it is used, matching Pi's settings getters.
/// An invalid entry for another model must not prevent the selected model running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionTokenSettings {
    ordinary: TokenFields,
    models: BTreeMap<String, Result<TokenFields, CompactionSettingError>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TokenFields {
    reserve: Option<Result<u64, CompactionSettingError>>,
    keep_recent: Option<Result<u64, CompactionSettingError>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CompactionSettingError(String);

impl CompactionTokenSettings {
    pub(crate) fn decode(document: &Map<String, Value>) -> Self {
        let Some(compaction) = document.get("compaction").and_then(Value::as_object) else {
            return Self::default();
        };
        Self {
            ordinary: TokenFields::decode(compaction, "compaction"),
            models: compaction
                .get("modelOverrides")
                .and_then(Value::as_object)
                .map(|models| {
                    models
                        .iter()
                        .map(|(key, value)| {
                            let path = format!("compaction.modelOverrides[\"{key}\"]");
                            let fields = value
                                .as_object()
                                .map(|fields| TokenFields::decode(fields, &path))
                                .ok_or_else(|| {
                                    CompactionSettingError(format!(
                                        "Invalid {path} setting: {}. Expected an object.",
                                        setting_string(value)
                                    ))
                                });
                            (key.clone(), fields)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Resolve each field independently: exact provider/model override, ordinary
    /// setting, then the caller's base. `enabled` is never model-specific.
    pub fn resolve(
        &self,
        base: CompactionSettings,
        model: Option<(&str, &str)>,
    ) -> Result<CompactionSettings, CompactionSettingError> {
        Ok(CompactionSettings {
            enabled: base.enabled,
            reserve_tokens: self.reserve_tokens(base.reserve_tokens, model)?,
            keep_recent_tokens: self.keep_recent_tokens(base.keep_recent_tokens, model)?,
        })
    }

    pub fn reserve_tokens(
        &self,
        base: u64,
        model: Option<(&str, &str)>,
    ) -> Result<u64, CompactionSettingError> {
        self.resolve_field(base, model, |fields| &fields.reserve)
    }

    pub fn keep_recent_tokens(
        &self,
        base: u64,
        model: Option<(&str, &str)>,
    ) -> Result<u64, CompactionSettingError> {
        self.resolve_field(base, model, |fields| &fields.keep_recent)
    }

    fn resolve_field(
        &self,
        base: u64,
        model: Option<(&str, &str)>,
        field: impl Fn(&TokenFields) -> &Option<Result<u64, CompactionSettingError>>,
    ) -> Result<u64, CompactionSettingError> {
        // Validate ordinary values even when a valid model override exists.
        let ordinary = field(&self.ordinary).clone().transpose()?;
        let entry = model
            .and_then(|(provider, id)| self.models.get(&format!("{provider}/{id}")))
            .map(|entry| entry.as_ref().map_err(Clone::clone))
            .transpose()?;
        let model_value = entry
            .and_then(|entry| field(entry).as_ref())
            .cloned()
            .transpose()?;
        Ok(model_value.or(ordinary).unwrap_or(base))
    }
}

impl TokenFields {
    fn decode(object: &Map<String, Value>, path: &str) -> Self {
        Self {
            reserve: object
                .get("reserveTokens")
                .map(|value| token_value(value, &format!("{path}.reserveTokens"))),
            keep_recent: object
                .get("keepRecentTokens")
                .map(|value| token_value(value, &format!("{path}.keepRecentTokens"))),
        }
    }
}

fn token_value(value: &Value, path: &str) -> Result<u64, CompactionSettingError> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    let integer = value
        .as_u64()
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .or_else(|| {
            let number = value.as_f64()?;
            (number >= 0.0 && number <= MAX_SAFE_INTEGER as f64 && number.fract() == 0.0)
                .then_some(number as u64)
        });
    integer.ok_or_else(|| {
        CompactionSettingError(format!(
            "Invalid {path} setting: {}. Expected a non-negative safe integer.",
            setting_string(value)
        ))
    })
}

/// JSON-representable JavaScript String(value), for Pi-compatible diagnostics.
fn setting_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Null => String::new(),
                value => setting_string(value),
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}
