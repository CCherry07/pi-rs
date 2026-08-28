use std::collections::{BTreeMap, HashSet};

use pi_core::{ContentBlock, Message, ModelSpec, ProviderRequest, ThinkingLevel, ToolSpec};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct OpenAiCompletionsCompat {
    pub supports_store: Option<bool>,
    pub supports_developer_role: Option<bool>,
    pub supports_reasoning_effort: Option<bool>,
    pub supports_usage_in_streaming: Option<bool>,
    pub supports_finish_reason: Option<bool>,
    pub max_tokens_field: Option<MaxTokensField>,
    pub requires_tool_result_name: Option<bool>,
    pub requires_assistant_after_tool_result: Option<bool>,
    pub requires_thinking_as_text: Option<bool>,
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    pub thinking_format: Option<ThinkingFormat>,
    pub chat_template_kwargs: Option<BTreeMap<String, Value>>,
    pub chat_template_args: Option<BTreeMap<String, Value>>,
    pub cache_control_format: Option<CacheControlFormat>,
    pub open_router_routing: Option<Value>,
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    pub zai_tool_stream: Option<bool>,
    pub supports_thinking_token_budget: Option<bool>,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    #[serde(rename = "supportsOpenAIGrammarTools")]
    pub supports_open_ai_grammar_tools: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    pub send_session_affinity_headers: Option<bool>,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_long_cache_retention: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
    #[default]
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlFormat {
    Anthropic,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeferredToolsMode {
    Kimi,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
    #[default]
    Openai,
    OpenaiNosession,
    Openrouter,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum ThinkingTokenBudgetField {
    #[serde(rename = "thinking_token_budget")]
    TokenBudget,
    #[serde(rename = "thinking_budget")]
    Budget,
    #[serde(rename = "thinking_budget_tokens")]
    BudgetTokens,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VercelGatewayRouting {
    pub only: Option<Vec<String>>,
    pub order: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedOpenAiCompletionsCompat {
    supports_store: bool,
    supports_developer_role: bool,
    supports_reasoning_effort: bool,
    supports_usage_in_streaming: bool,
    pub(crate) supports_finish_reason: bool,
    max_tokens_field: MaxTokensField,
    requires_tool_result_name: bool,
    requires_assistant_after_tool_result: bool,
    requires_thinking_as_text: bool,
    requires_reasoning_content_on_assistant_messages: bool,
    thinking_format: ThinkingFormat,
    chat_template_kwargs: BTreeMap<String, Value>,
    chat_template_args: BTreeMap<String, Value>,
    cache_control_format: Option<CacheControlFormat>,
    open_router_routing: Option<Value>,
    vercel_gateway_routing: Option<VercelGatewayRouting>,
    zai_tool_stream: bool,
    supports_thinking_token_budget: bool,
    thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    supports_strict_mode: bool,
    send_session_affinity_headers: bool,
    deferred_tools_mode: Option<DeferredToolsMode>,
    session_affinity_format: SessionAffinityFormat,
    supports_long_cache_retention: bool,
}

impl ResolvedOpenAiCompletionsCompat {
    pub(crate) fn for_request(request: &ProviderRequest) -> Self {
        let detected = detect_compat(request.model_spec.as_ref());
        let configured = request
            .model_spec
            .as_ref()
            .and_then(|model| model.compat.clone())
            .and_then(|value| serde_json::from_value::<OpenAiCompletionsCompat>(value).ok())
            .unwrap_or_default();
        Self {
            supports_store: configured.supports_store.unwrap_or(detected.supports_store),
            supports_developer_role: configured
                .supports_developer_role
                .unwrap_or(detected.supports_developer_role),
            supports_reasoning_effort: configured
                .supports_reasoning_effort
                .unwrap_or(detected.supports_reasoning_effort),
            supports_usage_in_streaming: configured
                .supports_usage_in_streaming
                .unwrap_or(detected.supports_usage_in_streaming),
            supports_finish_reason: configured
                .supports_finish_reason
                .unwrap_or(detected.supports_finish_reason),
            max_tokens_field: configured
                .max_tokens_field
                .unwrap_or(detected.max_tokens_field),
            requires_tool_result_name: configured
                .requires_tool_result_name
                .unwrap_or(detected.requires_tool_result_name),
            requires_assistant_after_tool_result: configured
                .requires_assistant_after_tool_result
                .unwrap_or(detected.requires_assistant_after_tool_result),
            requires_thinking_as_text: configured
                .requires_thinking_as_text
                .unwrap_or(detected.requires_thinking_as_text),
            requires_reasoning_content_on_assistant_messages: configured
                .requires_reasoning_content_on_assistant_messages
                .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
            thinking_format: configured
                .thinking_format
                .unwrap_or(detected.thinking_format),
            chat_template_kwargs: configured
                .chat_template_kwargs
                .unwrap_or(detected.chat_template_kwargs),
            chat_template_args: configured
                .chat_template_args
                .unwrap_or(detected.chat_template_args),
            cache_control_format: configured
                .cache_control_format
                .or(detected.cache_control_format),
            open_router_routing: configured.open_router_routing,
            vercel_gateway_routing: configured
                .vercel_gateway_routing
                .or(detected.vercel_gateway_routing),
            zai_tool_stream: configured
                .zai_tool_stream
                .unwrap_or(detected.zai_tool_stream),
            supports_thinking_token_budget: configured
                .supports_thinking_token_budget
                .unwrap_or(detected.supports_thinking_token_budget),
            thinking_token_budget_field: configured
                .thinking_token_budget_field
                .or(detected.thinking_token_budget_field)
                .or_else(|| {
                    configured
                        .supports_thinking_token_budget
                        .unwrap_or(detected.supports_thinking_token_budget)
                        .then_some(ThinkingTokenBudgetField::TokenBudget)
                }),
            supports_strict_mode: configured
                .supports_strict_mode
                .unwrap_or(detected.supports_strict_mode),
            send_session_affinity_headers: configured
                .send_session_affinity_headers
                .unwrap_or(detected.send_session_affinity_headers),
            deferred_tools_mode: configured
                .deferred_tools_mode
                .or(detected.deferred_tools_mode),
            session_affinity_format: configured
                .session_affinity_format
                .unwrap_or(detected.session_affinity_format),
            supports_long_cache_retention: configured
                .supports_long_cache_retention
                .unwrap_or(detected.supports_long_cache_retention),
        }
    }
}

pub(crate) fn request_body(request: &ProviderRequest) -> Value {
    let compat = ResolvedOpenAiCompletionsCompat::for_request(request);
    let mut messages = messages(request, &compat);
    let deferred_tool_names = if compat.deferred_tools_mode == Some(DeferredToolsMode::Kimi) {
        request
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(message) => message.added_tool_names.as_ref(),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    let mut tools = request
        .tools
        .iter()
        .filter(|tool| !deferred_tool_names.contains(&tool.name))
        .map(|tool| tool_value(tool, &compat))
        .collect::<Vec<_>>();
    let mut body = json!({
        "model":request.model.as_str(), "messages":messages, "stream":true
    });
    if compat.supports_usage_in_streaming {
        body["stream_options"] = json!({"include_usage": true});
    }
    if compat.supports_store {
        body["store"] = Value::Bool(false);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.clone());
        if compat.zai_tool_stream {
            body["tool_stream"] = Value::Bool(true);
        }
    } else if has_tool_history(&request.messages) {
        // Anthropic-compatible proxies can require a tools field while
        // replaying tool calls even when no tools are currently active.
        body["tools"] = Value::Array(Vec::new());
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        body[match compat.max_tokens_field {
            MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
            MaxTokensField::MaxTokens => "max_tokens",
        }] = Value::from(max_output_tokens);
    }
    let thinking_budget = thinking_budget(request);
    apply_thinking(request, &compat, thinking_budget, &mut body);
    if let (Some(field), Some(budget)) = (compat.thinking_token_budget_field, thinking_budget) {
        body[field.as_str()] = Value::from(budget);
    }
    if let Some(routing) = &compat.open_router_routing {
        body["provider"] = routing.clone();
    }
    if let Some(routing) = &compat.vercel_gateway_routing {
        let mut gateway = serde_json::Map::new();
        if let Some(only) = &routing.only {
            gateway.insert("only".to_string(), json!(only));
        }
        if let Some(order) = &routing.order {
            gateway.insert("order".to_string(), json!(order));
        }
        if !gateway.is_empty() {
            body["providerOptions"] = json!({"gateway": gateway});
        }
    }
    if compat.cache_control_format == Some(CacheControlFormat::Anthropic)
        && cache_retention() != CacheRetention::None
    {
        let cache_control =
            if cache_retention() == CacheRetention::Long && compat.supports_long_cache_retention {
                json!({"type": "ephemeral", "ttl": "1h"})
            } else {
                json!({"type": "ephemeral"})
            };
        apply_anthropic_cache_control(&mut messages, &mut tools, &cache_control);
        body["messages"] = Value::Array(messages);
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
    }
    match cache_retention() {
        CacheRetention::None => {}
        CacheRetention::Short => {
            let is_openai = request
                .model_spec
                .as_ref()
                .and_then(|model| model.base_url.as_deref())
                .is_some_and(|base_url| base_url.contains("api.openai.com"));
            if is_openai && let Some(session_id) = &request.session_id {
                body["prompt_cache_key"] = Value::String(session_id.clone());
            }
        }
        CacheRetention::Long if compat.supports_long_cache_retention => {
            if let Some(session_id) = &request.session_id {
                body["prompt_cache_key"] = Value::String(session_id.clone());
            }
            body["prompt_cache_retention"] = Value::String("24h".to_string());
        }
        CacheRetention::Long => {}
    }
    // models.json samplingParams are deliberately last so configured keys win.
    if let Value::Object(body) = &mut body {
        body.extend(request.sampling_params.clone());
    }
    body
}

pub(crate) fn affinity_headers(request: &ProviderRequest) -> BTreeMap<String, String> {
    let compat = ResolvedOpenAiCompletionsCompat::for_request(request);
    let Some(session_id) = request
        .session_id
        .as_ref()
        .filter(|_| compat.send_session_affinity_headers)
    else {
        return BTreeMap::new();
    };
    match compat.session_affinity_format {
        SessionAffinityFormat::Openrouter => {
            BTreeMap::from([("x-session-id".to_string(), session_id.clone())])
        }
        SessionAffinityFormat::Openai => BTreeMap::from([
            ("session_id".to_string(), session_id.clone()),
            ("x-client-request-id".to_string(), session_id.clone()),
            ("x-session-affinity".to_string(), session_id.clone()),
        ]),
        SessionAffinityFormat::OpenaiNosession => BTreeMap::from([
            ("x-client-request-id".to_string(), session_id.clone()),
            ("x-session-affinity".to_string(), session_id.clone()),
        ]),
    }
}

fn messages(request: &ProviderRequest, compat: &ResolvedOpenAiCompletionsCompat) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut pending_tool_images = Vec::new();
    let mut last_was_tool_result = false;
    if !request.system_prompt.is_empty() {
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
        messages.push(json!({"role": role, "content": request.system_prompt}));
    }
    for message in &request.messages {
        if !matches!(message, Message::ToolResult(_)) {
            let had_tool_images = !pending_tool_images.is_empty();
            flush_tool_images(
                &mut messages,
                &mut pending_tool_images,
                compat.requires_assistant_after_tool_result,
            );
            if had_tool_images {
                last_was_tool_result = false;
            }
        }
        match message {
            Message::User(message) => {
                if last_was_tool_result && compat.requires_assistant_after_tool_result {
                    messages.push(assistant_tool_bridge());
                }
                messages.push(json!({
                    "role": "user", "content": user_content(&message.content)
                }));
                last_was_tool_result = false;
            }
            Message::Custom(message) => {
                if last_was_tool_result && compat.requires_assistant_after_tool_result {
                    messages.push(assistant_tool_bridge());
                }
                messages.push(json!({
                    "role": "user", "content": user_content(&message.content.to_blocks())
                }));
                last_was_tool_result = false;
            }
            Message::Assistant(message) => {
                if let Some(message) = assistant_message(message, request, compat) {
                    messages.push(message);
                }
                last_was_tool_result = false;
            }
            Message::ToolResult(message) => {
                let text = blocks_text(&message.content);
                let has_images = message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Image(_)));
                let mut value = json!({
                    "role": "tool", "tool_call_id": message.tool_call_id.as_str(),
                    "content": if text.is_empty() {
                        if has_images { "(see attached image)" } else { "(no tool output)" }
                    } else { text.as_str() }
                });
                if compat.requires_tool_result_name && !message.tool_name.is_empty() {
                    value["name"] = Value::String(message.tool_name.clone());
                }
                messages.push(value);
                pending_tool_images.extend(message.content.iter().filter_map(image_part));
                if compat.deferred_tools_mode == Some(DeferredToolsMode::Kimi)
                    && let Some(names) = &message.added_tool_names
                {
                    let tools = request
                        .tools
                        .iter()
                        .filter(|tool| names.contains(&tool.name))
                        .map(|tool| tool_value(tool, compat))
                        .collect::<Vec<_>>();
                    if !tools.is_empty() {
                        messages.push(json!({"role": "system", "tools": tools}));
                    }
                }
                last_was_tool_result = true;
            }
        }
    }
    flush_tool_images(
        &mut messages,
        &mut pending_tool_images,
        compat.requires_assistant_after_tool_result,
    );
    messages
}

fn assistant_message(
    message: &pi_core::AssistantMessage,
    request: &ProviderRequest,
    compat: &ResolvedOpenAiCompletionsCompat,
) -> Option<Value> {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) if !text.text.trim().is_empty() => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let thinking = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                Some(thinking)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let calls = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(json!({
                "id":call.id.as_str(), "type":"function",
                "function":{"name":call.name, "arguments":call.arguments.to_string()}
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "role": "assistant",
        "content": if compat.requires_assistant_after_tool_result {
            Value::String(String::new())
        } else {
            Value::Null
        }
    });
    if compat.requires_thinking_as_text && !thinking.is_empty() {
        let thinking = thinking
            .iter()
            .map(|block| block.thinking.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut parts = vec![json!({"type": "text", "text": thinking})];
        if !text.is_empty() {
            parts.push(json!({"type": "text", "text": text}));
        }
        value["content"] = Value::Array(parts);
    } else {
        if !text.is_empty() {
            value["content"] = Value::String(text);
        }
        if let Some(block) = thinking.first()
            && let Some(signature) = block
                .thinking_signature
                .as_ref()
                .filter(|signature| !signature.is_empty())
        {
            value[signature] = Value::String(
                thinking
                    .iter()
                    .map(|block| block.thinking.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    if !calls.is_empty() {
        value["tool_calls"] = Value::Array(calls);
    }
    if compat.requires_reasoning_content_on_assistant_messages
        && request
            .model_spec
            .as_ref()
            .is_some_and(|model| model.reasoning)
        && value.get("reasoning_content").is_none()
    {
        value["reasoning_content"] = Value::String(String::new());
    }
    let has_content = value.get("content").is_some_and(|content| match content {
        Value::String(content) => !content.is_empty(),
        Value::Array(content) => !content.is_empty(),
        _ => false,
    });
    (has_content || value.get("tool_calls").is_some()).then_some(value)
}

fn flush_tool_images(messages: &mut Vec<Value>, images: &mut Vec<Value>, requires_bridge: bool) {
    if images.is_empty() {
        return;
    }
    if requires_bridge {
        messages.push(assistant_tool_bridge());
    }
    let mut parts = vec![json!({
        "type":"text", "text":"Attached image(s) from tool result:"
    })];
    parts.append(images);
    messages.push(json!({"role":"user", "content":parts}));
}

fn assistant_tool_bridge() -> Value {
    json!({"role": "assistant", "content": "I have processed the tool results."})
}

fn tool_value(tool: &ToolSpec, compat: &ResolvedOpenAiCompletionsCompat) -> Value {
    let mut function = json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters
    });
    if compat.supports_strict_mode {
        function["strict"] = Value::Bool(false);
    }
    json!({"type": "function", "function": function})
}

fn user_content(blocks: &[ContentBlock]) -> Value {
    if blocks
        .iter()
        .all(|block| matches!(block, ContentBlock::Text(_)))
    {
        return Value::String(blocks_text(blocks));
    }
    Value::Array(
        blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(value) => Some(json!({"type":"text","text":value.text})),
                ContentBlock::Image(_) => image_part(block),
                ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => None,
            })
            .collect(),
    )
}

fn image_part(block: &ContentBlock) -> Option<Value> {
    let ContentBlock::Image(image) = block else {
        return None;
    };
    Some(json!({
        "type":"image_url",
        "image_url":{"url":format!("data:{};base64,{}", image.mime_type, image.data)}
    }))
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(value) => Some(value.text.as_str()),
            ContentBlock::Thinking(value) => Some(value.thinking.as_str()),
            ContentBlock::Image(_) | ContentBlock::ToolCall(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_thinking(
    request: &ProviderRequest,
    compat: &ResolvedOpenAiCompletionsCompat,
    thinking_budget: Option<u64>,
    body: &mut Value,
) {
    let Some(model) = request.model_spec.as_ref().filter(|model| model.reasoning) else {
        return;
    };
    let enabled = request.thinking_level != ThinkingLevel::Off;
    let mapped = mapped_effort(model, request.thinking_level);
    match compat.thinking_format {
        ThinkingFormat::Zai => {
            body["thinking"] = if enabled {
                json!({"type": "enabled", "clear_thinking": false})
            } else if model.thinking_level_map.get("off") != Some(&None) {
                json!({"type": "disabled"})
            } else {
                Value::Null
            };
            add_reasoning_effort(body, mapped, enabled, compat);
        }
        ThinkingFormat::Qwen => {
            body["enable_thinking"] = Value::Bool(enabled);
            add_reasoning_effort(body, mapped, enabled, compat);
        }
        ThinkingFormat::QwenChatTemplate => {
            body["chat_template_kwargs"] = json!({
                "enable_thinking": enabled,
                "preserve_thinking": true
            });
        }
        ThinkingFormat::ChatTemplate => {
            if let Some(values) = resolve_chat_template_values(
                &compat.chat_template_kwargs,
                enabled,
                mapped.as_deref(),
                thinking_budget,
            ) {
                body["chat_template_kwargs"] = Value::Object(values);
            }
        }
        ThinkingFormat::Baseten => {
            if let Some(values) = resolve_chat_template_values(
                &compat.chat_template_args,
                enabled,
                mapped.as_deref(),
                thinking_budget,
            ) {
                body["chat_template_args"] = Value::Object(values);
            }
            add_reasoning_effort(body, mapped, enabled, compat);
        }
        ThinkingFormat::Deepseek => {
            if enabled {
                body["thinking"] = json!({"type": "enabled"});
            } else if model.thinking_level_map.get("off") != Some(&None) {
                body["thinking"] = json!({"type": "disabled"});
            }
            add_reasoning_effort(body, mapped, enabled, compat);
        }
        ThinkingFormat::Openrouter => {
            if let Some(effort) = mapped {
                body["reasoning"] = json!({"effort": effort});
            } else if !enabled && model.thinking_level_map.get("off") != Some(&None) {
                body["reasoning"] = json!({"effort": "none"});
            }
        }
        ThinkingFormat::AntLing => {
            if enabled
                && let Some(Some(effort)) = model
                    .thinking_level_map
                    .get(request.thinking_level.as_str())
            {
                body["reasoning"] = json!({"effort": effort});
            }
        }
        ThinkingFormat::Together => {
            body["reasoning"] = json!({"enabled": enabled});
            add_reasoning_effort(body, mapped, enabled, compat);
        }
        ThinkingFormat::StringThinking => {
            if let Some(effort) = mapped {
                body["thinking"] = Value::String(effort);
            } else if !enabled && model.thinking_level_map.get("off") != Some(&None) {
                body["thinking"] = Value::String("none".to_string());
            }
        }
        ThinkingFormat::Openai => add_reasoning_effort(body, mapped, enabled, compat),
    }
}

fn add_reasoning_effort(
    body: &mut Value,
    effort: Option<String>,
    enabled: bool,
    compat: &ResolvedOpenAiCompletionsCompat,
) {
    if compat.supports_reasoning_effort
        && (enabled || effort.is_some())
        && let Some(effort) = effort
    {
        body["reasoning_effort"] = Value::String(effort);
    }
}

fn mapped_effort(model: &ModelSpec, level: ThinkingLevel) -> Option<String> {
    match model.thinking_level_map.get(level.as_str()) {
        Some(Some(value)) => Some(value.clone()),
        Some(None) => None,
        None if level != ThinkingLevel::Off => Some(level.as_str().to_string()),
        None => None,
    }
}

fn resolve_chat_template_values(
    configured: &BTreeMap<String, Value>,
    enabled: bool,
    effort: Option<&str>,
    thinking_budget: Option<u64>,
) -> Option<serde_json::Map<String, Value>> {
    let mut values = serde_json::Map::new();
    for (name, value) in configured {
        let resolved = match value {
            Value::Object(variable) => {
                if !enabled
                    && variable
                        .get("omitWhenOff")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    continue;
                }
                match variable.get("$var").and_then(Value::as_str) {
                    Some("thinking.enabled") => Value::Bool(enabled),
                    Some("thinking.effort") => {
                        let Some(effort) = effort else { continue };
                        Value::String(effort.to_string())
                    }
                    Some("thinking.budget") => {
                        let Some(thinking_budget) = thinking_budget else {
                            continue;
                        };
                        Value::from(thinking_budget)
                    }
                    _ => value.clone(),
                }
            }
            _ => value.clone(),
        };
        values.insert(name.clone(), resolved);
    }
    (!values.is_empty()).then_some(values)
}

impl ThinkingTokenBudgetField {
    fn as_str(self) -> &'static str {
        match self {
            Self::TokenBudget => "thinking_token_budget",
            Self::Budget => "thinking_budget",
            Self::BudgetTokens => "thinking_budget_tokens",
        }
    }
}

fn thinking_budget(request: &ProviderRequest) -> Option<u64> {
    if request.thinking_level == ThinkingLevel::Off
        || !request
            .model_spec
            .as_ref()
            .is_some_and(|model| model.reasoning)
    {
        return None;
    }
    let budget: u64 = match request.thinking_level {
        ThinkingLevel::Minimal => 1_024,
        ThinkingLevel::Low => 2_048,
        ThinkingLevel::Medium => 8_192,
        ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => 16_384,
        ThinkingLevel::Off => return None,
    };
    let ceiling = request
        .max_output_tokens
        .or_else(|| request.model_spec.as_ref().map(|model| model.max_tokens))?;
    let clamped = budget.min(ceiling.saturating_sub(1_024));
    (clamped > 0).then_some(clamped)
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::Assistant(message) => message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall(_))),
        Message::ToolResult(_) => true,
        Message::User(_) | Message::Custom(_) => false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheRetention {
    None,
    Short,
    Long,
}

fn cache_retention() -> CacheRetention {
    match std::env::var("PI_CACHE_RETENTION").as_deref() {
        Ok("none") => CacheRetention::None,
        Ok("long") => CacheRetention::Long,
        _ => CacheRetention::Short,
    }
}

fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: &mut [Value],
    cache_control: &Value,
) {
    if let Some(message) = messages.iter_mut().find(|message| {
        matches!(
            message.get("role").and_then(Value::as_str),
            Some("system" | "developer")
        )
    }) {
        add_cache_control_to_content(message, cache_control);
    }
    if let Some(tool) = tools.last_mut() {
        tool["cache_control"] = cache_control.clone();
    }
    if let Some(message) = messages.iter_mut().rev().find(|message| {
        matches!(
            message.get("role").and_then(Value::as_str),
            Some("user" | "assistant" | "tool")
        )
    }) {
        add_cache_control_to_content(message, cache_control);
    }
}

fn add_cache_control_to_content(message: &mut Value, cache_control: &Value) {
    let Some(content) = message.get_mut("content") else {
        return;
    };
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        *content = json!([{
            "type": "text",
            "text": text,
            "cache_control": cache_control
        }]);
        return;
    }
    if let Some(parts) = content.as_array_mut()
        && let Some(text) = parts
            .iter_mut()
            .rev()
            .find(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    {
        text["cache_control"] = cache_control.clone();
    }
}

fn detect_compat(model: Option<&ModelSpec>) -> ResolvedOpenAiCompletionsCompat {
    let provider = model.map_or("", |model| model.provider.as_str());
    let base_url = model
        .and_then(|model| model.base_url.as_deref())
        .unwrap_or_default();
    let id = model.map_or("", |model| model.id.as_str());
    let is_zai = matches!(provider, "zai" | "zai-coding-cn")
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot =
        matches!(provider, "moonshotai" | "moonshotai-cn") || base_url.contains("api.moonshot.");
    let is_openrouter = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_deepseek =
        provider == "deepseek" || base_url.to_ascii_lowercase().contains("deepseek.com");
    let is_non_standard = is_nvidia
        || provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers
        || is_cloudflare_gateway
        || is_ant_ling;
    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cloudflare_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;
    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let developer_on_openrouter =
        is_openrouter && (id.starts_with("anthropic/") || id.starts_with("openai/"));
    ResolvedOpenAiCompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: developer_on_openrouter || (!is_non_standard && !is_openrouter),
        supports_reasoning_effort: !(is_grok
            || is_zai
            || is_moonshot
            || is_together
            || is_cloudflare_gateway
            || is_nvidia
            || is_ant_ling),
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_openrouter {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        chat_template_kwargs: BTreeMap::new(),
        chat_template_args: BTreeMap::new(),
        cache_control_format: (provider == "openrouter" && id.starts_with("anthropic/"))
            .then_some(CacheControlFormat::Anthropic),
        open_router_routing: None,
        vercel_gateway_routing: None,
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        thinking_token_budget_field: None,
        supports_strict_mode: !(is_moonshot || is_together || is_cloudflare_gateway || is_nvidia),
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: if is_openrouter {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers
            || is_cloudflare_gateway
            || is_nvidia
            || is_ant_ling),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{
        ImageContent, ModelId, TextContent, ToolCallId, ToolExecutionMode, ToolResultMessage,
        Usage, UserMessage,
    };
    use std::sync::Arc;

    #[test]
    fn converts_user_image_to_data_url() {
        let request = ProviderRequest {
            model: ModelId::new("gpt"),
            model_spec: None,
            system_prompt: String::new(),
            tools: vec![],
            thinking_level: pi_core::ThinkingLevel::Off,
            max_output_tokens: Some(123),
            headers: Default::default(),
            sampling_params: std::collections::BTreeMap::from([(
                "temperature".to_string(),
                json!(0.4),
            )]),
            session_id: None,
            messages: vec![Message::User(pi_core::UserMessage {
                content: vec![
                    ContentBlock::Text(pi_core::TextContent::new("look")),
                    ContentBlock::Image(ImageContent {
                        data: "YWJj".to_string(),
                        mime_type: "image/png".to_string(),
                    }),
                ],
                timestamp_ms: 0,
            })],
        };
        let body = request_body(&request);
        assert_eq!(body["max_completion_tokens"], 123);
        assert_eq!(body["temperature"], 0.4);
        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,YWJj"
        );
    }

    #[test]
    fn defers_tool_images_until_every_result_in_the_batch_is_serialized() {
        let request = ProviderRequest {
            model: ModelId::new("gpt"),
            model_spec: None,
            system_prompt: String::new(),
            tools: vec![],
            thinking_level: pi_core::ThinkingLevel::Off,
            max_output_tokens: None,
            headers: Default::default(),
            sampling_params: Default::default(),
            session_id: None,
            messages: vec![
                Message::ToolResult(Arc::new(ToolResultMessage {
                    tool_call_id: ToolCallId::new("image"),
                    tool_name: "read".to_string(),
                    content: vec![ContentBlock::Image(ImageContent {
                        data: "YWJj".to_string(),
                        mime_type: "image/png".to_string(),
                    })],
                    details: None,
                    usage: Some(Usage::default()),
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: 0,
                })),
                Message::ToolResult(Arc::new(ToolResultMessage {
                    tool_call_id: ToolCallId::new("text"),
                    tool_name: "bash".to_string(),
                    content: vec![ContentBlock::Text(TextContent::new("passed"))],
                    details: None,
                    usage: Some(Usage::default()),
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: 0,
                })),
            ],
        };
        let body = request_body(&request);
        assert_eq!(body["messages"][0]["role"], "tool");
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][1]["type"], "image_url");
    }

    #[test]
    fn models_json_compat_controls_chat_request_shape() {
        let mut model = ModelSpec::new(
            "custom",
            "reasoning-model",
            "Reasoning Model",
            "openai-completions",
        );
        model.reasoning = true;
        model
            .thinking_level_map
            .insert("high".to_string(), Some("provider-high".to_string()));
        model.compat = Some(json!({
            "supportsStore": false,
            "supportsDeveloperRole": false,
            "supportsReasoningEffort": false,
            "supportsUsageInStreaming": false,
            "maxTokensField": "max_tokens",
            "requiresToolResultName": true,
            "requiresAssistantAfterToolResult": true,
            "thinkingFormat": "qwen",
            "openRouterRouting": {"only": ["test"]},
            "vercelGatewayRouting": {"order": ["test"]},
            "zaiToolStream": true,
            "thinkingTokenBudgetField": "thinking_budget_tokens",
            "supportsStrictMode": false,
            "sendSessionAffinityHeaders": true,
            "sessionAffinityFormat": "openai-nosession"
        }));
        let request = ProviderRequest {
            model: ModelId::new("reasoning-model"),
            model_spec: Some(model),
            system_prompt: "system".to_string(),
            messages: vec![
                Message::ToolResult(Arc::new(ToolResultMessage {
                    tool_call_id: ToolCallId::new("call"),
                    tool_name: "lookup".to_string(),
                    content: vec![ContentBlock::Text(TextContent::new("result"))],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: 0,
                })),
                Message::User(UserMessage::text("continue", 0)),
            ],
            tools: vec![ToolSpec {
                name: "lookup".to_string(),
                label: "Lookup".to_string(),
                description: "lookup".to_string(),
                parameters: json!({"type": "object"}),
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            }],
            thinking_level: ThinkingLevel::High,
            max_output_tokens: Some(4_096),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: Some("session-1".to_string()),
        };
        let body = request_body(&request);
        let headers = affinity_headers(&request);

        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["name"], "lookup");
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(body["messages"][3]["role"], "user");
        assert!(body.get("stream_options").is_none());
        assert!(body.get("store").is_none());
        assert_eq!(body["max_tokens"], 4_096);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["thinking_budget_tokens"], 3_072);
        assert_eq!(body["tool_stream"], true);
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["provider"]["only"][0], "test");
        assert_eq!(body["providerOptions"]["gateway"]["order"][0], "test");
        assert!(body["tools"][0]["function"].get("strict").is_none());
        assert_eq!(headers["x-client-request-id"], "session-1");
        assert_eq!(headers["x-session-affinity"], "session-1");
        assert!(!headers.contains_key("session_id"));
    }

    #[test]
    fn chat_template_compat_resolves_pi_thinking_variables() {
        let mut model = ModelSpec::new(
            "custom",
            "template-model",
            "Template Model",
            "openai-completions",
        );
        model.reasoning = true;
        model.compat = Some(json!({
            "thinkingFormat": "chat-template",
            "chatTemplateKwargs": {
                "enable_thinking": {"$var": "thinking.enabled"},
                "effort": {"$var": "thinking.effort"},
                "budget": {"$var": "thinking.budget"},
                "literal": 7
            }
        }));
        let request = ProviderRequest {
            model: ModelId::new("template-model"),
            model_spec: Some(model),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking_level: ThinkingLevel::Medium,
            max_output_tokens: None,
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let body = request_body(&request);

        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["chat_template_kwargs"]["effort"], "medium");
        assert_eq!(body["chat_template_kwargs"]["budget"], 8_192);
        assert_eq!(body["chat_template_kwargs"]["literal"], 7);
    }
}
