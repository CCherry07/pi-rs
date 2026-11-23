#![forbid(unsafe_code)]

//! Shared OpenAI Responses wire projection and SSE stream adaptation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{AbortSignal, Message, ProviderId, ResponseMetadata, StreamEvent, ToolSpec, Usage};
use pi_plugin::{
    CacheRetention, Provider, ProviderAvailability, ProviderCallContext, ProviderError,
    ProviderRequest, ProviderStream,
};
use pi_provider::{
    HttpBodyStream, HttpTransport, ReqwestTransport, SseDecoder, TransportError, insert_header,
    post_json_with_provider_hooks, read_error_response,
};
use pi_utils::time::unix_timestamp_ms as now_ms;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{OpenAiCompatibleConfig, validate_config};
use crate::request::SessionAffinityFormat;

mod replay;
mod state;

pub const OPENAI_RESPONSES_API: &str = "openai-responses";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptCacheKeyMode {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenAiResponsesCompat {
    pub supports_developer_role: Option<bool>,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_long_cache_retention: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    #[serde(rename = "supportsOpenAIGrammarTools")]
    pub supports_open_ai_grammar_tools: Option<bool>,
    pub supports_additional_tools: Option<bool>,
    pub supports_tool_search: Option<bool>,
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    pub supports_mid_convo_effort: Option<bool>,
    pub supports_mid_convo_system_messages: Option<bool>,
    pub supports_max_output_tokens: Option<bool>,
    pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
}

#[derive(Debug, Clone)]
struct ResolvedOpenAiResponsesCompat {
    supports_developer_role: bool,
    session_affinity_format: SessionAffinityFormat,
    supports_long_cache_retention: bool,
    supports_strict_mode: bool,
    default_strict_null: bool,
    supports_open_ai_grammar_tools: bool,
    supports_additional_tools: bool,
    supports_tool_search: bool,
    supports_explicit_prompt_cache_mode: bool,
    supports_mid_convo_effort: bool,
    supports_mid_convo_system_messages: bool,
    supports_max_output_tokens: bool,
    prompt_cache_key_mode: PromptCacheKeyMode,
}

impl ResolvedOpenAiResponsesCompat {
    fn for_codex_request(request: &ProviderRequest) -> Self {
        let mut compat = Self::for_request(request);
        compat.supports_strict_mode = request
            .model_spec
            .as_ref()
            .and_then(|model| model.compat.as_ref())
            .and_then(|config| config["supportsStrictMode"].as_bool())
            .unwrap_or(true);
        compat.default_strict_null = true;
        compat
    }

    fn for_request(request: &ProviderRequest) -> Self {
        let configured = request
            .model_spec
            .as_ref()
            .and_then(|model| model.compat.clone())
            .and_then(|value| serde_json::from_value::<OpenAiResponsesCompat>(value).ok())
            .unwrap_or_default();
        let openrouter = request.model_spec.as_ref().is_some_and(|model| {
            model.provider.as_str() == "openrouter"
                || model
                    .base_url
                    .as_deref()
                    .is_some_and(|url| url.contains("openrouter.ai"))
        });
        Self {
            supports_developer_role: configured.supports_developer_role.unwrap_or(true),
            session_affinity_format: configured.session_affinity_format.unwrap_or(if openrouter {
                SessionAffinityFormat::Openrouter
            } else {
                SessionAffinityFormat::Openai
            }),
            supports_long_cache_retention: configured.supports_long_cache_retention.unwrap_or(true),
            supports_strict_mode: configured.supports_strict_mode.unwrap_or(false),
            default_strict_null: false,
            supports_open_ai_grammar_tools: configured
                .supports_open_ai_grammar_tools
                .unwrap_or(false),
            supports_additional_tools: configured.supports_additional_tools.unwrap_or(false),
            supports_tool_search: configured.supports_tool_search.unwrap_or(false),
            supports_explicit_prompt_cache_mode: configured
                .supports_explicit_prompt_cache_mode
                .unwrap_or(false),
            supports_mid_convo_effort: configured.supports_mid_convo_effort.unwrap_or(false),
            supports_mid_convo_system_messages: configured
                .supports_mid_convo_system_messages
                .unwrap_or(false),
            supports_max_output_tokens: configured.supports_max_output_tokens.unwrap_or(true),
            prompt_cache_key_mode: configured.prompt_cache_key_mode.unwrap_or_default(),
        }
    }
}

/// Configurable provider for OpenAI Responses-compatible endpoints.
pub struct OpenAiResponsesCompatibleProvider {
    config: OpenAiCompatibleConfig,
    transport: Arc<dyn HttpTransport>,
}

impl OpenAiResponsesCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: OpenAiCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        validate_config(&config)?;
        Ok(Self { config, transport })
    }

    pub(crate) fn endpoint(&self) -> String {
        responses_endpoint(&self.config.base_url)
    }

    fn headers(
        &self,
        request: &ProviderRequest,
        cache_retention: CacheRetention,
    ) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        if let Some(key) = &self.config.api_key {
            insert_header(&mut headers, "Authorization", format!("Bearer {key}"));
        }
        for (name, value) in response_affinity_headers(request, cache_retention) {
            insert_header(&mut headers, name, value);
        }
        // Match Codex client thread identity without a provider-specific switch.
        // Unlike cache affinity, this stays present when prompt caching is off.
        if let Some(session_id) = request
            .session_id
            .as_ref()
            .filter(|id| !id.trim().is_empty())
        {
            insert_header(&mut headers, "thread-id", session_id.clone());
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }
}

#[async_trait]
impl Provider for OpenAiResponsesCompatibleProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id.clone()
    }

    fn availability(&self) -> ProviderAvailability {
        if self.config.api_key.is_some() {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let cache_retention = cache_retention(&request);
        let headers = self.headers(&request, cache_retention);
        let endpoint = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .map(responses_endpoint)
            .unwrap_or_else(|| self.endpoint());
        // Match Pi's direct ChatGPT grant detection, not arbitrary bearer-auth
        // gateways. Keep credential inspection inside the provider, never in
        // public model metadata or the semantic request.
        let base_url = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .unwrap_or(&self.config.base_url);
        let chatgpt_sign_in = self.config.provider_id.as_str() == "openai"
            && base_url == "https://api.openai.com/v1"
            && self
                .config
                .api_key
                .as_deref()
                .is_some_and(|key| !key.starts_with("sk-"));
        let payload = context
            .before_provider_request(
                &signal,
                request_body_with_options(&request, cache_retention, chatgpt_sign_in)?,
            )
            .await?;
        let response = post_json_with_provider_hooks(
            self.transport.as_ref(),
            &context,
            &endpoint,
            headers,
            &payload,
            signal.clone(),
        )
        .await
        .map_err(map_transport_error)?;
        if !(200..300).contains(&response.status) {
            let message = read_error_response(response)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(message));
        }
        if !response
            .content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected text/event-stream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }

        let grammar = grammar_properties(&request)?;
        let effort = configuration_effort(&request, chatgpt_sign_in);
        Ok(Box::pin(
            stream_with_grammar(
                self.config.provider_id.clone(),
                request.model,
                OPENAI_RESPONSES_API,
                response.body,
                signal,
                grammar,
            )
            .map(move |event| {
                event.map(|mut event| {
                    if let StreamEvent::Start { metadata } = &mut event {
                        metadata.provider_thinking_level.clone_from(&effort);
                    }
                    event
                })
            }),
        ))
    }
}

fn responses_endpoint(base: &str) -> String {
    let base = base.trim();
    let suffix_start = [base.find('?'), base.find('#')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(base.len());
    let (path, suffix) = base.split_at(suffix_start);
    let path = path.trim_end_matches('/');
    if path.ends_with("/responses") {
        format!("{path}{suffix}")
    } else {
        format!("{path}/responses{suffix}")
    }
}

/// Projects semantic messages without a target model. Model-bound signatures
/// are omitted; normal provider requests use the model-aware replay path.
pub fn input_items(messages: &[Message]) -> Vec<Value> {
    replay::balanced(messages, None)
        .iter()
        .enumerate()
        .flat_map(|(position, message)| replay::project(message, None, position))
        .collect()
}

/// Model-aware replay for sibling Responses transports. Their system prompt
/// remains in the transport-owned instructions field; explicit updates stay in input.
pub(crate) fn replay_input(request: &ProviderRequest) -> Result<Vec<Value>, ProviderError> {
    let mut compat = ResolvedOpenAiResponsesCompat::for_codex_request(request);
    compat.supports_mid_convo_system_messages = true;
    input_items_with_deferred_tools(request, &compat, None)
}

/// Projects semantic tools into OpenAI Responses function definitions.
pub fn tools(tools: &[ToolSpec]) -> Result<Vec<Value>, ProviderError> {
    tools_with_compat(tools, true, false, false, false)
}

pub fn grammar_properties(
    request: &ProviderRequest,
) -> Result<BTreeMap<String, String>, ProviderError> {
    pi_provider::grammar_tool_input_properties(
        &request.tools,
        ResolvedOpenAiResponsesCompat::for_request(request).supports_open_ai_grammar_tools,
    )
    .map_err(ProviderError::Protocol)
}

pub(crate) fn codex_tools(request: &ProviderRequest) -> Result<Vec<Value>, ProviderError> {
    let compat = ResolvedOpenAiResponsesCompat::for_codex_request(request);
    tools_with_compat(
        &request.tools,
        compat.supports_strict_mode,
        compat.supports_open_ai_grammar_tools,
        false,
        true,
    )
}

fn tools_with_compat(
    tools: &[ToolSpec],
    supports_strict_mode: bool,
    supports_grammar: bool,
    defer_loading: bool,
    strict_null: bool,
) -> Result<Vec<Value>, ProviderError> {
    tools.iter().map(|tool| {
        let mut value = if let Some(grammar) = pi_provider::resolve_grammar_sampling(tool, supports_grammar).map_err(ProviderError::Protocol)? {
            json!({"type":"custom","name":tool.name,"description":tool.description,"format":{"type":"grammar","syntax":grammar.format,"definition":grammar.definition}})
        } else {
            let strict = pi_provider::resolve_json_schema_sampling(tool, supports_strict_mode).map_err(ProviderError::Protocol)?;
            let mut value = json!({"type":"function","name":tool.name,"description":tool.description,"parameters":strict.as_ref().unwrap_or(&tool.parameters)});
            if supports_strict_mode { value["strict"] = if strict.is_some() { Value::Bool(true) } else if strict_null { Value::Null } else { Value::Bool(false) }; }
            value
        };
        if defer_loading { value["defer_loading"] = Value::Bool(true); }
        Ok(value)
    }).collect()
}

/// Projects a semantic request into an OpenAI Responses payload.
pub fn request_body(request: &ProviderRequest) -> Result<Value, ProviderError> {
    request_body_with_options(request, cache_retention(request), false)
}

fn request_body_with_options(
    request: &ProviderRequest,
    cache_retention: CacheRetention,
    chatgpt_sign_in: bool,
) -> Result<Value, ProviderError> {
    let compat = ResolvedOpenAiResponsesCompat::for_request(request);
    let mut input = Vec::new();
    let system_prompt = if compat.supports_mid_convo_system_messages {
        request.system_prompt.clone()
    } else {
        pi_core::SystemMessage::fold(&request.system_prompt, &request.messages)
    };
    if !system_prompt.is_empty() {
        let role = if request
            .model_spec
            .as_ref()
            .is_some_and(|model| model.reasoning)
            && compat.supports_developer_role
        {
            "developer"
        } else {
            "system"
        };
        input.push(json!({"role": role, "content": system_prompt}));
    }
    let configuration_effort = configuration_effort(request, chatgpt_sign_in);
    input.extend(input_items_with_deferred_tools(
        request,
        &compat,
        configuration_effort.as_deref(),
    )?);
    if let Some(effort) = &configuration_effort {
        input.push(json!({"type":"configuration_update","reasoning":{"effort":effort}}));
    }
    let deferred_names = deferred_tool_names(request, &compat);
    let immediate_tools = request
        .tools
        .iter()
        .filter(|tool| !deferred_names.contains(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    let tools = tools_with_compat(
        &immediate_tools,
        compat.supports_strict_mode,
        compat.supports_open_ai_grammar_tools,
        false,
        false,
    )?;
    let mut payload = json!({
        "model": request.model.as_str(),
        "input": input,
        "stream": true,
        "store": false
    });
    if !tools.is_empty() {
        payload["tools"] = Value::Array(tools);
        payload["tool_choice"] = Value::String("auto".to_string());
    }
    if request.thinking_level != pi_core::ThinkingLevel::Off {
        let effort = request
            .model_spec
            .as_ref()
            .and_then(|model| {
                model
                    .thinking_level_map
                    .get(request.thinking_level.as_str())
            })
            .map_or_else(
                || Some(request.thinking_level.as_str().to_string()),
                Clone::clone,
            );
        if let Some(effort) = effort {
            payload["reasoning"] = json!({
                "effort": if configuration_effort.is_some() { "medium" } else { &effort },
                "summary": "auto"
            });
            payload["include"] = json!(["reasoning.encrypted_content"]);
        }
    } else if let Some(model) = request.model_spec.as_ref().filter(|model| model.reasoning)
        && model.thinking_level_map.get("off") != Some(&None)
    {
        payload["reasoning"] = json!({
            "effort": model
                .thinking_level_map
                .get("off")
                .and_then(Clone::clone)
                .unwrap_or_else(|| "none".to_string())
        });
    }
    if let Some(max_tokens) = request.max_output_tokens
        && compat.supports_max_output_tokens
        && !chatgpt_sign_in
    {
        payload["max_output_tokens"] = json!(max_tokens.max(16));
    }
    if cache_retention != CacheRetention::None
        && compat.prompt_cache_key_mode != PromptCacheKeyMode::Disabled
        && let Some(session_id) = &request.session_id
    {
        // Pi counts Unicode code points, not UTF-8 bytes, for the 64-character limit.
        payload["prompt_cache_key"] = Value::String(session_id.chars().take(64).collect());
    }
    if !chatgpt_sign_in {
        match cache_retention {
            CacheRetention::None if compat.supports_explicit_prompt_cache_mode => {
                payload["prompt_cache_options"] = json!({"mode": "explicit"});
            }
            CacheRetention::Long if compat.supports_long_cache_retention => {
                if compat.supports_explicit_prompt_cache_mode {
                    payload["prompt_cache_options"] = json!({"ttl": "30m"});
                } else {
                    payload["prompt_cache_retention"] = json!("24h");
                }
            }
            _ => {}
        }
    }
    // models.json samplingParams are applied last so user keys win.
    if let Value::Object(payload) = &mut payload {
        payload.extend(request.sampling_params.clone());
    }
    Ok(payload)
}

fn configuration_effort(request: &ProviderRequest, chatgpt_sign_in: bool) -> Option<String> {
    if chatgpt_sign_in
        || request.thinking_level == pi_core::ThinkingLevel::Off
        || !ResolvedOpenAiResponsesCompat::for_request(request).supports_mid_convo_effort
    {
        return None;
    }
    let model = request.model_spec.as_ref()?;
    if !model.reasoning || model.api != OPENAI_RESPONSES_API {
        return None;
    }
    Some(
        model
            .thinking_level_map
            .get(request.thinking_level.as_str())
            .and_then(Clone::clone)
            .unwrap_or_else(|| request.thinking_level.as_str().into()),
    )
}

fn deferred_tool_names(
    request: &ProviderRequest,
    compat: &ResolvedOpenAiResponsesCompat,
) -> std::collections::HashSet<String> {
    if !compat.supports_additional_tools && !compat.supports_tool_search {
        return std::collections::HashSet::new();
    }
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(message) => message.added_tool_names.as_ref(),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect()
}

fn input_items_with_deferred_tools(
    request: &ProviderRequest,
    compat: &ResolvedOpenAiResponsesCompat,
    configuration_effort: Option<&str>,
) -> Result<Vec<Value>, ProviderError> {
    let mut items = Vec::new();
    let mut loaded = std::collections::HashSet::new();
    let by_name = request
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<HashMap<_, _>>();
    let balanced = replay::balanced(&request.messages, request.model_spec.as_ref());
    for (position, message) in balanced.iter().enumerate() {
        if let Message::System(system) = message {
            if compat.supports_mid_convo_system_messages {
                let text = if position == 0 && request.system_prompt.is_empty() {
                    system.render()
                } else {
                    let mut parts = vec![system.instruction_text()];
                    parts.extend(system.sections.iter().map(|(name, value)| match value {
                        Some(text) => {
                            format!("Updated system prompt section \"{name}\":\n\n{text}")
                        }
                        None => format!("Removed system prompt section \"{name}\"."),
                    }));
                    parts
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };
                if !text.is_empty() {
                    let role = if compat.supports_developer_role
                        && request
                            .model_spec
                            .as_ref()
                            .is_some_and(|model| model.reasoning)
                    {
                        "developer"
                    } else {
                        "system"
                    };
                    items.push(json!({"role":role,"content":text}));
                }
            }
            continue;
        }
        let leading_system = matches!(balanced.first(), Some(Message::System(_)));
        let projected = replay::project(
            message,
            request.model_spec.as_ref(),
            position.saturating_sub(usize::from(leading_system)),
        );
        if !projected.is_empty()
            && configuration_effort.is_some()
            && let Message::Assistant(assistant) = message
            && request.model_spec.as_ref().is_some_and(|model| {
                assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id
            })
            && let Some(effort) = &assistant.provider_thinking_level
        {
            items.push(json!({"type":"configuration_update","reasoning":{"effort":effort}}));
        }
        items.extend(projected);
        let Message::ToolResult(result) = message else {
            continue;
        };
        let deferred = result
            .added_tool_names
            .iter()
            .flatten()
            .filter(|name| loaded.insert((*name).clone()))
            .filter_map(|name| by_name.get(name.as_str()).copied().cloned())
            .collect::<Vec<_>>();
        if deferred.is_empty() {
            continue;
        }
        if compat.supports_additional_tools {
            items.push(json!({
                "type": "additional_tools",
                "role": "developer",
                "tools": tools_with_compat(&deferred, compat.supports_strict_mode, compat.supports_open_ai_grammar_tools, false, compat.default_strict_null)?
            }));
        } else if compat.supports_tool_search {
            let names = deferred
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>();
            let call_id = format!("pi_tool_load_{}", items.len());
            items.push(json!({
                "type": "tool_search_call",
                "call_id": call_id,
                "execution": "client",
                "status": "completed",
                "arguments": {"query": names.join(" "), "limit": names.len()}
            }));
            items.push(json!({
                "type": "tool_search_output",
                "call_id": call_id,
                "execution": "client",
                "status": "completed",
                "tools": tools_with_compat(&deferred, compat.supports_strict_mode, compat.supports_open_ai_grammar_tools, true, compat.default_strict_null)?
            }));
        }
    }
    apply_grammar_replay(&mut items, request, &grammar_properties(request)?)?;
    Ok(items)
}

fn apply_grammar_replay(
    items: &mut [Value],
    request: &ProviderRequest,
    properties: &BTreeMap<String, String>,
) -> Result<(), ProviderError> {
    let mut calls = std::collections::HashSet::new();
    for message in &request.messages {
        if let Message::ToolResult(result) = message
            && properties.contains_key(&result.tool_name)
        {
            calls.insert(
                result
                    .tool_call_id
                    .as_str()
                    .split('|')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }
    for item in items {
        if item["type"] == "function_call" {
            let name = item["name"].as_str().unwrap_or_default();
            if let Some(property) = properties.get(name) {
                let arguments: Value =
                    serde_json::from_str(item["arguments"].as_str().unwrap_or("{}"))
                        .map_err(|error| ProviderError::Protocol(error.to_string()))?;
                let input = arguments[property].as_str().ok_or_else(||ProviderError::Protocol(format!("Grammar tool call \"{name}\" requires argument \"{property}\" to be a string.")))?.to_string();
                calls.insert(item["call_id"].as_str().unwrap_or_default().to_string());
                let object = item
                    .as_object_mut()
                    .expect("projected tool calls are objects");
                object.remove("arguments");
                if object
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.starts_with("ctc_"))
                {
                    object.remove("id");
                }
                object.insert("type".into(), json!("custom_tool_call"));
                object.insert("input".into(), json!(input));
            } else if item["id"].as_str().is_some_and(|id| !id.starts_with("fc_")) {
                item.as_object_mut()
                    .expect("projected tool calls are objects")
                    .remove("id");
            }
        } else if item["type"] == "function_call_output"
            && item["call_id"]
                .as_str()
                .is_some_and(|id| calls.contains(id))
        {
            item["type"] = json!("custom_tool_call_output");
        }
    }
    Ok(())
}

/// Adapts an accepted OpenAI Responses SSE body into the semantic provider stream.
pub fn stream(
    provider: ProviderId,
    model: pi_core::ModelId,
    api: impl Into<String>,
    body: HttpBodyStream,
    signal: AbortSignal,
) -> ProviderStream {
    stream_with_grammar(provider, model, api, body, signal, BTreeMap::new())
}

pub fn stream_with_grammar(
    provider: ProviderId,
    model: pi_core::ModelId,
    api: impl Into<String>,
    mut body: HttpBodyStream,
    signal: AbortSignal,
    grammar: BTreeMap<String, String>,
) -> ProviderStream {
    let api = api.into();
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(provider, model.clone(), api, now_ms()),
        });
        let mut decoder = SseDecoder::new();
        let mut state = StreamState::new(&model).with_grammar(grammar);
        loop {
            let next = tokio::select! {
                _ = signal.wait() => { yield Err(ProviderError::Aborted); return; }
                next = body.next() => next,
            };
            match next {
                Some(Ok(bytes)) => {
                    let events = match decoder.push(&bytes) {
                        Ok(events) => events,
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    };
                    for event in events {
                        if event.data == "[DONE]" {
                            for event in state.finish() { yield Ok(event); }
                            if let Some(error) = state.take_pending_error() { yield Err(error); }
                            return;
                        }
                        match state.consume(&event.data) {
                            Ok(events) => {
                                let done = events.iter().any(|event| matches!(event, StreamEvent::Done { .. }));
                                let pending_error = state.take_pending_error();
                                for event in events { yield Ok(event); }
                                if let Some(error) = pending_error { yield Err(error); return; }
                                if done { return; }
                            }
                            Err(error) => { yield Err(error); return; }
                        }
                    }
                }
                Some(Err(error)) => { yield Err(map_transport_error(error)); return; }
                None => {
                    match decoder.finish() {
                        Ok(Some(event)) if event.data != "[DONE]" => match state.consume(&event.data) {
                            Ok(events) => {
                                let pending_error = state.take_pending_error();
                                for event in events { yield Ok(event); }
                                if let Some(error) = pending_error { yield Err(error); return; }
                            },
                            Err(error) => { yield Err(error); return; }
                        },
                        Ok(_) => {}
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    }
                    for event in state.finish() { yield Ok(event); }
                    if let Some(error) = state.take_pending_error() { yield Err(error); }
                    return;
                }
            }
        }
    })
}

use state::StreamState;

fn response_usage(response: &Value) -> Usage {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let input_total = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Usage {
        input: input_total.saturating_sub(cache_read),
        output,
        cache_read,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(input_total + output),
        cost: Default::default(),
    }
}

fn cache_retention(request: &ProviderRequest) -> CacheRetention {
    request.cache_retention.unwrap_or_else(|| {
        match std::env::var("PI_CACHE_RETENTION").as_deref() {
            Ok("none") => CacheRetention::None,
            Ok("long") => CacheRetention::Long,
            _ => CacheRetention::Short,
        }
    })
}

fn response_affinity_headers(
    request: &ProviderRequest,
    cache_retention: CacheRetention,
) -> BTreeMap<String, String> {
    if cache_retention == CacheRetention::None {
        return BTreeMap::new();
    }
    let Some(session_id) = &request.session_id else {
        return BTreeMap::new();
    };
    match ResolvedOpenAiResponsesCompat::for_request(request).session_affinity_format {
        SessionAffinityFormat::Openrouter => {
            BTreeMap::from([("x-session-id".to_string(), session_id.clone())])
        }
        SessionAffinityFormat::Openai => BTreeMap::from([
            ("session-id".to_string(), session_id.clone()),
            ("x-client-request-id".to_string(), session_id.clone()),
        ]),
        SessionAffinityFormat::OpenaiNosession => {
            BTreeMap::from([("x-client-request-id".to_string(), session_id.clone())])
        }
    }
}

fn map_transport_error(error: TransportError) -> ProviderError {
    error.into_provider_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use pi_core::{AbortHandle, ContentMetadata, ModelId, ModelSpec, ThinkingLevel, UserMessage};
    use pi_plugin::ProviderRequest;

    #[test]
    fn compatible_endpoint_accepts_root_and_full_urls() {
        let root = OpenAiResponsesCompatibleProvider::new(OpenAiCompatibleConfig::without_api_key(
            "https://api.x.ai/v1",
        ))
        .unwrap();
        assert_eq!(root.endpoint(), "https://api.x.ai/v1/responses");

        let full = OpenAiResponsesCompatibleProvider::new(OpenAiCompatibleConfig::without_api_key(
            "https://gateway.example/responses?api-version=2026-01-01",
        ))
        .unwrap();
        assert_eq!(
            full.endpoint(),
            "https://gateway.example/responses?api-version=2026-01-01"
        );
    }

    #[test]
    fn projects_messages_and_tools_through_the_public_interface() {
        let input = input_items(&[Message::User(UserMessage::text("hello", 0))]);
        assert_eq!(input[0]["content"][0]["type"], "input_text");
    }

    #[test]
    fn models_json_compat_controls_responses_cache_and_affinity() {
        let mut model = ModelSpec::new("custom", "model", "Model", OPENAI_RESPONSES_API);
        model.compat = Some(json!({
            "supportsExplicitPromptCacheMode": true,
            "sessionAffinityFormat": "openrouter"
        }));
        let request = ProviderRequest {
            model: ModelId::new("model"),
            model_spec: Some(model),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Off,
            thinking_budgets: None,
            max_output_tokens: None,
            cache_retention: None,
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: Some("session-1".to_string()),
        };

        let body = request_body_with_options(&request, CacheRetention::None, false).unwrap();
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");
        assert!(body.get("prompt_cache_key").is_none());
        assert_eq!(
            response_affinity_headers(&request, CacheRetention::Short)["x-session-id"],
            "session-1"
        );
    }

    #[test]
    fn response_stream_state_preserves_metadata_namespace_and_failure_status() {
        let mut state = StreamState::new(&ModelId::new("requested-model"));
        let created = state
            .consume(
                r#"{"type":"response.created","response":{"id":"response-1","model":"resolved-model"}}"#,
            )
            .unwrap();
        assert!(matches!(
            &created[0],
            StreamEvent::Metadata { patch }
                if patch.response_id.as_deref() == Some("response-1")
                    && patch.response_model.as_deref() == Some("resolved-model")
        ));

        let added = state
            .consume(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item-1","call_id":"call-1","name":"echo","namespace":"dynamic"}}"#,
            )
            .unwrap();
        assert!(added.iter().any(|event| matches!(
            event,
            StreamEvent::ContentMetadata {
                metadata: ContentMetadata::ToolCall { namespace: Some(namespace) },
                ..
            } if namespace == "dynamic"
        )));

        let failed = state
            .consume(
                r#"{"type":"response.failed","response":{"id":"response-1","status":"failed","end_turn":false,"error":{"message":"upstream failed"}}}"#,
            )
            .unwrap();
        assert!(failed.iter().any(|event| matches!(
            event,
            StreamEvent::Metadata { patch }
                if patch.raw_stop_reason.as_deref() == Some("failed")
                    && patch.end_turn == Some(false)
        )));
        assert!(matches!(
            state.take_pending_error(),
            Some(ProviderError::Failure(message)) if message == "upstream failed"
        ));
    }

    #[tokio::test]
    async fn adapts_completed_sse_into_semantic_events() {
        let body: HttpBodyStream = Box::pin(stream::iter([Ok(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                .to_vec(),
        )]));
        let (_, signal) = AbortHandle::new();
        let events = stream(
            ProviderId::new("test"),
            ModelId::new("model"),
            "openai-responses",
            body,
            signal,
        )
        .collect::<Vec<_>>()
        .await;
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::TextDelta { delta, .. }) if delta == "hi")
        ));
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::Done { usage, .. }) if usage.total_tokens == 2)
        ));
    }

    #[tokio::test]
    async fn response_failure_yields_observed_metadata_before_the_error() {
        let body: HttpBodyStream = Box::pin(stream::iter([Ok(
            b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"response-1\"}}\n\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"response-1\",\"status\":\"failed\",\"error\":{\"message\":\"upstream failed\"}}}\n\n"
                .to_vec(),
        )]));
        let (_, signal) = AbortHandle::new();
        let events = stream(
            ProviderId::new("test"),
            ModelId::new("model"),
            "openai-responses",
            body,
            signal,
        )
        .collect::<Vec<_>>()
        .await;

        let failed_metadata = events.iter().position(|event| {
            matches!(
                event,
                Ok(StreamEvent::Metadata { patch })
                    if patch.raw_stop_reason.as_deref() == Some("failed")
            )
        });
        let failure = events.iter().position(|event| {
            matches!(
                event,
                Err(ProviderError::Failure(message)) if message == "upstream failed"
            )
        });
        assert!(
            failed_metadata.is_some_and(|metadata| failure.is_some_and(|error| metadata < error))
        );
    }
}
