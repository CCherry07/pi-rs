#![forbid(unsafe_code)]

//! Mistral Conversations provider, catalog, and reusable wire adapter.

mod catalog;

pub use catalog::mistral_models;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, Message, ModelInput, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream, ResponseMetadata, ResponseMetadataPatch, StopReason,
    StreamEvent, ThinkingLevel, ToolCallId, Usage,
};
use pi_provider::{
    HttpTransport, ReqwestTransport, SseDecoder, TransportError, collect_body_limited,
    post_json_with_provider_hooks,
};
use serde_json::{Value, json};

pub const MISTRAL_CONVERSATIONS_API: &str = "mistral-conversations";
pub const MISTRAL_PROVIDER_ID: &str = "mistral";
pub const MISTRAL_BASE_URL: &str = "https://api.mistral.ai";

pub struct MistralPlugin {
    provider: Arc<MistralCompatibleProvider>,
}

impl MistralPlugin {
    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::new(std::env::var("MISTRAL_API_KEY").ok().or(api_key))
    }

    pub fn from_stored_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_transport(std::env::var("MISTRAL_API_KEY").ok().or(api_key), transport)
    }

    pub fn new(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::new_with_transport(api_key, Arc::new(ReqwestTransport::new()))
    }

    pub fn new_with_transport(
        api_key: Option<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        let config = api_key.map_or_else(
            || MistralCompatibleConfig::without_api_key(MISTRAL_BASE_URL),
            |api_key| MistralCompatibleConfig::new(MISTRAL_BASE_URL, api_key),
        );
        Ok(Self {
            provider: Arc::new(MistralCompatibleProvider::with_transport(
                config, transport,
            )?),
        })
    }
}

#[pi_core::provider_plugin]
impl ProviderPlugin for MistralPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("mistral-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in mistral_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct MistralCompatibleConfig {
    pub provider_id: ProviderId,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl MistralCompatibleConfig {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new(MISTRAL_PROVIDER_ID),
            base_url: base_url.into(),
            api_key: Some(api_key.into()),
            headers: BTreeMap::new(),
        }
    }

    pub fn without_api_key(base_url: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new(MISTRAL_PROVIDER_ID),
            base_url: base_url.into(),
            api_key: None,
            headers: BTreeMap::new(),
        }
    }

    pub fn provider_id(mut self, provider_id: impl Into<ProviderId>) -> Self {
        self.provider_id = provider_id.into();
        self
    }
}

pub struct MistralCompatibleProvider {
    config: MistralCompatibleConfig,
    transport: Arc<dyn HttpTransport>,
}

impl MistralCompatibleProvider {
    pub fn new(config: MistralCompatibleConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: MistralCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        if config.base_url.trim().is_empty() {
            return Err(ProviderError::Failure(
                "Mistral base URL cannot be empty".to_string(),
            ));
        }
        if config
            .api_key
            .as_ref()
            .is_some_and(|key| key.contains(['\r', '\n']))
        {
            return Err(ProviderError::Failure(
                "invalid Mistral API key".to_string(),
            ));
        }
        Ok(Self { config, transport })
    }

    fn headers(&self, request: &ProviderRequest) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        insert_header(&mut headers, "User-Agent", "pi-rs");
        if let Some(api_key) = &self.config.api_key {
            insert_header(&mut headers, "Authorization", format!("Bearer {api_key}"));
        }
        if let Some(session_id) = &request.session_id
            && !contains_header(&headers, "x-affinity")
            && !contains_header(&request.headers, "x-affinity")
        {
            insert_header(&mut headers, "x-affinity", session_id);
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }
}

#[async_trait]
impl Provider for MistralCompatibleProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id.clone()
    }

    fn name(&self) -> String {
        if self.config.provider_id.as_str() == MISTRAL_PROVIDER_ID {
            "Mistral".to_string()
        } else {
            self.config.provider_id.to_string()
        }
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
        let endpoint = request
            .model_spec
            .as_ref()
            .and_then(|model| model.base_url.as_deref())
            .map(mistral_endpoint)
            .unwrap_or_else(|| mistral_endpoint(&self.config.base_url));
        let headers = self.headers(&request);
        let payload = context
            .before_provider_request(&signal, request_body(&request))
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
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!(
                "Mistral API error ({status}): {body}"
            )));
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

        Ok(mistral_stream(
            self.config.provider_id.clone(),
            request.model,
            response.body,
            signal,
        ))
    }
}

pub fn request_body(request: &ProviderRequest) -> Value {
    let supports_images = request
        .model_spec
        .as_ref()
        .is_some_and(|model| model.input.contains(&ModelInput::Image));
    let mut normalizer = ToolCallIdNormalizer::default();
    let mut messages = Vec::new();
    if !request.system_prompt.is_empty() {
        messages.push(json!({"role": "system", "content": request.system_prompt}));
    }
    for message in &request.messages {
        if let Some(message) = project_message(message, supports_images, &mut normalizer) {
            messages.push(message);
        }
    }
    let mut payload = json!({
        "model": request.model.as_str(),
        "stream": true,
        "messages": messages
    });
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                            "strict": false
                        }
                    })
                })
                .collect(),
        );
    }
    if let Some(max_tokens) = request.max_output_tokens {
        payload["max_tokens"] = json!(max_tokens);
    }
    if let Some(session_id) = &request.session_id {
        payload["prompt_cache_key"] = Value::String(session_id.clone());
    }
    if request
        .model_spec
        .as_ref()
        .is_some_and(|model| model.reasoning)
        && request.thinking_level != ThinkingLevel::Off
    {
        if uses_reasoning_effort(request.model.as_str()) {
            let effort = request
                .model_spec
                .as_ref()
                .and_then(|model| {
                    model
                        .thinking_level_map
                        .get(request.thinking_level.as_str())
                })
                .and_then(Clone::clone)
                .unwrap_or_else(|| "high".to_string());
            payload["reasoning_effort"] = Value::String(effort);
        } else {
            payload["prompt_mode"] = Value::String("reasoning".to_string());
        }
    }
    if let Value::Object(payload) = &mut payload {
        payload.extend(request.sampling_params.clone());
    }
    payload
}

fn project_message(
    message: &Message,
    supports_images: bool,
    normalizer: &mut ToolCallIdNormalizer,
) -> Option<Value> {
    match message {
        Message::User(message) => project_user_blocks(&message.content, supports_images),
        Message::Custom(message) => {
            project_user_blocks(&message.content.to_blocks(), supports_images)
        }
        Message::Assistant(message) => {
            let mut content = Vec::new();
            let mut calls = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text(text) if !text.text.trim().is_empty() => {
                        content.push(json!({"type": "text", "text": text.text}));
                    }
                    ContentBlock::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                        content.push(json!({
                            "type": "thinking",
                            "thinking": [{"type": "text", "text": thinking.thinking}]
                        }));
                    }
                    ContentBlock::ToolCall(call) => {
                        let id = normalizer.normalize(call.id.as_str());
                        calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.arguments.to_string()
                            },
                            "index": 0
                        }));
                    }
                    ContentBlock::Image(_) | ContentBlock::Text(_) | ContentBlock::Thinking(_) => {}
                }
            }
            if content.is_empty() && calls.is_empty() {
                return None;
            }
            let mut value = json!({"role": "assistant", "prefix": false});
            if !content.is_empty() {
                value["content"] = Value::Array(content);
            }
            if !calls.is_empty() {
                value["tool_calls"] = Value::Array(calls);
            }
            Some(value)
        }
        Message::ToolResult(message) => {
            let mut content = vec![json!({
                "type": "text",
                "text": tool_result_text(message, supports_images)
            })];
            if supports_images {
                content.extend(message.content.iter().filter_map(|block| match block {
                    ContentBlock::Image(image) => Some(json!({
                        "type": "image_url",
                        "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
                    })),
                    _ => None,
                }));
            }
            Some(json!({
                "role": "tool",
                "tool_call_id": normalizer.normalize(message.tool_call_id.as_str()),
                "name": message.tool_name,
                "content": content
            }))
        }
    }
}

fn project_user_blocks(blocks: &[ContentBlock], supports_images: bool) -> Option<Value> {
    let had_images = blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(_)));
    let mut content = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(json!({"type": "text", "text": text.text})),
            ContentBlock::Image(image) if supports_images => Some(json!({
                "type": "image_url",
                "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
            })),
            ContentBlock::Image(_) | ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => None,
        })
        .collect::<Vec<_>>();
    if content.is_empty() && had_images && !supports_images {
        content.push(json!({
            "type": "text",
            "text": "(image omitted: model does not support images)"
        }));
    }
    (!content.is_empty()).then(|| json!({"role": "user", "content": content}))
}

fn tool_result_text(message: &pi_core::ToolResultMessage, supports_images: bool) -> String {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let has_images = message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(_)));
    let prefix = if message.is_error {
        "[tool error] "
    } else {
        ""
    };
    if !text.trim().is_empty() {
        let suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{prefix}{}{suffix}", text.trim());
    }
    match (has_images, supports_images, message.is_error) {
        (true, true, true) => "[tool error] (see attached image)".to_string(),
        (true, true, false) => "(see attached image)".to_string(),
        (true, false, true) => {
            "[tool error] (image omitted: model does not support images)".to_string()
        }
        (true, false, false) => "(image omitted: model does not support images)".to_string(),
        (false, _, true) => "[tool error] (no tool output)".to_string(),
        (false, _, false) => "(no tool output)".to_string(),
    }
}

fn uses_reasoning_effort(model: &str) -> bool {
    matches!(
        model,
        "mistral-small-2603" | "mistral-small-latest" | "mistral-medium-3.5"
    )
}

fn mistral_endpoint(base: &str) -> String {
    let base = base.trim();
    let suffix_start = [base.find('?'), base.find('#')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(base.len());
    let (path, suffix) = base.split_at(suffix_start);
    let path = path.trim_end_matches('/');
    if path.ends_with("/chat/completions") {
        format!("{path}{suffix}")
    } else if path.ends_with("/v1") {
        format!("{path}/chat/completions{suffix}")
    } else {
        format!("{path}/v1/chat/completions{suffix}")
    }
}

fn mistral_stream(
    provider: ProviderId,
    model: pi_core::ModelId,
    mut body: pi_provider::HttpBodyStream,
    signal: AbortSignal,
) -> ProviderStream {
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(
                provider,
                model.clone(),
                MISTRAL_CONVERSATIONS_API,
                now_ms(),
            ),
        });
        let mut decoder = SseDecoder::new();
        let mut state = MistralStreamState::new(model.as_str());
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
                            match state.finish() {
                                Ok(events) => for event in events { yield Ok(event); },
                                Err(error) => yield Err(error),
                            }
                            return;
                        }
                        match state.consume(&event.data) {
                            Ok(events) => for event in events { yield Ok(event); },
                            Err(error) => { yield Err(error); return; }
                        }
                    }
                }
                Some(Err(error)) => { yield Err(map_transport_error(error)); return; }
                None => {
                    match decoder.finish() {
                        Ok(Some(event)) if event.data != "[DONE]" => match state.consume(&event.data) {
                            Ok(events) => for event in events { yield Ok(event); },
                            Err(error) => { yield Err(error); return; }
                        },
                        Ok(_) => {}
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    }
                    match state.finish() {
                        Ok(events) => for event in events { yield Ok(event); },
                        Err(error) => yield Err(error),
                    }
                    return;
                }
            }
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
}

struct ActiveBlock {
    kind: BlockKind,
    index: usize,
}

struct MistralStreamState {
    requested_model: String,
    active: Option<ActiveBlock>,
    tools: HashMap<String, usize>,
    next_index: usize,
    reason: Option<StopReason>,
    raw_reason: Option<String>,
    usage: Usage,
    closed: bool,
}

impl MistralStreamState {
    fn new(model: &str) -> Self {
        Self {
            requested_model: model.to_string(),
            active: None,
            tools: HashMap::new(),
            next_index: 0,
            reason: None,
            raw_reason: None,
            usage: Usage::default(),
            closed: false,
        }
    }

    fn consume(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|error| {
            ProviderError::Protocol(format!("invalid Mistral SSE JSON: {error}"))
        })?;
        let mut events = Vec::new();
        let response_id = value.get("id").and_then(Value::as_str).map(str::to_string);
        let response_model = value
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| *model != self.requested_model)
            .map(str::to_string);
        if response_id.is_some() || response_model.is_some() {
            events.push(StreamEvent::Metadata {
                patch: ResponseMetadataPatch {
                    response_id,
                    response_model,
                    ..ResponseMetadataPatch::default()
                },
            });
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = parse_usage(usage);
        }
        let Some(choice) = value.pointer("/choices/0") else {
            return Ok(events);
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.raw_reason = Some(reason.to_string());
            self.reason = Some(match reason {
                "stop" => StopReason::Stop,
                "length" | "model_length" => StopReason::Length,
                "tool_calls" => StopReason::ToolUse,
                "error" => StopReason::Error,
                _ => StopReason::Error,
            });
            events.push(StreamEvent::Metadata {
                patch: ResponseMetadataPatch {
                    raw_stop_reason: Some(reason.to_string()),
                    ..ResponseMetadataPatch::default()
                },
            });
        }
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(content) = delta.get("content").filter(|content| !content.is_null()) {
            match content {
                Value::String(text) => self.push_delta(BlockKind::Text, text, &mut events),
                Value::Array(items) => {
                    for item in items {
                        match item.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                let text = item
                                    .get("thinking")
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                                    .collect::<String>();
                                self.push_delta(BlockKind::Thinking, &text, &mut events);
                            }
                            Some("text") => {
                                let text =
                                    item.get("text").and_then(Value::as_str).unwrap_or_default();
                                self.push_delta(BlockKind::Text, text, &mut events);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            self.close_active(&mut events);
            for call in calls {
                let source_index = call.get("index").and_then(Value::as_u64);
                let supplied_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                let id = if supplied_id.is_empty() || supplied_id == "null" {
                    derive_tool_call_id(
                        &format!("toolcall:{}", source_index.unwrap_or_default()),
                        0,
                    )
                } else {
                    supplied_id.to_string()
                };
                // Mistral can include the id only on the first delta. Current Pi
                // keys streamed calls by index when one is present, falling back
                // to the id only for index-less events.
                let key = source_index
                    .map(|index| format!("index:{index}"))
                    .unwrap_or_else(|| format!("id:{id}"));
                let content_index = match self.tools.get(&key).copied() {
                    Some(index) => index,
                    None => {
                        let index = self.next_index;
                        self.next_index += 1;
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        events.push(StreamEvent::ToolCallStart {
                            content_index: index,
                            id: ToolCallId::new(id),
                            name: name.to_string(),
                        });
                        self.tools.insert(key.clone(), index);
                        index
                    }
                };
                if let Some(arguments) = call.pointer("/function/arguments") {
                    let delta = match arguments {
                        Value::String(value) => value.clone(),
                        Value::Null => String::new(),
                        value => value.to_string(),
                    };
                    if !delta.is_empty() {
                        events.push(StreamEvent::ToolCallDelta {
                            content_index,
                            arguments_delta: delta,
                        });
                    }
                }
            }
        }
        Ok(events)
    }

    fn push_delta(&mut self, kind: BlockKind, delta: &str, events: &mut Vec<StreamEvent>) {
        if delta.is_empty() {
            return;
        }
        if self.active.as_ref().is_none_or(|block| block.kind != kind) {
            self.close_active(events);
            let index = self.next_index;
            self.next_index += 1;
            events.push(match kind {
                BlockKind::Text => StreamEvent::TextStart {
                    content_index: index,
                },
                BlockKind::Thinking => StreamEvent::ThinkingStart {
                    content_index: index,
                },
            });
            self.active = Some(ActiveBlock { kind, index });
        }
        let index = self
            .active
            .as_ref()
            .expect("active block was created")
            .index;
        events.push(match kind {
            BlockKind::Text => StreamEvent::TextDelta {
                content_index: index,
                delta: delta.to_string(),
            },
            BlockKind::Thinking => StreamEvent::ThinkingDelta {
                content_index: index,
                delta: delta.to_string(),
            },
        });
    }

    fn close_active(&mut self, events: &mut Vec<StreamEvent>) {
        if let Some(block) = self.active.take() {
            events.push(match block.kind {
                BlockKind::Text => StreamEvent::TextEnd {
                    content_index: block.index,
                    text_signature: None,
                },
                BlockKind::Thinking => StreamEvent::ThinkingEnd {
                    content_index: block.index,
                    thinking_signature: None,
                },
            });
        }
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        if self.closed {
            return Ok(Vec::new());
        }
        self.closed = true;
        let Some(reason) = self.reason else {
            return Err(ProviderError::Protocol(
                "Mistral stream ended without a finish reason".to_string(),
            ));
        };
        if reason == StopReason::Error {
            return Err(ProviderError::Failure(format!(
                "Provider stopped with: {}",
                self.raw_reason.as_deref().unwrap_or("error")
            )));
        }
        let mut events = Vec::new();
        self.close_active(&mut events);
        for (_, index) in self.tools.drain() {
            events.push(StreamEvent::ToolCallEnd {
                content_index: index,
                thought_signature: None,
            });
        }
        events.push(StreamEvent::Done {
            reason,
            usage: self.usage.clone(),
        });
        Ok(events)
    }
}

fn parse_usage(value: &Value) -> Usage {
    let prompt = value
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = [
        "/prompt_tokens_details/cached_tokens",
        "/promptTokenDetails/cachedTokens",
        "/prompt_token_details/cached_tokens",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_u64))
    .or_else(|| value.get("num_cached_tokens").and_then(Value::as_u64))
    .or_else(|| value.get("numCachedTokens").and_then(Value::as_u64))
    .unwrap_or(0)
    .min(prompt);
    let output = value
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Usage {
        input: prompt.saturating_sub(cache_read),
        output,
        cache_read,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: value
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(prompt.saturating_add(output)),
        cost: pi_core::UsageCost::default(),
    }
}

#[derive(Default)]
struct ToolCallIdNormalizer {
    ids: HashMap<String, String>,
    owners: HashMap<String, String>,
}

impl ToolCallIdNormalizer {
    fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.ids.get(id) {
            return existing.clone();
        }
        let mut attempt = 0;
        loop {
            let candidate = derive_tool_call_id(id, attempt);
            if self.owners.get(&candidate).is_none_or(|owner| owner == id) {
                self.ids.insert(id.to_string(), candidate.clone());
                self.owners.insert(candidate.clone(), id.to_string());
                return candidate;
            }
            attempt += 1;
        }
    }
}

fn derive_tool_call_id(id: &str, attempt: usize) -> String {
    const LENGTH: usize = 9;
    let normalized = id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    if attempt == 0 && normalized.len() == LENGTH {
        return normalized;
    }
    let base = if normalized.is_empty() {
        id
    } else {
        &normalized
    };
    let seed = if attempt == 0 {
        base.to_string()
    } else {
        format!("{base}:{attempt}")
    };
    short_hash(&seed).chars().take(LENGTH).collect()
}

fn short_hash(value: &str) -> String {
    let mut h1 = 0xdead_beef_u32;
    let mut h2 = 0x41c6_ce57_u32;
    for character in value.encode_utf16() {
        h1 = (h1 ^ u32::from(character)).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ u32::from(character)).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", base36(h2), base36(h1))
}

fn base36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let mut output = Vec::new();
    while value > 0 {
        let digit = value % 36;
        output.push(if digit < 10 {
            char::from(b'0' + digit as u8)
        } else {
            char::from(b'a' + (digit - 10) as u8)
        });
        value /= 36;
    }
    output.into_iter().rev().collect()
}

fn contains_header(headers: &BTreeMap<String, String>, target: &str) -> bool {
    headers.keys().any(|name| name.eq_ignore_ascii_case(target))
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

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        error => ProviderError::Failure(error.to_string()),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, ModelId, ModelSpec, ProviderCallContext, ToolSpec};
    use pi_provider::{HttpResponse, TransportError};

    use super::*;

    #[derive(Default)]
    struct Capture {
        url: String,
        headers: BTreeMap<String, String>,
        body: Value,
    }

    struct CapturingTransport {
        capture: Arc<Mutex<Capture>>,
        response: String,
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn post_json(
            &self,
            url: &str,
            headers: &BTreeMap<String, String>,
            body: &Value,
            _signal: AbortSignal,
        ) -> Result<HttpResponse, TransportError> {
            *self.capture.lock().unwrap() = Capture {
                url: url.to_string(),
                headers: headers.clone(),
                body: body.clone(),
            };
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/event-stream".to_string()),
                headers: Vec::new(),
                body: Box::pin(stream::iter([Ok(self.response.as_bytes().to_vec())])),
            })
        }
    }

    fn request(model: &str) -> ProviderRequest {
        let mut spec = ModelSpec::new(MISTRAL_PROVIDER_ID, model, model, MISTRAL_CONVERSATIONS_API);
        spec.base_url = Some(MISTRAL_BASE_URL.to_string());
        spec.reasoning = true;
        ProviderRequest {
            model: ModelId::new(model),
            model_spec: Some(spec),
            system_prompt: "system".to_string(),
            messages: Vec::new(),
            tools: vec![ToolSpec {
                name: "read".to_string(),
                label: "Read".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
                execution_mode: pi_core::ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            }],
            thinking_level: ThinkingLevel::High,
            thinking_budgets: None,
            max_output_tokens: Some(1234),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: Some("session-1".to_string()),
        }
    }

    #[test]
    fn tool_call_ids_are_pi_compatible_and_stable() {
        assert_eq!(derive_tool_call_id("abcdefghi", 0), "abcdefghi");
        assert_eq!(derive_tool_call_id("call-with-punctuation", 0).len(), 9);
        assert_eq!(
            derive_tool_call_id("call-with-punctuation", 0),
            derive_tool_call_id("call-with-punctuation", 0)
        );
    }

    #[test]
    fn reasoning_payload_uses_model_specific_mistral_controls() {
        let prompt_mode = request_body(&request("magistral-medium-latest"));
        assert_eq!(prompt_mode["prompt_mode"], "reasoning");
        assert_eq!(prompt_mode["prompt_cache_key"], "session-1");
        assert_eq!(prompt_mode["max_tokens"], 1234);
        assert_eq!(prompt_mode["tools"][0]["function"]["strict"], false);

        let effort = request_body(&request("mistral-small-latest"));
        assert_eq!(effort["reasoning_effort"], "high");
        assert!(effort.get("prompt_mode").is_none());
    }

    #[tokio::test]
    async fn streams_thinking_text_tools_and_cached_usage() {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let response = concat!(
            "data: {\"id\":\"resp-1\",\"choices\":[{\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"text\":\"plan\"}]}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"abcdefghi\",\"function\":{\"name\":\"read\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"\",\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":3},\"total_tokens\":12}}\n\n",
            "data: [DONE]\n\n"
        );
        let provider = MistralCompatibleProvider::with_transport(
            MistralCompatibleConfig::new(MISTRAL_BASE_URL, "secret"),
            Arc::new(CapturingTransport {
                capture: Arc::clone(&capture),
                response: response.to_string(),
            }),
        )
        .unwrap();
        let request = request("mistral-small-latest");
        let (abort, signal) = AbortHandle::new();
        drop(abort);
        let events = provider
            .stream(
                request,
                ProviderCallContext::without_plugins(
                    ".",
                    ProviderId::new(MISTRAL_PROVIDER_ID),
                    ModelId::new("mistral-small-latest"),
                ),
                signal,
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::ThinkingDelta { delta, .. }) if delta == "plan")
        ));
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::TextDelta { delta, .. }) if delta == "done")
        ));
        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::ToolCallStart { name, .. }) if name == "read")
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Ok(StreamEvent::ToolCallStart { .. })))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(event, Ok(StreamEvent::Done { reason: StopReason::ToolUse, usage }) if usage.input == 7 && usage.cache_read == 3)));

        let capture = capture.lock().unwrap();
        assert_eq!(capture.url, "https://api.mistral.ai/v1/chat/completions");
        assert_eq!(capture.headers["Authorization"], "Bearer secret");
        assert_eq!(capture.headers["x-affinity"], "session-1");
        assert_eq!(capture.body["reasoning_effort"], "high");
    }
}
