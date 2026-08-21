use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use garde::Validate;
use pi_core::{ModelCost, ModelCostTier, ModelId, ModelInput, ModelSpec, ProviderId};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::ModelsPluginError;

const OPENAI_COMPLETIONS_API: &str = "openai-completions";

#[derive(Debug, Clone)]
pub(crate) struct PreparedProvider {
    pub id: ProviderId,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub runtime_api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub auth_header: bool,
    pub models: Vec<PreparedModel>,
    pub model_overrides: BTreeMap<ModelId, PreparedOverride>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedModel {
    pub id: ModelId,
    pub spec: ModelSpec,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedOverride {
    pub reasoning: Option<bool>,
    pub thinking_level_map: BTreeMap<String, Option<String>>,
    pub max_tokens: Option<u64>,
    pub sampling_params: BTreeMap<String, Value>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
struct ModelsFile {
    #[garde(custom(non_blank_map_keys), dive)]
    providers: BTreeMap<String, ProviderDefinition>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
struct ProviderDefinition {
    #[garde(length(min = 1), custom(optional_non_blank))]
    name: Option<String>,
    #[garde(length(min = 1), custom(optional_non_blank))]
    base_url: Option<String>,
    #[garde(length(min = 1), custom(optional_non_blank))]
    api_key: Option<String>,
    #[garde(length(min = 1), custom(optional_non_blank))]
    api: Option<String>,
    #[garde(length(min = 1), custom(optional_non_blank))]
    oauth: Option<String>,
    #[serde(default)]
    #[garde(custom(non_blank_map_keys))]
    headers: BTreeMap<String, String>,
    #[schemars(with = "Option<BTreeMap<String, Value>>")]
    #[garde(custom(optional_object_value))]
    compat: Option<Value>,
    #[serde(default)]
    auth_header: bool,
    #[serde(default)]
    #[garde(dive)]
    models: Vec<ModelDefinition>,
    #[serde(default)]
    #[garde(custom(non_blank_map_keys), dive)]
    model_overrides: BTreeMap<String, ModelOverride>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
struct ModelDefinition {
    #[garde(length(min = 1), custom(non_blank))]
    id: String,
    #[garde(length(min = 1), custom(optional_non_blank))]
    name: Option<String>,
    #[garde(length(min = 1), custom(optional_non_blank))]
    api: Option<String>,
    #[garde(length(min = 1), custom(optional_non_blank))]
    base_url: Option<String>,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    #[garde(custom(validate_thinking_level_map))]
    thinking_level_map: BTreeMap<String, Option<String>>,
    input: Option<Vec<ModelInputConfig>>,
    cost: Option<ModelCostConfig>,
    #[garde(range(min = 1))]
    context_window: Option<u64>,
    #[garde(range(min = 1))]
    max_tokens: Option<u64>,
    #[serde(default)]
    sampling_params: BTreeMap<String, Value>,
    #[serde(default)]
    #[garde(custom(non_blank_map_keys))]
    headers: BTreeMap<String, String>,
    #[schemars(with = "Option<BTreeMap<String, Value>>")]
    #[garde(custom(optional_object_value))]
    compat: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[garde(allow_unvalidated)]
struct ModelOverride {
    #[garde(length(min = 1), custom(optional_non_blank))]
    name: Option<String>,
    reasoning: Option<bool>,
    #[serde(default)]
    #[garde(custom(validate_thinking_level_map))]
    thinking_level_map: BTreeMap<String, Option<String>>,
    input: Option<Vec<ModelInputConfig>>,
    cost: Option<ModelCostOverride>,
    #[garde(range(min = 1))]
    context_window: Option<u64>,
    #[garde(range(min = 1))]
    max_tokens: Option<u64>,
    #[serde(default)]
    sampling_params: BTreeMap<String, Value>,
    #[serde(default)]
    #[garde(custom(non_blank_map_keys))]
    headers: BTreeMap<String, String>,
    #[schemars(with = "Option<BTreeMap<String, Value>>")]
    #[garde(custom(optional_object_value))]
    compat: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCostOverride {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    tiers: Option<Vec<ModelCostTierConfig>>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ModelInputConfig {
    Text,
    Image,
}

impl From<ModelInputConfig> for ModelInput {
    fn from(value: ModelInputConfig) -> Self {
        match value {
            ModelInputConfig::Text => Self::Text,
            ModelInputConfig::Image => Self::Image,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCostConfig {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    #[serde(default)]
    tiers: Vec<ModelCostTierConfig>,
}

impl From<ModelCostConfig> for ModelCost {
    fn from(value: ModelCostConfig) -> Self {
        Self {
            input: value.input,
            output: value.output,
            cache_read: value.cache_read,
            cache_write: value.cache_write,
            tiers: value.tiers.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCostTierConfig {
    input_tokens_above: u64,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

impl From<ModelCostTierConfig> for ModelCostTier {
    fn from(value: ModelCostTierConfig) -> Self {
        Self {
            input_tokens_above: value.input_tokens_above,
            input: value.input,
            output: value.output,
            cache_read: value.cache_read,
            cache_write: value.cache_write,
        }
    }
}

pub(crate) fn models_json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ModelsFile))
        .expect("a generated models.json schema must serialize")
}

pub(crate) fn load_models_file(
    path: &Path,
    runtime_api_keys: &BTreeMap<ProviderId, String>,
) -> Result<Vec<PreparedProvider>, ModelsPluginError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ModelsPluginError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let stripped = strip_json_comments(&content).map_err(|message| ModelsPluginError::Parse {
        path: path.to_path_buf(),
        message,
    })?;
    let parsed: ModelsFile =
        serde_json::from_str(&stripped).map_err(|error| ModelsPluginError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;

    parsed
        .compile(runtime_api_keys)
        .map_err(|message| ModelsPluginError::Invalid {
            path: path.to_path_buf(),
            message,
        })
}

impl ModelsFile {
    fn compile(
        self,
        runtime_api_keys: &BTreeMap<ProviderId, String>,
    ) -> Result<Vec<PreparedProvider>, String> {
        self.validate().map_err(|report| report.to_string())?;
        self.providers
            .into_iter()
            .map(|(id, provider)| provider.compile(id, runtime_api_keys))
            .collect()
    }
}

impl ProviderDefinition {
    fn compile(
        self,
        id: String,
        runtime_api_keys: &BTreeMap<ProviderId, String>,
    ) -> Result<PreparedProvider, String> {
        let provider = self;
        if let Some(oauth) = &provider.oauth {
            return Err(format!(
                "provider {id}: oauth {oauth:?} is not supported by pi-plugin-models yet"
            ));
        }
        if provider.models.is_empty()
            && provider.base_url.is_none()
            && provider.api_key.is_none()
            && provider.api.is_none()
            && provider.headers.is_empty()
            && provider.compat.is_none()
            && provider.model_overrides.is_empty()
            && !provider.auth_header
        {
            return Err(format!(
                "provider {id}: must configure baseUrl, apiKey, api, headers, modelOverrides, or models"
            ));
        }

        let provider_api = provider
            .api
            .as_deref()
            .map(|api| normalize_api(&id, None, api))
            .transpose()?;
        if provider.base_url.is_some() && provider_api.is_none() && provider.models.is_empty() {
            return Err(format!(
                "provider {id}: api is required when baseUrl configures a provider-wide route"
            ));
        }

        let provider_compat = provider.compat.clone();
        let mut seen = HashSet::new();
        let mut models = Vec::with_capacity(provider.models.len());
        for definition in provider.models {
            if !seen.insert(definition.id.clone()) {
                return Err(format!(
                    "provider {id}: duplicate model id {:?}",
                    definition.id
                ));
            }
            let model_override = provider.model_overrides.get(&definition.id);
            models.push(definition.compile(
                &id,
                model_override,
                provider_api.as_deref(),
                provider.base_url.as_deref(),
                provider_compat.as_ref(),
            )?);
        }

        let mut model_overrides = BTreeMap::new();
        for (model_id, value) in provider.model_overrides {
            if seen.contains(&model_id) {
                continue;
            }
            model_overrides.insert(
                ModelId::new(model_id),
                PreparedOverride {
                    reasoning: value.reasoning,
                    thinking_level_map: value.thinking_level_map,
                    max_tokens: value.max_tokens,
                    sampling_params: value.sampling_params,
                    headers: value.headers,
                },
            );
        }

        let provider_id = ProviderId::new(id);
        Ok(PreparedProvider {
            runtime_api_key: runtime_api_keys.get(&provider_id).cloned(),
            id: provider_id,
            api: provider_api,
            base_url: provider.base_url,
            api_key: provider.api_key,
            headers: provider.headers,
            auth_header: provider.auth_header,
            models,
            model_overrides,
        })
    }
}

impl ModelDefinition {
    fn compile(
        self,
        provider: &str,
        model_override: Option<&ModelOverride>,
        provider_api: Option<&str>,
        provider_base_url: Option<&str>,
        provider_compat: Option<&Value>,
    ) -> Result<PreparedModel, String> {
        let definition = self;
        let api = match definition.api.as_deref().or(provider_api) {
            Some(api) => normalize_api(provider, Some(&definition.id), api)?,
            None => {
                return Err(format!(
                    "provider {provider}, model {}: no api specified at model or provider level",
                    definition.id
                ));
            }
        };
        let base_url = definition
            .base_url
            .clone()
            .or_else(|| provider_base_url.map(str::to_string))
            .ok_or_else(|| {
                format!(
                    "provider {provider}, model {}: baseUrl is required for a custom model",
                    definition.id
                )
            })?;
        let context_window = definition.context_window.unwrap_or(128_000);
        let max_tokens = definition.max_tokens.unwrap_or(16_384);

        let mut spec = ModelSpec::new(
            provider,
            definition.id.as_str(),
            definition
                .name
                .clone()
                .unwrap_or_else(|| definition.id.clone()),
            api,
        );
        spec.base_url = Some(base_url);
        spec.reasoning = definition.reasoning;
        spec.thinking_level_map = definition.thinking_level_map;
        spec.input = definition.input.map_or_else(
            || vec![ModelInput::Text],
            |input| input.into_iter().map(Into::into).collect(),
        );
        spec.cost = definition.cost.map(Into::into).unwrap_or_default();
        spec.context_window = context_window;
        spec.max_tokens = max_tokens;
        spec.sampling_params = definition.sampling_params;
        spec.compat = merge_compat(provider_compat.cloned(), definition.compat);

        let mut headers = BTreeMap::new();
        if let Some(model_override) = model_override {
            apply_override(&mut spec, model_override);
            headers.extend(model_override.headers.clone());
        }
        // Pi gives the concrete model definition precedence over modelOverrides
        // for request headers.
        headers.extend(definition.headers);
        Ok(PreparedModel {
            id: spec.id.clone(),
            spec,
            headers,
        })
    }
}

fn apply_override(spec: &mut ModelSpec, value: &ModelOverride) {
    if let Some(name) = &value.name {
        spec.name.clone_from(name);
    }
    if let Some(reasoning) = value.reasoning {
        spec.reasoning = reasoning;
    }
    spec.thinking_level_map
        .extend(value.thinking_level_map.clone());
    if let Some(input) = &value.input {
        spec.input = input.iter().copied().map(Into::into).collect();
    }
    if let Some(cost) = &value.cost {
        if let Some(input) = cost.input {
            spec.cost.input = input;
        }
        if let Some(output) = cost.output {
            spec.cost.output = output;
        }
        if let Some(cache_read) = cost.cache_read {
            spec.cost.cache_read = cache_read;
        }
        if let Some(cache_write) = cost.cache_write {
            spec.cost.cache_write = cache_write;
        }
        if let Some(tiers) = &cost.tiers {
            spec.cost.tiers = tiers.iter().cloned().map(Into::into).collect();
        }
    }
    if let Some(context_window) = value.context_window {
        spec.context_window = context_window;
    }
    if let Some(max_tokens) = value.max_tokens {
        spec.max_tokens = max_tokens;
    }
    spec.sampling_params.extend(value.sampling_params.clone());
    spec.compat = merge_compat(spec.compat.take(), value.compat.clone());
}

fn normalize_api(provider: &str, model: Option<&str>, api: &str) -> Result<String, String> {
    let canonical = match api {
        "openai-completions" | "openai-chat-completions" | "openai-compatible" => {
            OPENAI_COMPLETIONS_API
        }
        other => {
            let target = model.map_or_else(
                || format!("provider {provider}"),
                |model| format!("provider {provider}, model {model}"),
            );
            return Err(format!(
                "{target}: unsupported api {other:?}; this build supports openai-completions"
            ));
        }
    };
    Ok(canonical.to_string())
}

fn non_blank(value: &str, _: &()) -> garde::Result {
    if value.trim().is_empty() {
        Err(garde::Error::new("must contain a non-whitespace character"))
    } else {
        Ok(())
    }
}

fn optional_non_blank(value: &Option<String>, context: &()) -> garde::Result {
    value
        .as_deref()
        .map_or(Ok(()), |value| non_blank(value, context))
}

fn non_blank_map_keys<V>(values: &BTreeMap<String, V>, _: &()) -> garde::Result {
    if let Some(key) = values.keys().find(|key| key.trim().is_empty()) {
        return Err(garde::Error::new(format!(
            "map key {key:?} must contain a non-whitespace character"
        )));
    }
    Ok(())
}

fn validate_thinking_level_map(map: &BTreeMap<String, Option<String>>, _: &()) -> garde::Result {
    const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    if let Some(level) = map.keys().find(|level| !LEVELS.contains(&level.as_str())) {
        return Err(garde::Error::new(format!(
            "unknown thinking level {level:?}"
        )));
    }
    Ok(())
}

fn optional_object_value(value: &Option<Value>, _: &()) -> garde::Result {
    match value {
        Some(value) if !value.is_object() => Err(garde::Error::new("must be an object")),
        _ => Ok(()),
    }
}

fn merge_compat(base: Option<Value>, overlay: Option<Value>) -> Option<Value> {
    match (base, overlay) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value),
        (Some(Value::Object(mut base)), Some(Value::Object(overlay))) => {
            merge_objects(&mut base, overlay);
            Some(Value::Object(base))
        }
        (_, overlay) => overlay,
    }
}

fn merge_objects(base: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(Value::Object(base)), Value::Object(overlay)) => merge_objects(base, overlay),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn strip_json_comments(input: &str) -> Result<String, String> {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        String,
        LineComment,
        BlockComment,
    }

    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut state = State::Normal;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        match state {
            State::Normal if character == '"' => {
                output.push(character);
                state = State::String;
            }
            State::Normal if character == '/' && chars.peek() == Some(&'/') => {
                chars.next();
                output.push_str("  ");
                state = State::LineComment;
            }
            State::Normal if character == '/' && chars.peek() == Some(&'*') => {
                chars.next();
                output.push_str("  ");
                state = State::BlockComment;
            }
            State::Normal => output.push(character),
            State::String => {
                output.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    state = State::Normal;
                }
            }
            State::LineComment if character == '\n' => {
                output.push('\n');
                state = State::Normal;
            }
            State::LineComment => output.push(' '),
            State::BlockComment if character == '*' && chars.peek() == Some(&'/') => {
                chars.next();
                output.push_str("  ");
                state = State::Normal;
            }
            State::BlockComment if character == '\n' => output.push('\n'),
            State::BlockComment => output.push(' '),
        }
    }
    if matches!(state, State::BlockComment) {
        return Err("unterminated block comment".to_string());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_comments_do_not_touch_urls_or_string_content() {
        let input = r#"{
          // comment
          "url": "https://example.test/a//b",
          "literal": "/* still text */" /* comment */
        }"#;
        let stripped = strip_json_comments(input).unwrap();
        let value: Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["url"], "https://example.test/a//b");
        assert_eq!(value["literal"], "/* still text */");
    }

    #[test]
    fn generated_schema_is_strict_and_carries_derived_constraints() {
        let schema = models_json_schema();
        let encoded = serde_json::to_string(&schema).unwrap();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert!(
            schema["required"]
                .as_array()
                .is_some_and(|required| { required.iter().any(|field| field == "providers") })
        );
        assert!(encoded.contains("\"minimum\":1"));
        assert!(encoded.contains("\"additionalProperties\":false"));
    }

    #[test]
    fn derived_validation_reports_nested_fields_before_compilation() {
        let parsed: ModelsFile = serde_json::from_str(
            r#"{
              "providers": {
                "custom": {
                  "baseUrl": "https://example.test",
                  "api": "openai-completions",
                  "models": [{
                    "id": " ",
                    "contextWindow": 0,
                    "thinkingLevelMap": { "unknown": "high" }
                  }]
                }
              }
            }"#,
        )
        .unwrap();

        let error = parsed.compile(&BTreeMap::new()).unwrap_err();
        assert!(error.contains("models[0].id"), "{error}");
        assert!(error.contains("context_window"), "{error}");
        assert!(error.contains("thinking_level_map"), "{error}");
    }

    #[test]
    fn compilation_converts_config_types_into_runtime_model_metadata() {
        let parsed: ModelsFile = serde_json::from_str(
            r#"{
              "providers": {
                "custom": {
                  "baseUrl": "https://example.test",
                  "api": "openai-completions",
                  "models": [{
                    "id": "vision",
                    "input": ["text", "image"],
                    "cost": {
                      "input": 1.0,
                      "output": 2.0,
                      "cacheRead": 0.5,
                      "cacheWrite": 0.75,
                      "tiers": [{
                        "inputTokensAbove": 100000,
                        "input": 3.0,
                        "output": 4.0,
                        "cacheRead": 1.0,
                        "cacheWrite": 1.5
                      }]
                    }
                  }]
                }
              }
            }"#,
        )
        .unwrap();

        let providers = parsed.compile(&BTreeMap::new()).unwrap();
        let model = &providers[0].models[0].spec;
        assert_eq!(model.input, vec![ModelInput::Text, ModelInput::Image]);
        assert_eq!(model.cost.input, 1.0);
        assert_eq!(model.cost.tiers[0].input_tokens_above, 100_000);
    }
}
