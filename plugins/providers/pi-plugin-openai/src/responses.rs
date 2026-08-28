#![forbid(unsafe_code)]

//! Shared OpenAI Responses wire projection and SSE stream adaptation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, ContentMetadata, Message, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderRequest, ProviderStream,
    ResponseMetadata, ResponseMetadataPatch, StopReason, StreamEvent, ToolCallId, ToolSpec, Usage,
};
use pi_provider::{
    HttpBodyStream, HttpTransport, ReqwestTransport, SseDecoder, TransportError,
    collect_body_limited,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{OpenAiCompatibleConfig, validate_config};
use crate::request::SessionAffinityFormat;

pub const OPENAI_RESPONSES_API: &str = "openai-responses";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
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
}

#[derive(Debug, Clone)]
struct ResolvedOpenAiResponsesCompat {
    supports_developer_role: bool,
    session_affinity_format: SessionAffinityFormat,
    supports_long_cache_retention: bool,
    supports_strict_mode: bool,
    supports_additional_tools: bool,
    supports_tool_search: bool,
    supports_explicit_prompt_cache_mode: bool,
}

impl ResolvedOpenAiResponsesCompat {
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
            supports_additional_tools: configured.supports_additional_tools.unwrap_or(false),
            supports_tool_search: configured.supports_tool_search.unwrap_or(false),
            supports_explicit_prompt_cache_mode: configured
                .supports_explicit_prompt_cache_mode
                .unwrap_or(false),
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

    fn headers(&self, request: &ProviderRequest) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        if let Some(key) = &self.config.api_key {
            insert_header(&mut headers, "Authorization", format!("Bearer {key}"));
        }
        for (name, value) in response_affinity_headers(request) {
            insert_header(&mut headers, name, value);
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
        let headers = self.headers(&request);
        let endpoint = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .map(responses_endpoint)
            .unwrap_or_else(|| self.endpoint());
        let payload = context
            .before_provider_request(&signal, request_body(&request))
            .await?;
        let response = self
            .transport
            .post_json(&endpoint, &headers, &payload, signal.clone())
            .await
            .map_err(map_transport_error)?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!("HTTP {status}: {body}")));
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

        Ok(stream(
            self.config.provider_id.clone(),
            request.model,
            OPENAI_RESPONSES_API,
            response.body,
            signal,
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

/// Projects semantic messages into OpenAI Responses input items.
pub fn input_items(messages: &[Message]) -> Vec<Value> {
    messages.iter().flat_map(message_input_items).collect()
}

/// Projects semantic tools into OpenAI Responses function definitions.
pub fn tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools_with_compat(tools, false, false)
}

fn tools_with_compat(
    tools: &[ToolSpec],
    supports_strict_mode: bool,
    defer_loading: bool,
) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut value = json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters
            });
            if supports_strict_mode {
                value["strict"] = Value::Bool(false);
            }
            if defer_loading {
                value["defer_loading"] = Value::Bool(true);
            }
            value
        })
        .collect()
}

/// Projects a semantic request into an OpenAI Responses payload.
pub fn request_body(request: &ProviderRequest) -> Value {
    request_body_with_cache_retention(request, cache_retention())
}

fn request_body_with_cache_retention(
    request: &ProviderRequest,
    cache_retention: CacheRetention,
) -> Value {
    let compat = ResolvedOpenAiResponsesCompat::for_request(request);
    let mut input = Vec::new();
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
        input.push(json!({"role": role, "content": request.system_prompt}));
    }
    input.extend(input_items_with_deferred_tools(request, &compat));
    let deferred_names = deferred_tool_names(request, &compat);
    let immediate_tools = request
        .tools
        .iter()
        .filter(|tool| !deferred_names.contains(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    let tools = tools_with_compat(&immediate_tools, compat.supports_strict_mode, false);
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
                    "effort": effort,
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
    if let Some(max_tokens) = request.max_output_tokens {
        payload["max_output_tokens"] = json!(max_tokens.max(16));
    }
    match cache_retention {
        CacheRetention::None if compat.supports_explicit_prompt_cache_mode => {
            payload["prompt_cache_options"] = json!({"mode": "explicit"});
        }
        CacheRetention::None => {}
        CacheRetention::Short => {
            if let Some(session_id) = &request.session_id {
                payload["prompt_cache_key"] = Value::String(session_id.clone());
            }
        }
        CacheRetention::Long => {
            if let Some(session_id) = &request.session_id {
                payload["prompt_cache_key"] = Value::String(session_id.clone());
            }
            if compat.supports_long_cache_retention {
                payload["prompt_cache_retention"] = Value::String("24h".to_string());
            }
        }
    }
    // models.json samplingParams are applied last so user keys win.
    if let Value::Object(payload) = &mut payload {
        payload.extend(request.sampling_params.clone());
    }
    payload
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
) -> Vec<Value> {
    let mut items = Vec::new();
    let mut loaded = std::collections::HashSet::new();
    let by_name = request
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<HashMap<_, _>>();
    for message in &request.messages {
        items.extend(message_input_items(message));
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
                "tools": tools_with_compat(&deferred, compat.supports_strict_mode, false)
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
                "tools": tools_with_compat(&deferred, compat.supports_strict_mode, true)
            }));
        }
    }
    items
}

/// Adapts an accepted OpenAI Responses SSE body into the semantic provider stream.
pub fn stream(
    provider: ProviderId,
    model: pi_core::ModelId,
    api: impl Into<String>,
    mut body: HttpBodyStream,
    signal: AbortSignal,
) -> ProviderStream {
    let api = api.into();
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(provider, model.clone(), api, now_ms()),
        });
        let mut decoder = SseDecoder::new();
        let mut state = StreamState::new(&model);
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
                    return;
                }
            }
        }
    })
}

fn message_input_items(message: &Message) -> Vec<Value> {
    match message {
        Message::User(message) => vec![json!({
            "type": "message",
            "role": "user",
            "content": message.content.iter().filter_map(input_content).collect::<Vec<_>>()
        })],
        Message::Custom(message) => vec![json!({
            "type": "message",
            "role": "user",
            "content": message
                .content
                .to_blocks()
                .iter()
                .filter_map(input_content)
                .collect::<Vec<_>>()
        })],
        Message::Assistant(message) => {
            let mut items = Vec::new();
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                items.push(json!({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": text}]
                }));
            }
            items.extend(message.content.iter().filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(json!({
                    "type": "function_call",
                    "call_id": call.id.as_str(),
                    "name": call.name,
                    "arguments": call.arguments.to_string()
                })),
                _ => None,
            }));
            items
        }
        Message::ToolResult(message) => vec![json!({
            "type": "function_call_output",
            "call_id": message.tool_call_id.as_str(),
            "output": message.content.iter().filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                ContentBlock::Thinking(text) => Some(text.thinking.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n")
        })],
    }
}

fn input_content(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text(text) => Some(json!({"type": "input_text", "text": text.text})),
        ContentBlock::Image(image) => Some(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
        })),
        ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => None,
    }
}

struct StreamState {
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tools: HashMap<String, usize>,
    had_tool_call: bool,
    next_index: usize,
    closed: bool,
    requested_model: Option<String>,
    pending_error: Option<ProviderError>,
}

impl StreamState {
    fn new(model: &pi_core::ModelId) -> Self {
        Self {
            text_index: None,
            thinking_index: None,
            tools: HashMap::new(),
            had_tool_call: false,
            next_index: 0,
            closed: false,
            requested_model: Some(model.as_str().to_string()),
            pending_error: None,
        }
    }

    fn take_pending_error(&mut self) -> Option<ProviderError> {
        self.pending_error.take()
    }

    fn consume(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|error| {
            ProviderError::Protocol(format!("invalid OpenAI Responses SSE JSON: {error}"))
        })?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut events = Vec::new();
        if matches!(
            kind,
            "response.created"
                | "response.completed"
                | "response.done"
                | "response.incomplete"
                | "response.failed"
        ) {
            let response = value.get("response").unwrap_or(&Value::Null);
            let status = response.get("status").and_then(Value::as_str);
            let incomplete_reason = response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str);
            let terminal = kind != "response.created";
            let patch = ResponseMetadataPatch {
                response_model: response
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|model| self.requested_model.as_deref() != Some(*model))
                    .map(str::to_string),
                response_id: response
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                raw_stop_reason: terminal
                    .then(|| match (status, incomplete_reason) {
                        (Some(status), Some(reason)) => Some(format!("{status}.{reason}")),
                        (Some(status), None) => Some(status.to_string()),
                        (None, Some(reason)) => Some(reason.to_string()),
                        (None, None) => None,
                    })
                    .flatten(),
                end_turn: response.get("end_turn").and_then(Value::as_bool),
                ..ResponseMetadataPatch::default()
            };
            if patch.response_model.is_some()
                || patch.response_id.is_some()
                || patch.raw_stop_reason.is_some()
                || patch.end_turn.is_some()
            {
                events.push(StreamEvent::Metadata { patch });
            }
        }
        match kind {
            "response.output_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !delta.is_empty() {
                    let index = *self.text_index.get_or_insert_with(|| {
                        let index = self.next_index;
                        self.next_index += 1;
                        events.push(StreamEvent::TextStart {
                            content_index: index,
                        });
                        index
                    });
                    events.push(StreamEvent::TextDelta {
                        content_index: index,
                        delta: delta.to_string(),
                    });
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !delta.is_empty() {
                    let index = *self.thinking_index.get_or_insert_with(|| {
                        let index = self.next_index;
                        self.next_index += 1;
                        events.push(StreamEvent::ThinkingStart {
                            content_index: index,
                        });
                        index
                    });
                    events.push(StreamEvent::ThinkingDelta {
                        content_index: index,
                        delta: delta.to_string(),
                    });
                }
            }
            "response.output_item.added" => {
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let item_id = value
                        .pointer("/item/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let call_id = value
                        .pointer("/item/call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(item_id);
                    let name = value
                        .pointer("/item/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let index = self.next_index;
                    self.next_index += 1;
                    self.had_tool_call = true;
                    self.tools.insert(item_id.to_string(), index);
                    events.push(StreamEvent::ToolCallStart {
                        content_index: index,
                        id: ToolCallId::new(call_id),
                        name: name.to_string(),
                    });
                    if let Some(namespace) =
                        value.pointer("/item/namespace").and_then(Value::as_str)
                    {
                        events.push(StreamEvent::ContentMetadata {
                            content_index: index,
                            metadata: ContentMetadata::ToolCall {
                                namespace: Some(namespace.to_string()),
                            },
                        });
                    }
                    if let Some(arguments) = value
                        .pointer("/item/arguments")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        events.push(StreamEvent::ToolCallDelta {
                            content_index: index,
                            arguments_delta: arguments.to_string(),
                        });
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(index) = self.tools.get(item_id).copied() {
                    let delta = value
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !delta.is_empty() {
                        events.push(StreamEvent::ToolCallDelta {
                            content_index: index,
                            arguments_delta: delta.to_string(),
                        });
                    }
                }
            }
            "response.output_item.done" => {
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let item_id = value
                        .pointer("/item/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if let Some(index) = self.tools.remove(item_id) {
                        if let Some(namespace) =
                            value.pointer("/item/namespace").and_then(Value::as_str)
                        {
                            events.push(StreamEvent::ContentMetadata {
                                content_index: index,
                                metadata: ContentMetadata::ToolCall {
                                    namespace: Some(namespace.to_string()),
                                },
                            });
                        }
                        events.push(StreamEvent::ToolCallEnd {
                            content_index: index,
                            thought_signature: None,
                        });
                    }
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                let reason = if kind == "response.incomplete" {
                    StopReason::Length
                } else if self.had_tool_call {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                };
                let usage = response_usage(value.get("response").unwrap_or(&Value::Null));
                events.extend(self.close());
                events.push(StreamEvent::Done { reason, usage });
            }
            "response.failed" | "error" => {
                let message = value
                    .pointer("/response/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Responses request failed");
                self.pending_error = Some(ProviderError::Failure(message.to_string()));
            }
            _ => {}
        }
        Ok(events)
    }

    fn close(&mut self) -> Vec<StreamEvent> {
        if self.closed {
            return Vec::new();
        }
        self.closed = true;
        let mut events = Vec::new();
        if let Some(index) = self.thinking_index {
            events.push(StreamEvent::ThinkingEnd {
                content_index: index,
                thinking_signature: None,
            });
        }
        if let Some(index) = self.text_index {
            events.push(StreamEvent::TextEnd {
                content_index: index,
                text_signature: None,
            });
        }
        for (_, index) in self.tools.drain() {
            events.push(StreamEvent::ToolCallEnd {
                content_index: index,
                thought_signature: None,
            });
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = self.close();
        if !events.is_empty() {
            events.push(StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            });
        }
        events
    }
}

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

fn response_affinity_headers(request: &ProviderRequest) -> BTreeMap<String, String> {
    let Some(session_id) = &request.session_id else {
        return BTreeMap::new();
    };
    match ResolvedOpenAiResponsesCompat::for_request(request).session_affinity_format {
        SessionAffinityFormat::Openrouter => {
            BTreeMap::from([("x-session-id".to_string(), session_id.clone())])
        }
        SessionAffinityFormat::Openai => BTreeMap::from([
            ("session_id".to_string(), session_id.clone()),
            ("x-client-request-id".to_string(), session_id.clone()),
        ]),
        SessionAffinityFormat::OpenaiNosession => {
            BTreeMap::from([("x-client-request-id".to_string(), session_id.clone())])
        }
    }
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        TransportError::InvalidConfiguration(message) | TransportError::InvalidSse(message) => {
            ProviderError::Protocol(message)
        }
        other => ProviderError::Failure(other.to_string()),
    }
}

fn insert_header(
    headers: &mut BTreeMap<String, String>,
    name: impl AsRef<str>,
    value: impl Into<String>,
) {
    let name = name.as_ref();
    if let Some(existing) = headers
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.into());
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use pi_core::{AbortHandle, ModelId, ModelSpec, ProviderRequest, ThinkingLevel, UserMessage};

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
            max_output_tokens: None,
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: Some("session-1".to_string()),
        };

        let body = request_body_with_cache_retention(&request, CacheRetention::None);
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");
        assert!(body.get("prompt_cache_key").is_none());
        assert_eq!(
            response_affinity_headers(&request)["x-session-id"],
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
