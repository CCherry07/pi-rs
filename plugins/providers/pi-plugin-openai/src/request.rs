use pi_core::{ContentBlock, Message, ProviderRequest};
use serde_json::{Value, json};

pub(crate) fn request_body(request: &ProviderRequest) -> Value {
    let mut messages = Vec::new();
    let mut pending_tool_images = Vec::new();
    if !request.system_prompt.is_empty() {
        messages.push(json!({"role":"system", "content":request.system_prompt}));
    }
    for message in &request.messages {
        if !matches!(message, Message::ToolResult(_)) {
            flush_tool_images(&mut messages, &mut pending_tool_images);
        }
        match message {
            Message::User(message) => messages.push(json!({
                "role":"user", "content":user_content(&message.content)
            })),
            Message::Assistant(message) => {
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
                let mut value =
                    json!({"role":"assistant", "content":blocks_text(&message.content)});
                if !calls.is_empty() {
                    value["tool_calls"] = Value::Array(calls);
                }
                messages.push(value);
            }
            Message::ToolResult(message) => {
                messages.push(json!({
                    "role":"tool", "tool_call_id":message.tool_call_id.as_str(),
                    "content":blocks_text(&message.content)
                }));
                pending_tool_images.extend(message.content.iter().filter_map(image_part));
            }
        }
    }
    flush_tool_images(&mut messages, &mut pending_tool_images);
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type":"function", "function":{
                    "name":tool.name, "description":tool.description, "parameters":tool.parameters
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model":request.model.as_str(), "messages":messages, "stream":true,
        "stream_options":{"include_usage":true}
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String("auto".to_string());
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        body["max_completion_tokens"] = Value::from(max_output_tokens);
    }
    if let Value::Object(body) = &mut body {
        body.extend(request.sampling_params.clone());
    }
    body
}

fn flush_tool_images(messages: &mut Vec<Value>, images: &mut Vec<Value>) {
    if images.is_empty() {
        return;
    }
    let mut parts = vec![json!({
        "type":"text", "text":"Attached image(s) from tool result:"
    })];
    parts.append(images);
    messages.push(json!({"role":"user", "content":parts}));
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

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ImageContent, ModelId, TextContent, ToolCallId, ToolResultMessage, Usage};
    use std::sync::Arc;

    #[test]
    fn converts_user_image_to_data_url() {
        let request = ProviderRequest {
            model: ModelId::new("gpt"),
            system_prompt: String::new(),
            tools: vec![],
            thinking_level: pi_core::ThinkingLevel::Off,
            max_output_tokens: Some(123),
            headers: Default::default(),
            sampling_params: std::collections::BTreeMap::from([(
                "temperature".to_string(),
                json!(0.4),
            )]),
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
            system_prompt: String::new(),
            tools: vec![],
            thinking_level: pi_core::ThinkingLevel::Off,
            max_output_tokens: None,
            headers: Default::default(),
            sampling_params: Default::default(),
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
}
