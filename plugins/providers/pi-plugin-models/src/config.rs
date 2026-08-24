use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use garde::Validate;
use pi_core::{ModelCost, ModelCostTier, ModelId, ModelInput, ModelSpec, ProviderId};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::ModelsPluginError;

const OPENAI_COMPLETIONS_API: &str = "openai-completions";
const OPENAI_RESPONSES_API: &str = "openai-responses";
const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";
const GOOGLE_GENERATIVE_AI_API: &str = "google-generative-ai";

#[derive(Debug, Clone)]
pub(crate) struct PreparedProvider {
    pub id: ProviderId,
    pub name: Option<String>,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub compat: Option<Value>,
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
    pub name: Option<String>,
    pub reasoning: Option<bool>,
    pub thinking_level_map: BTreeMap<String, Option<String>>,
    pub input: Option<Vec<ModelInput>>,
    pub cost: Option<PreparedCostOverride>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub sampling_params: BTreeMap<String, Value>,
    pub headers: BTreeMap<String, String>,
    pub compat: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedCostOverride {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub tiers: Option<Vec<ModelCostTier>>,
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
    oauth: Option<ModelsOAuthConfig>,
    #[garde(custom(optional_non_blank_map_keys))]
    headers: Option<BTreeMap<String, String>>,
    #[schemars(with = "Option<ProviderCompatSchema>")]
    #[garde(custom(optional_object_value))]
    compat: Option<Value>,
    auth_header: Option<bool>,
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
    #[schemars(with = "Option<ProviderCompatSchema>")]
    #[garde(custom(optional_object_value))]
    compat: Option<Value>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ModelsOAuthConfig {
    Radius,
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
    #[schemars(with = "Option<ProviderCompatSchema>")]
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

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum ProviderCompatSchema {
    OpenAiCompletions(Box<OpenAiCompletionsCompatSchema>),
    OpenAiResponses(OpenAiResponsesCompatSchema),
    AnthropicMessages(AnthropicMessagesCompatSchema),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenAiCompletionsCompatSchema {
    supports_store: Option<bool>,
    supports_developer_role: Option<bool>,
    supports_reasoning_effort: Option<bool>,
    supports_usage_in_streaming: Option<bool>,
    supports_finish_reason: Option<bool>,
    max_tokens_field: Option<MaxTokensFieldSchema>,
    requires_tool_result_name: Option<bool>,
    requires_assistant_after_tool_result: Option<bool>,
    requires_thinking_as_text: Option<bool>,
    requires_reasoning_content_on_assistant_messages: Option<bool>,
    thinking_format: Option<ThinkingFormatSchema>,
    chat_template_kwargs: Option<BTreeMap<String, ChatTemplateKwargSchema>>,
    chat_template_args: Option<BTreeMap<String, ChatTemplateKwargSchema>>,
    cache_control_format: Option<CacheControlFormatSchema>,
    open_router_routing: Option<OpenRouterRoutingSchema>,
    vercel_gateway_routing: Option<VercelGatewayRoutingSchema>,
    zai_tool_stream: Option<bool>,
    supports_thinking_token_budget: Option<bool>,
    thinking_token_budget_field: Option<ThinkingTokenBudgetFieldSchema>,
    #[serde(rename = "supportsOpenAIGrammarTools")]
    #[schemars(rename = "supportsOpenAIGrammarTools")]
    supports_open_ai_grammar_tools: Option<bool>,
    supports_strict_mode: Option<bool>,
    send_session_affinity_headers: Option<bool>,
    deferred_tools_mode: Option<DeferredToolsModeSchema>,
    session_affinity_format: Option<SessionAffinityFormatSchema>,
    supports_long_cache_retention: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenAiResponsesCompatSchema {
    supports_developer_role: Option<bool>,
    session_affinity_format: Option<SessionAffinityFormatSchema>,
    supports_long_cache_retention: Option<bool>,
    supports_strict_mode: Option<bool>,
    #[serde(rename = "supportsOpenAIGrammarTools")]
    #[schemars(rename = "supportsOpenAIGrammarTools")]
    supports_open_ai_grammar_tools: Option<bool>,
    supports_additional_tools: Option<bool>,
    supports_tool_search: Option<bool>,
    supports_explicit_prompt_cache_mode: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AnthropicMessagesCompatSchema {
    supports_eager_tool_input_streaming: Option<bool>,
    supports_long_cache_retention: Option<bool>,
    send_session_affinity_headers: Option<bool>,
    supports_cache_control_on_tools: Option<bool>,
    supports_temperature: Option<bool>,
    force_adaptive_thinking: Option<bool>,
    allow_empty_signature: Option<bool>,
    supports_strict_tools: Option<bool>,
    allowed_fallback_models: Option<Vec<String>>,
    supports_tool_references: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum MaxTokensFieldSchema {
    MaxCompletionTokens,
    MaxTokens,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum ThinkingFormatSchema {
    Openai,
    Openrouter,
    Together,
    Baseten,
    Deepseek,
    Zai,
    Qwen,
    ChatTemplate,
    QwenChatTemplate,
    StringThinking,
    AntLing,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum CacheControlFormatSchema {
    Anthropic,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum DeferredToolsModeSchema {
    Kimi,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum SessionAffinityFormatSchema {
    Openai,
    OpenaiNosession,
    Openrouter,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
enum ThinkingTokenBudgetFieldSchema {
    #[serde(rename = "thinking_token_budget")]
    #[schemars(rename = "thinking_token_budget")]
    TokenBudget,
    #[serde(rename = "thinking_budget")]
    #[schemars(rename = "thinking_budget")]
    Budget,
    #[serde(rename = "thinking_budget_tokens")]
    #[schemars(rename = "thinking_budget_tokens")]
    BudgetTokens,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum ChatTemplateKwargSchema {
    String(String),
    Number(f64),
    Boolean(bool),
    Null(()),
    Variable(ChatTemplateVariableSchema),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatTemplateVariableSchema {
    #[serde(rename = "$var")]
    #[schemars(rename = "$var")]
    variable: ChatTemplateVariableNameSchema,
    omit_when_off: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
enum ChatTemplateVariableNameSchema {
    #[serde(rename = "thinking.enabled")]
    #[schemars(rename = "thinking.enabled")]
    Enabled,
    #[serde(rename = "thinking.effort")]
    #[schemars(rename = "thinking.effort")]
    Effort,
    #[serde(rename = "thinking.budget")]
    #[schemars(rename = "thinking.budget")]
    Budget,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VercelGatewayRoutingSchema {
    only: Option<Vec<String>>,
    order: Option<Vec<String>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenRouterRoutingSchema {
    allow_fallbacks: Option<bool>,
    require_parameters: Option<bool>,
    data_collection: Option<DataCollectionSchema>,
    zdr: Option<bool>,
    enforce_distillable_text: Option<bool>,
    order: Option<Vec<String>>,
    only: Option<Vec<String>>,
    ignore: Option<Vec<String>>,
    quantizations: Option<Vec<String>>,
    sort: Option<OpenRouterSortSchema>,
    max_price: Option<OpenRouterMaxPriceSchema>,
    preferred_min_throughput: Option<NumberOrPercentilesSchema>,
    preferred_max_latency: Option<NumberOrPercentilesSchema>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum DataCollectionSchema {
    Deny,
    Allow,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum OpenRouterSortSchema {
    String(String),
    Object(OpenRouterSortObjectSchema),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenRouterSortObjectSchema {
    by: Option<String>,
    partition: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenRouterMaxPriceSchema {
    prompt: Option<NumberOrStringSchema>,
    completion: Option<NumberOrStringSchema>,
    image: Option<NumberOrStringSchema>,
    audio: Option<NumberOrStringSchema>,
    request: Option<NumberOrStringSchema>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum NumberOrStringSchema {
    Number(f64),
    String(String),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum NumberOrPercentilesSchema {
    Number(f64),
    Percentiles(PercentileCutoffsSchema),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PercentileCutoffsSchema {
    p50: Option<f64>,
    p75: Option<f64>,
    p90: Option<f64>,
    p99: Option<f64>,
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

impl From<ModelCostOverride> for PreparedCostOverride {
    fn from(value: ModelCostOverride) -> Self {
        Self {
            input: value.input,
            output: value.output,
            cache_read: value.cache_read,
            cache_write: value.cache_write,
            tiers: value
                .tiers
                .map(|tiers| tiers.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<ModelOverride> for PreparedOverride {
    fn from(value: ModelOverride) -> Self {
        Self {
            name: value.name,
            reasoning: value.reasoning,
            thinking_level_map: value.thinking_level_map,
            input: value
                .input
                .map(|input| input.into_iter().map(Into::into).collect()),
            cost: value.cost.map(Into::into),
            context_window: value.context_window,
            max_tokens: value.max_tokens,
            sampling_params: value.sampling_params,
            headers: value.headers,
            compat: value.compat,
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

impl PreparedProvider {
    /// Composes one models.json provider over model metadata registered by
    /// earlier provider plugins, matching Pi's built-in/custom upsert order.
    pub(crate) fn compose_with_base(&self, base_models: &[ModelSpec]) -> Result<Self, String> {
        let mut models = base_models
            .iter()
            .cloned()
            .map(|mut spec| {
                if let Some(base_url) = &self.base_url {
                    spec.base_url = Some(base_url.clone());
                }
                spec.compat = merge_compat(spec.compat.take(), self.compat.clone());
                PreparedModel {
                    id: spec.id.clone(),
                    spec,
                    headers: BTreeMap::new(),
                }
            })
            .collect::<Vec<_>>();

        for configured in &self.models {
            let existing_index = models.iter().position(|model| model.id == configured.id);
            let defaults = existing_index
                .and_then(|index| models.get(index))
                .or_else(|| models.first());
            let mut model = configured.clone();
            if model.spec.api.is_empty() {
                model.spec.api = self
                    .api
                    .clone()
                    .or_else(|| defaults.map(|model| model.spec.api.clone()))
                    .ok_or_else(|| {
                        format!(
                            "provider {}, model {}: no api specified at model or provider level",
                            self.id, model.id
                        )
                    })?;
            }
            if model.spec.base_url.is_none() {
                model.spec.base_url = self
                    .base_url
                    .clone()
                    .or_else(|| defaults.and_then(|model| model.spec.base_url.clone()));
            }
            if model.spec.base_url.is_none() {
                return Err(format!(
                    "provider {}, model {}: baseUrl is required for a custom model",
                    self.id, model.id
                ));
            }
            match existing_index {
                Some(index) => models[index] = model,
                None => models.push(model),
            }
        }

        for model in &mut models {
            let Some(model_override) = self.model_overrides.get(&model.id) else {
                continue;
            };
            apply_override(&mut model.spec, model_override);
            let mut headers = model_override.headers.clone();
            // Pi gives the concrete custom model definition precedence over
            // modelOverrides for request headers.
            headers.extend(std::mem::take(&mut model.headers));
            model.headers = headers;
        }
        for model in &models {
            validate_compat(&self.id, &model.spec)?;
        }

        let mut composed = self.clone();
        composed.models = models;
        Ok(composed)
    }
}

fn validate_compat(provider: &ProviderId, model: &ModelSpec) -> Result<(), String> {
    let Some(compat) = &model.compat else {
        return Ok(());
    };
    let result = match model.api.as_str() {
        OPENAI_COMPLETIONS_API => {
            serde_json::from_value::<OpenAiCompletionsCompatSchema>(compat.clone()).map(|_| ())
        }
        OPENAI_RESPONSES_API => {
            serde_json::from_value::<OpenAiResponsesCompatSchema>(compat.clone()).map(|_| ())
        }
        ANTHROPIC_MESSAGES_API => {
            serde_json::from_value::<AnthropicMessagesCompatSchema>(compat.clone()).map(|_| ())
        }
        _ => Ok(()),
    };
    result.map_err(|error| {
        format!(
            "provider {provider}, model {}: invalid compat for API {:?}: {error}",
            model.id, model.api
        )
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
        if provider.oauth.is_some() {
            return Err(format!(
                "provider {id}: oauth \"radius\" is not supported by pi-plugin-models yet"
            ));
        }
        if provider.models.is_empty()
            && provider.base_url.is_none()
            && provider.api_key.is_none()
            && provider.api.is_none()
            && provider.headers.is_none()
            && provider.compat.is_none()
            && provider.model_overrides.is_empty()
            && provider.auth_header.is_none()
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
            models.push(definition.compile(
                &id,
                provider_api.as_deref(),
                provider.base_url.as_deref(),
                provider_compat.as_ref(),
            )?);
        }

        let model_overrides = provider
            .model_overrides
            .into_iter()
            .map(|(model_id, value)| (ModelId::new(model_id), value.into()))
            .collect();

        let provider_id = ProviderId::new(id);
        Ok(PreparedProvider {
            runtime_api_key: runtime_api_keys.get(&provider_id).cloned(),
            id: provider_id,
            name: provider.name,
            api: provider_api,
            base_url: provider.base_url,
            compat: provider_compat,
            api_key: provider.api_key,
            headers: provider.headers.unwrap_or_default(),
            auth_header: provider.auth_header.unwrap_or(false),
            models,
            model_overrides,
        })
    }
}

impl ModelDefinition {
    fn compile(
        self,
        provider: &str,
        provider_api: Option<&str>,
        provider_base_url: Option<&str>,
        provider_compat: Option<&Value>,
    ) -> Result<PreparedModel, String> {
        let definition = self;
        let api = match definition.api.as_deref().or(provider_api) {
            Some(api) => normalize_api(provider, Some(&definition.id), api)?,
            // A custom model under a built-in provider inherits the API from
            // the replaced model (or the provider's first model) during
            // generation registration, when that catalog is available.
            None => String::new(),
        };
        let base_url = definition
            .base_url
            .clone()
            .or_else(|| provider_base_url.map(str::to_string));
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
        spec.base_url = base_url;
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

        let headers = definition.headers;
        Ok(PreparedModel {
            id: spec.id.clone(),
            spec,
            headers,
        })
    }
}

pub(crate) fn apply_override(spec: &mut ModelSpec, value: &PreparedOverride) {
    if let Some(name) = &value.name {
        spec.name.clone_from(name);
    }
    if let Some(reasoning) = value.reasoning {
        spec.reasoning = reasoning;
    }
    spec.thinking_level_map
        .extend(value.thinking_level_map.clone());
    if let Some(input) = &value.input {
        spec.input.clone_from(input);
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
            spec.cost.tiers.clone_from(tiers);
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
        OPENAI_RESPONSES_API => OPENAI_RESPONSES_API,
        ANTHROPIC_MESSAGES_API => ANTHROPIC_MESSAGES_API,
        GOOGLE_GENERATIVE_AI_API => GOOGLE_GENERATIVE_AI_API,
        other => {
            let target = model.map_or_else(
                || format!("provider {provider}"),
                |model| format!("provider {provider}, model {model}"),
            );
            return Err(format!(
                "{target}: unsupported api {other:?}; this build supports openai-completions, openai-responses, anthropic-messages, and google-generative-ai"
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

fn optional_non_blank_map_keys<V>(
    values: &Option<BTreeMap<String, V>>,
    context: &(),
) -> garde::Result {
    values
        .as_ref()
        .map_or(Ok(()), |values| non_blank_map_keys(values, context))
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
            merge_compat_objects(&mut base, overlay);
            Some(Value::Object(base))
        }
        (_, overlay) => overlay,
    }
}

fn merge_compat_objects(base: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        const NESTED_MERGE_KEYS: [&str; 4] = [
            "openRouterRouting",
            "vercelGatewayRouting",
            "chatTemplateKwargs",
            "chatTemplateArgs",
        ];
        if NESTED_MERGE_KEYS.contains(&key.as_str())
            && let (Some(Value::Object(base_value)), Value::Object(overlay_value)) =
                (base.get_mut(&key), &value)
        {
            base_value.extend(overlay_value.clone());
            continue;
        }
        base.insert(key, value);
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
    use serde_json::json;

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
        assert!(encoded.contains("supportsFinishReason"));
        assert!(encoded.contains("thinkingTokenBudgetField"));
        assert!(encoded.contains("supportsExplicitPromptCacheMode"));
        assert!(encoded.contains("allowedFallbackModels"));
        assert!(encoded.contains("preferred_min_throughput"));
        assert!(encoded.contains("thinking.budget"));
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

    #[test]
    fn explicit_empty_headers_and_false_auth_header_are_valid_overlays() {
        let parsed: ModelsFile = serde_json::from_str(
            r#"{
              "providers": {
                "custom": { "authHeader": false },
                "headers-only": { "headers": {} }
              }
            }"#,
        )
        .unwrap();

        let providers = parsed.compile(&BTreeMap::new()).unwrap();
        assert_eq!(providers.len(), 2);
        assert!(!providers[0].auth_header);
        assert!(providers[1].headers.is_empty());
    }

    #[test]
    fn compat_validation_rejects_unknown_nested_routing_fields() {
        let parsed: ModelsFile = serde_json::from_str(
            r#"{
              "providers": {
                "custom": {
                  "baseUrl": "https://example.test",
                  "api": "openai-completions",
                  "models": [{
                    "id": "model",
                    "compat": {
                      "openRouterRouting": {"unknown": true}
                    }
                  }]
                }
              }
            }"#,
        )
        .unwrap();

        let provider = parsed.compile(&BTreeMap::new()).unwrap().remove(0);
        let error = provider.compose_with_base(&[]).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn compat_merge_is_shallow_except_for_pi_nested_maps() {
        let merged = merge_compat(
            Some(json!({
                "openRouterRouting": {"only": ["base"], "order": ["base"]},
                "chatTemplateKwargs": {"base": 1},
                "ordinary": {"base": true}
            })),
            Some(json!({
                "openRouterRouting": {"order": ["override"]},
                "chatTemplateKwargs": {"override": 2},
                "ordinary": {"override": true}
            })),
        )
        .unwrap();

        assert_eq!(merged["openRouterRouting"]["only"][0], "base");
        assert_eq!(merged["openRouterRouting"]["order"][0], "override");
        assert_eq!(merged["chatTemplateKwargs"]["base"], 1);
        assert_eq!(merged["chatTemplateKwargs"]["override"], 2);
        assert!(merged["ordinary"].get("base").is_none());
        assert_eq!(merged["ordinary"]["override"], true);
    }
}
