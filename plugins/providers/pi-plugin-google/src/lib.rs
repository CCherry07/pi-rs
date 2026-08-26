#![forbid(unsafe_code)]

//! Google AI Studio provider/catalog and reusable Generative AI wire adapter.

mod catalog;

pub use catalog::google_models;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{
    AbortSignal, ContentBlock, Message, PluginId, Provider, ProviderAvailability,
    ProviderCallContext, ProviderError, ProviderId, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream, ResponseMetadata, ResponseMetadataPatch, StopReason,
    StreamEvent, ThinkingLevel, ToolCallId, Usage,
};
use pi_provider::{
    HttpBodyStream, HttpTransport, ReqwestTransport, SseDecoder, TransportError,
    collect_body_limited,
};
use serde_json::{Value, json};

pub const GOOGLE_GENERATIVE_AI_API: &str = "google-generative-ai";
pub(crate) const GOOGLE_PROVIDER_ID: &str = "google";
pub(crate) const GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Built-in Google AI Studio provider and current Pi Gemini catalog.
pub struct GooglePlugin {
    provider: Arc<GoogleCompatibleProvider>,
}

impl GooglePlugin {
    pub fn discover() -> Result<Self, ProviderError> {
        Self::from_stored(None)
    }

    pub fn from_stored(api_key: Option<String>) -> Result<Self, ProviderError> {
        Self::new(env("GEMINI_API_KEY").or(api_key))
    }

    pub fn new(api_key: Option<String>) -> Result<Self, ProviderError> {
        let config = api_key
            .map_or_else(
                || GoogleCompatibleConfig::without_api_key(GOOGLE_BASE_URL),
                |api_key| GoogleCompatibleConfig::new(GOOGLE_BASE_URL, api_key),
            )
            .provider_id(GOOGLE_PROVIDER_ID);
        Ok(Self {
            provider: Arc::new(GoogleCompatibleProvider::new(config)?),
        })
    }
}

impl ProviderPlugin for GooglePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("google-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())?;
        for model in google_models() {
            context.register_model(model)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GoogleCompatibleConfig {
    pub provider_id: ProviderId,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl GoogleCompatibleConfig {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("google-compatible"),
            base_url: base_url.into(),
            api_key: Some(api_key.into()),
            headers: BTreeMap::new(),
        }
    }

    pub fn without_api_key(base_url: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::new("google-compatible"),
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

pub struct GoogleCompatibleProvider {
    config: GoogleCompatibleConfig,
    transport: Arc<dyn HttpTransport>,
}

impl GoogleCompatibleProvider {
    pub fn new(config: GoogleCompatibleConfig) -> Result<Self, ProviderError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        config: GoogleCompatibleConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, ProviderError> {
        if config.base_url.trim().is_empty() {
            return Err(ProviderError::Failure(
                "Google base URL cannot be empty".to_string(),
            ));
        }
        if config
            .api_key
            .as_ref()
            .is_some_and(|key| key.contains(['\r', '\n']))
        {
            return Err(ProviderError::Failure("invalid Google API key".to_string()));
        }
        Ok(Self { config, transport })
    }

    fn endpoint(&self, model: &str) -> String {
        google_endpoint(&self.config.base_url, model)
    }

    fn headers(&self, request: &ProviderRequest) -> BTreeMap<String, String> {
        let mut headers = self.config.headers.clone();
        insert_header(&mut headers, "Accept", "text/event-stream");
        insert_header(&mut headers, "Content-Type", "application/json");
        if let Some(api_key) = &self.config.api_key {
            insert_header(&mut headers, "x-goog-api-key", api_key);
        }
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        headers
    }
}

#[async_trait]
impl Provider for GoogleCompatibleProvider {
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
            .map(|base_url| google_endpoint(base_url, request.model.as_str()))
            .unwrap_or_else(|| self.endpoint(request.model.as_str()));
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
        Ok(google_stream(
            self.config.provider_id.clone(),
            request.model,
            response.body,
            signal,
        ))
    }
}

pub fn request_body(request: &ProviderRequest) -> Value {
    let mut body = json!({"contents": project_messages(request)});
    if !request.system_prompt.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": request.system_prompt}]});
    }
    if !request.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": request.tools.iter().map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "parametersJsonSchema": tool.parameters
            })).collect::<Vec<_>>()
        }]);
    }
    let mut generation = serde_json::Map::new();
    if let Some(max_tokens) = request.max_output_tokens {
        generation.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if request
        .model_spec
        .as_ref()
        .is_some_and(|model| model.reasoning)
    {
        generation.insert(
            "thinkingConfig".to_string(),
            google_thinking_config(request),
        );
    }
    if !generation.is_empty() {
        body["generationConfig"] = Value::Object(generation);
    }
    body
}

fn project_messages(request: &ProviderRequest) -> Vec<Value> {
    let mut messages = Vec::new();
    let model_id = request.model.as_str();
    let include_tool_call_id = requires_tool_call_id(model_id);
    let supports_multimodal_tool_results = supports_multimodal_function_response(model_id);
    let accepts_images = request
        .model_spec
        .as_ref()
        .is_some_and(|model| model.input.contains(&pi_core::ModelInput::Image));
    for message in &request.messages {
        match message {
            Message::User(message) => {
                let parts = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(json!({"text": text.text})),
                        ContentBlock::Image(image) => Some(json!({
                            "inlineData": {"mimeType": image.mime_type, "data": image.data}
                        })),
                        ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => None,
                    })
                    .collect::<Vec<_>>();
                if !parts.is_empty() {
                    messages.push(json!({"role": "user", "parts": parts}));
                }
            }
            Message::Custom(message) => {
                let parts = message
                    .content
                    .to_blocks()
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(json!({"text": text.text})),
                        ContentBlock::Image(image) => Some(json!({
                            "inlineData": {"mimeType": image.mime_type, "data": image.data}
                        })),
                        ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) => None,
                    })
                    .collect::<Vec<_>>();
                if !parts.is_empty() {
                    messages.push(json!({"role": "user", "parts": parts}));
                }
            }
            Message::Assistant(message) => {
                let same_model = request.model_spec.as_ref().is_some_and(|model| {
                    message.provider == model.provider && message.model == request.model
                });
                let parts = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => {
                            let signature =
                                usable_google_signature(same_model, text.text_signature.as_deref());
                            if text.text.trim().is_empty() && signature.is_none() {
                                return None;
                            }
                            let mut part = json!({"text": text.text});
                            if let Some(signature) = signature {
                                part["thoughtSignature"] = Value::String(signature.to_string());
                            }
                            Some(part)
                        }
                        ContentBlock::Thinking(thinking) => {
                            let signature = usable_google_signature(
                                same_model,
                                thinking.thinking_signature.as_deref(),
                            );
                            if thinking.thinking.trim().is_empty() && signature.is_none() {
                                return None;
                            }
                            let mut part = if same_model {
                                json!({"text": thinking.thinking, "thought": true})
                            } else {
                                json!({"text": thinking.thinking})
                            };
                            if let Some(signature) = signature {
                                part["thoughtSignature"] = Value::String(signature.to_string());
                            }
                            Some(part)
                        }
                        ContentBlock::ToolCall(call) => {
                            let mut function_call = json!({
                                "name": call.name,
                                "args": call.arguments
                            });
                            if include_tool_call_id {
                                function_call["id"] =
                                    Value::String(normalized_tool_id(call.id.as_str()));
                            }
                            let mut part = json!({"functionCall": function_call});
                            if let Some(signature) = usable_google_signature(
                                same_model,
                                call.thought_signature.as_deref(),
                            ) {
                                part["thoughtSignature"] = Value::String(signature.to_string());
                            }
                            Some(part)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !parts.is_empty() {
                    messages.push(json!({"role": "model", "parts": parts}));
                }
            }
            Message::ToolResult(message) => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        ContentBlock::Thinking(thinking) => Some(thinking.thinking.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let images = if accepts_images {
                    message
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Image(image) => Some(json!({
                                "inlineData": {
                                    "mimeType": image.mime_type,
                                    "data": image.data
                                }
                            })),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let response_text = if !text.is_empty() {
                    text
                } else if images.is_empty() {
                    String::new()
                } else {
                    "(see attached image)".to_string()
                };
                let response = if message.is_error {
                    json!({"error": response_text})
                } else {
                    json!({"output": response_text})
                };
                let mut function_response = json!({
                    "name": message.tool_name,
                    "response": response
                });
                if include_tool_call_id {
                    function_response["id"] =
                        Value::String(normalized_tool_id(message.tool_call_id.as_str()));
                }
                if !images.is_empty() && supports_multimodal_tool_results {
                    function_response["parts"] = Value::Array(images.clone());
                }
                let part = json!({"functionResponse": function_response});
                if let Some(last) = messages.last_mut()
                    && last.get("role").and_then(Value::as_str) == Some("user")
                    && last.pointer("/parts/0/functionResponse").is_some()
                {
                    last["parts"]
                        .as_array_mut()
                        .expect("parts is an array")
                        .push(part);
                } else {
                    messages.push(json!({"role": "user", "parts": [part]}));
                }
                if !images.is_empty() && !supports_multimodal_tool_results {
                    let mut parts = vec![json!({"text": "Tool result image:"})];
                    parts.extend(images);
                    messages.push(json!({"role": "user", "parts": parts}));
                }
            }
        }
    }
    messages
}

fn google_thinking_config(request: &ProviderRequest) -> Value {
    let id = request.model.as_str().to_ascii_lowercase();
    let enabled = request.thinking_level != ThinkingLevel::Off;
    let is_gemini_3_pro = is_gemini_3_pro(&id);
    let is_gemini_3_flash = is_gemini_3_flash(&id);
    let is_gemma_4 = is_gemma_4(&id);
    let uses_levels =
        is_gemini_3_pro || is_gemini_3_flash || id.contains("gemini-flash-latest") || is_gemma_4;
    if uses_levels {
        let level = if enabled {
            let mapped = mapped_google_level(request);
            if is_gemini_3_pro {
                match mapped {
                    "minimal" | "low" => "LOW",
                    _ => "HIGH",
                }
            } else if is_gemma_4 {
                match mapped {
                    "minimal" | "low" => "MINIMAL",
                    _ => "HIGH",
                }
            } else {
                match mapped {
                    "minimal" => "MINIMAL",
                    "low" => "LOW",
                    "medium" => "MEDIUM",
                    _ => "HIGH",
                }
            }
        } else if is_gemini_3_pro {
            "LOW"
        } else {
            "MINIMAL"
        };
        json!({"includeThoughts": enabled, "thinkingLevel": level})
    } else {
        let budget = if enabled {
            google_thinking_budget(&id, request.thinking_level)
        } else {
            0
        };
        json!({"includeThoughts": enabled, "thinkingBudget": budget})
    }
}

fn google_stream(
    provider: ProviderId,
    model: pi_core::ModelId,
    mut body: HttpBodyStream,
    signal: AbortSignal,
) -> ProviderStream {
    Box::pin(stream! {
        yield Ok(StreamEvent::Start {
            metadata: ResponseMetadata::new(
                provider,
                model.clone(),
                GOOGLE_GENERATIVE_AI_API,
                now_ms(),
            ),
        });
        let mut decoder = SseDecoder::new();
        let mut state = GoogleStreamState::new(&model);
        loop {
            let next = tokio::select! {
                _ = signal.wait() => { yield Err(ProviderError::Aborted); return; }
                next = body.next() => next,
            };
            match next {
                Some(Ok(bytes)) => {
                    let decoded = match decoder.push(&bytes) {
                        Ok(events) => events,
                        Err(error) => { yield Err(map_transport_error(error)); return; }
                    };
                    for event in decoded {
                        match state.consume(&event.data) {
                            Ok(events) => {
                                let done = events.iter().any(|event| matches!(event, StreamEvent::Done { .. }));
                                for event in events { yield Ok(event); }
                                if done { return; }
                            }
                            Err(error) => { yield Err(error); return; }
                        }
                    }
                }
                Some(Err(error)) => { yield Err(map_transport_error(error)); return; }
                None => {
                    match decoder.finish() {
                        Ok(Some(event)) => match state.consume(&event.data) {
                            Ok(events) => for event in events { yield Ok(event); },
                            Err(error) => { yield Err(error); return; }
                        },
                        Ok(None) => {}
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

struct GoogleStreamState {
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    next_index: usize,
    next_tool: usize,
    usage: Usage,
    reason: Option<StopReason>,
    closed: bool,
    saw_finish_reason: bool,
    signatures: HashMap<usize, String>,
    requested_model: String,
}

impl GoogleStreamState {
    fn new(model: &pi_core::ModelId) -> Self {
        Self {
            text_index: None,
            thinking_index: None,
            next_index: 0,
            next_tool: 0,
            usage: Usage::default(),
            reason: None,
            closed: false,
            saw_finish_reason: false,
            signatures: HashMap::new(),
            requested_model: model.as_str().to_string(),
        }
    }

    fn consume(&mut self, data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|error| {
            ProviderError::Protocol(format!("invalid Google SSE JSON: {error}"))
        })?;
        if let Some(message) = value.pointer("/error/message").and_then(Value::as_str) {
            return Err(ProviderError::Failure(message.to_string()));
        }
        self.update_usage(value.get("usageMetadata"));
        let mut events = Vec::new();
        let response_patch = ResponseMetadataPatch {
            response_model: value
                .get("modelVersion")
                .and_then(Value::as_str)
                .filter(|model| *model != self.requested_model.as_str())
                .map(str::to_string),
            response_id: value
                .get("responseId")
                .and_then(Value::as_str)
                .map(str::to_string),
            ..ResponseMetadataPatch::default()
        };
        if response_patch.response_model.is_some() || response_patch.response_id.is_some() {
            events.push(StreamEvent::Metadata {
                patch: response_patch,
            });
        }
        if let Some(parts) = value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let thinking = part.get("thought").and_then(Value::as_bool) == Some(true);
                    let index = if thinking {
                        if let Some(index) = self.text_index.take() {
                            events.push(StreamEvent::TextEnd {
                                content_index: index,
                                text_signature: self.signatures.remove(&index),
                            });
                        }
                        *self.thinking_index.get_or_insert_with(|| {
                            let index = self.next_index;
                            self.next_index += 1;
                            events.push(StreamEvent::ThinkingStart {
                                content_index: index,
                            });
                            index
                        })
                    } else {
                        if let Some(index) = self.thinking_index.take() {
                            events.push(StreamEvent::ThinkingEnd {
                                content_index: index,
                                thinking_signature: self.signatures.remove(&index),
                            });
                        }
                        *self.text_index.get_or_insert_with(|| {
                            let index = self.next_index;
                            self.next_index += 1;
                            events.push(StreamEvent::TextStart {
                                content_index: index,
                            });
                            index
                        })
                    };
                    if let Some(signature) = part.get("thoughtSignature").and_then(Value::as_str) {
                        self.signatures.insert(index, signature.to_string());
                    }
                    if !text.is_empty() {
                        events.push(if thinking {
                            StreamEvent::ThinkingDelta {
                                content_index: index,
                                delta: text.to_string(),
                            }
                        } else {
                            StreamEvent::TextDelta {
                                content_index: index,
                                delta: text.to_string(),
                            }
                        });
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    self.close_text(&mut events);
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            self.next_tool += 1;
                            format!("google-tool-{}", self.next_tool)
                        });
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let index = self.next_index;
                    self.next_index += 1;
                    events.push(StreamEvent::ToolCallStart {
                        content_index: index,
                        id: ToolCallId::new(id),
                        name,
                    });
                    events.push(StreamEvent::ToolCallDelta {
                        content_index: index,
                        arguments_delta: call
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| json!({}))
                            .to_string(),
                    });
                    events.push(StreamEvent::ToolCallEnd {
                        content_index: index,
                        thought_signature: part
                            .get("thoughtSignature")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                    self.reason = Some(StopReason::ToolUse);
                }
            }
        }
        if let Some(reason) = value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
        {
            self.saw_finish_reason = true;
            self.reason = Some(match reason {
                "MAX_TOKENS" => StopReason::Length,
                "STOP" if self.reason == Some(StopReason::ToolUse) => StopReason::ToolUse,
                "STOP" => StopReason::Stop,
                _ => StopReason::Error,
            });
            self.close_text(&mut events);
            self.closed = true;
            events.push(StreamEvent::Metadata {
                patch: ResponseMetadataPatch {
                    raw_stop_reason: Some(reason.to_string()),
                    ..ResponseMetadataPatch::default()
                },
            });
            events.push(StreamEvent::Done {
                reason: self.reason.unwrap_or(StopReason::Stop),
                usage: self.usage.clone(),
            });
        }
        Ok(events)
    }

    fn close_text(&mut self, events: &mut Vec<StreamEvent>) {
        if let Some(index) = self.thinking_index.take() {
            events.push(StreamEvent::ThinkingEnd {
                content_index: index,
                thinking_signature: self.signatures.remove(&index),
            });
        }
        if let Some(index) = self.text_index.take() {
            events.push(StreamEvent::TextEnd {
                content_index: index,
                text_signature: self.signatures.remove(&index),
            });
        }
    }

    fn update_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        let prompt = usage
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_read = usage
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let candidates = usage
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning = usage
            .get("thoughtsTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.usage.input = prompt.saturating_sub(cache_read);
        self.usage.output = candidates.saturating_add(reasoning);
        self.usage.cache_read = cache_read;
        self.usage.reasoning = Some(reasoning);
        self.usage.total_tokens = usage
            .get("totalTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(prompt.saturating_add(candidates).saturating_add(reasoning));
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        if self.closed {
            return Ok(Vec::new());
        }
        if !self.saw_finish_reason {
            return Err(ProviderError::Protocol(
                "Google stream ended without a finish reason".to_string(),
            ));
        }
        let mut events = Vec::new();
        self.close_text(&mut events);
        self.closed = true;
        events.push(StreamEvent::Done {
            reason: self.reason.unwrap_or(StopReason::Stop),
            usage: self.usage.clone(),
        });
        Ok(events)
    }
}

fn is_gemini_3_pro(id: &str) -> bool {
    is_gemini_3_kind(id, "pro")
}

fn is_gemini_3_flash(id: &str) -> bool {
    is_gemini_3_kind(id, "flash")
        || matches!(id, "gemini-flash-latest" | "gemini-flash-lite-latest")
}

fn is_gemini_3_kind(id: &str, kind: &str) -> bool {
    let Some(rest) = id.strip_prefix("gemini-3") else {
        return false;
    };
    let rest = if let Some(version) = rest.strip_prefix('.') {
        version.trim_start_matches(|character: char| character.is_ascii_digit())
    } else {
        rest
    };
    rest.starts_with(&format!("-{kind}"))
}

fn is_gemma_4(id: &str) -> bool {
    id.contains("gemma-4") || id.contains("gemma4")
}

fn mapped_google_level(request: &ProviderRequest) -> &str {
    if matches!(
        request.thinking_level,
        ThinkingLevel::XHigh | ThinkingLevel::Max
    ) {
        return "high";
    }
    request
        .model_spec
        .as_ref()
        .and_then(|model| {
            model
                .thinking_level_map
                .get(request.thinking_level.as_str())
        })
        .and_then(|level| level.as_deref())
        .unwrap_or_else(|| request.thinking_level.as_str())
}

fn google_thinking_budget(id: &str, level: ThinkingLevel) -> i64 {
    let level = match level {
        ThinkingLevel::XHigh | ThinkingLevel::Max => ThinkingLevel::High,
        level => level,
    };
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal if id.contains("2.5-flash-lite") => 512,
        ThinkingLevel::Minimal if id.contains("2.5-pro") || id.contains("2.5-flash") => 128,
        ThinkingLevel::Low if id.contains("2.5-") => 2_048,
        ThinkingLevel::Medium if id.contains("2.5-") => 8_192,
        ThinkingLevel::High if id.contains("2.5-pro") => 32_768,
        ThinkingLevel::High if id.contains("2.5-flash") => 24_576,
        _ => -1,
    }
}

fn requires_tool_call_id(id: &str) -> bool {
    id.starts_with("claude-")
        || id.starts_with("gpt-oss-")
        || gemini_major_version(id).is_some_and(|major| major >= 3)
}

fn supports_multimodal_function_response(id: &str) -> bool {
    gemini_major_version(id).is_none_or(|major| major >= 3)
}

fn gemini_major_version(id: &str) -> Option<u64> {
    let lower = id.to_ascii_lowercase();
    lower
        .strip_prefix("gemini-live-")
        .or_else(|| lower.strip_prefix("gemini-"))
        .and_then(|rest| rest.split(['-', '.']).next())
        .and_then(|major| major.parse().ok())
}

fn usable_google_signature(same_model: bool, signature: Option<&str>) -> Option<&str> {
    signature.filter(|signature| {
        same_model
            && !signature.is_empty()
            && signature.len() % 4 == 0
            && signature
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    })
}

fn google_endpoint(base_url: &str, model: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.contains(":streamGenerateContent") {
        if base.contains('?') {
            base.to_string()
        } else {
            format!("{base}?alt=sse")
        }
    } else {
        format!("{base}/models/{model}:streamGenerateContent?alt=sse")
    }
}

fn normalized_tool_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
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
        TransportError::InvalidConfiguration(message) | TransportError::InvalidSse(message) => {
            ProviderError::Protocol(message)
        }
        other => ProviderError::Failure(other.to_string()),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use futures::{StreamExt, stream};
    use pi_core::{AbortHandle, ModelId, ModelSpec, UserMessage};

    use super::*;

    fn request() -> ProviderRequest {
        let mut model = ModelSpec::new(
            "google",
            "gemini-3-pro",
            "Gemini 3 Pro",
            GOOGLE_GENERATIVE_AI_API,
        );
        model.reasoning = true;
        ProviderRequest {
            model: ModelId::new("gemini-3-pro"),
            model_spec: Some(model),
            system_prompt: "system".to_string(),
            messages: vec![Message::User(UserMessage::text("hello", 0))],
            tools: Vec::new(),
            thinking_level: ThinkingLevel::High,
            max_output_tokens: Some(123),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        }
    }

    #[test]
    fn builds_google_ai_studio_payload_and_endpoint() {
        let body = request_body(&request());
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "system");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 123);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "HIGH"
        );
        assert_eq!(
            google_endpoint(
                "https://generativelanguage.googleapis.com/v1beta",
                "gemini-3-pro"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-3-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn builtin_catalog_contains_current_gemini_models() {
        let models = google_models();
        assert_eq!(models.len(), 22);
        let pro = models
            .iter()
            .find(|model| model.id == ModelId::new("gemini-3.1-pro-preview"))
            .unwrap();

        assert_eq!(pro.provider, ProviderId::new("google"));
        assert_eq!(pro.api, GOOGLE_GENERATIVE_AI_API);
        assert_eq!(pro.base_url.as_deref(), Some(GOOGLE_BASE_URL));
        assert_eq!(pro.context_window, 1_048_576);
        assert_eq!(pro.max_tokens, 65_536);
        assert_eq!(pro.cost.input, 2.0);
        assert_eq!(pro.cost.output, 12.0);
        assert_eq!(pro.cost.cache_read, 0.2);
        assert_eq!(pro.thinking_level_map["high"].as_deref(), Some("HIGH"));
        assert!(
            models
                .iter()
                .any(|model| model.id == ModelId::new("gemini-3.6-flash"))
        );
    }

    #[test]
    fn builtin_plugin_availability_tracks_api_key_configuration() {
        let missing = GooglePlugin::new(None).unwrap();
        let configured = GooglePlugin::new(Some("gemini-test-key".to_string())).unwrap();

        assert!(matches!(
            missing.provider.availability(),
            ProviderAvailability::MissingCredentials
        ));
        assert!(matches!(
            configured.provider.availability(),
            ProviderAvailability::Available
        ));
    }

    #[test]
    fn stream_state_preserves_response_identity_and_raw_finish_reason() {
        let mut state = GoogleStreamState::new(&ModelId::new("requested-model"));
        let events = state
            .consume(
                r#"{"responseId":"response-1","modelVersion":"resolved-model","candidates":[{"finishReason":"MAX_TOKENS"}]}"#,
            )
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::Metadata { patch }
                if patch.response_id.as_deref() == Some("response-1")
                    && patch.response_model.as_deref() == Some("resolved-model")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::Metadata { patch }
                if patch.raw_stop_reason.as_deref() == Some("MAX_TOKENS")
        )));
    }

    #[tokio::test]
    async fn adapts_google_sse_text_and_usage() {
        let body: HttpBodyStream = Box::pin(stream::iter([Ok(
            b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"cachedContentTokenCount\":2,\"candidatesTokenCount\":1,\"totalTokenCount\":6}}\n\n"
                .to_vec(),
        )]));
        let (_, signal) = AbortHandle::new();
        let events = google_stream(
            ProviderId::new("google"),
            ModelId::new("gemini"),
            body,
            signal,
        )
        .collect::<Vec<_>>()
        .await;

        assert!(events.iter().any(
            |event| matches!(event, Ok(StreamEvent::TextDelta { delta, .. }) if delta == "hi")
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::Done { usage, .. }) if usage.input == 3 && usage.cache_read == 2
        )));
    }
}
