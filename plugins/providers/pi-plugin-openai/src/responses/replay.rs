//! Responses-specific, non-mutating replay. Never executes or changes saved tools.
use pi_core::{ContentBlock, Message, ModelSpec, StopReason, TextContent, ToolResultMessage};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(super) fn balanced(messages: &[Message], model: Option<&ModelSpec>) -> Vec<Message> {
    let mut output = Vec::new();
    let mut pending = Vec::new();
    let mut answered = HashSet::new();
    let mut held = Vec::new();
    let mut ids = HashMap::new();
    for message in messages {
        if matches!(message, Message::System(_)) && !pending.is_empty() {
            held.push(message.clone());
            continue;
        }
        if !matches!(message, Message::System(_) | Message::ToolResult(_)) {
            close_pending(&mut output, &mut pending, &mut answered);
            output.append(&mut held);
        }
        let mut message = message.clone();
        match &mut message {
            Message::Assistant(assistant) => {
                if matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    continue;
                }
                let same = model.is_some_and(|model| {
                    assistant.provider == model.provider
                        && assistant.api == model.api
                        && assistant.model == model.id
                });
                for call in assistant.tool_calls() {
                    let id = normalize_id(&call.id.to_string(), model, assistant, same);
                    ids.insert(call.id.to_string(), id.clone());
                    pending.push((id, call.name, assistant.timestamp_ms));
                }
                if !same {
                    let assistant = std::sync::Arc::make_mut(assistant);
                    assistant
                        .content
                        .retain(|block| !matches!(block, ContentBlock::Thinking(_)));
                    for block in &mut assistant.content {
                        match block {
                            ContentBlock::Text(text) => text.text_signature = None,
                            ContentBlock::ToolCall(call) => {
                                call.thought_signature = None;
                                call.namespace = None;
                            }
                            _ => {}
                        }
                    }
                }
                if assistant.tool_calls().iter().any(|call| {
                    ids.get(call.id.as_str()).is_some_and(|id| id != call.id.as_str())
                }) {
                    for block in &mut std::sync::Arc::make_mut(assistant).content {
                        if let ContentBlock::ToolCall(call) = block
                            && let Some(id) = ids.get(call.id.as_str())
                        {
                            call.id = id.clone().into();
                        }
                    }
                }
            }
            Message::ToolResult(result) => {
                if let Some(id) = ids.get(result.tool_call_id.as_str())
                    && id != result.tool_call_id.as_str()
                {
                    std::sync::Arc::make_mut(result).tool_call_id = id.clone().into();
                }
                answered.insert(result.tool_call_id.to_string());
            }
            _ => {}
        }
        output.push(message);
    }
    close_pending(&mut output, &mut pending, &mut answered);
    output.append(&mut held);
    output
}
fn close_pending(
    output: &mut Vec<Message>,
    pending: &mut Vec<(String, String, i64)>,
    answered: &mut HashSet<String>,
) {
    for (id, name, timestamp) in pending.drain(..) {
        if !answered.contains(&id) {
            output.push(Message::tool_result(ToolResultMessage {
                tool_call_id: id.into(),
                tool_name: name,
                content: vec![ContentBlock::Text(TextContent::new("No result provided"))],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: true,
                timestamp_ms: timestamp,
            }));
        }
    }
    answered.clear();
}
fn normalize_part(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>()
        .trim_end_matches('_')
        .into()
}
fn normalize_id(
    id: &str,
    model: Option<&ModelSpec>,
    source: &pi_core::AssistantMessage,
    same: bool,
) -> String {
    if same {
        return id.into();
    }
    let Some(model) = model else {
        return id.into();
    };
    if !matches!(
        model.provider.as_str(),
        "openai" | "openai-codex" | "opencode" | "opencode-go" | "github-copilot"
    ) || !id.contains('|')
    {
        return normalize_part(id);
    }
    let (call, item) = id.split_once('|').unwrap();
    let item = if source.provider != model.provider || source.api != model.api {
        format!("fc_{}", short_hash(item))
    } else {
        normalize_part(item)
    };
    let item = if item.starts_with("fc_") {
        item
    } else {
        normalize_part(&format!("fc_{item}"))
    };
    format!("{}|{item}", normalize_part(call))
}
fn short_hash(value: &str) -> String {
    let (mut a, mut b) = (0xdeadbeefu32, 0x41c6ce57u32);
    for c in value.encode_utf16() {
        a = (a ^ u32::from(c)).wrapping_mul(2654435761);
        b = (b ^ u32::from(c)).wrapping_mul(1597334677);
    }
    a = (a ^ (a >> 16)).wrapping_mul(2246822507) ^ (b ^ (b >> 13)).wrapping_mul(3266489909);
    b = (b ^ (b >> 16)).wrapping_mul(2246822507) ^ (a ^ (a >> 13)).wrapping_mul(3266489909);
    fn base36(mut n: u32) -> String {
        let mut v = Vec::new();
        loop {
            v.push(char::from_digit(n % 36, 36).unwrap());
            n /= 36;
            if n == 0 {
                break;
            }
        }
        v.into_iter().rev().collect()
    }
    format!("{}{}", base36(b), base36(a))
}

pub(super) fn project(message: &Message, model: Option<&ModelSpec>, position: usize) -> Vec<Value> {
    let vision = model.is_none_or(|model| model.input.contains(&pi_core::ModelInput::Image));
    match message {
        Message::System(system) => vec![json!({"role":"system","content":system.render()})],
        Message::User(user) => vec![
            json!({"type":"message","role":"user","content":input_content(&user.content,vision,false)}),
        ],
        Message::Custom(custom) => vec![
            json!({"type":"message","role":"user","content":input_content(&custom.content.to_blocks(),vision,false)}),
        ],
        Message::Assistant(assistant) => {
            let same = model.is_some_and(|model| {
                assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id
            });
            let different_model = model.is_some_and(|model| {
                assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model != model.id
            });
            let mut text_index = 0;
            assistant.content.iter().filter_map(|block| match block {
                ContentBlock::Thinking(thinking) if same=>thinking.thinking_signature.as_ref()
                    .and_then(|value|serde_json::from_str::<Value>(value).ok()).filter(|value|value["type"]=="reasoning"),
                ContentBlock::Text(text)=>{
                    let signature=same.then_some(text.text_signature.as_deref()).flatten();
                    let parsed=signature.and_then(|value|serde_json::from_str::<Value>(value).ok()).filter(|value|value["v"]==1 && value["id"].is_string());
                    let id=parsed.as_ref().and_then(|value|value["id"].as_str()).or(signature).map(str::to_owned).unwrap_or_else(||if text_index==0{format!("msg_pi_{position}")}else{format!("msg_pi_{position}_{text_index}")});
                    text_index+=1;
                    let id=if id.encode_utf16().count()>64{format!("msg_{}",short_hash(&id))}else{id};
                    let mut value=json!({"type":"message","role":"assistant","id":id,"status":"completed","content":[{"type":"output_text","text":text.text,"annotations":[]}]});
                    if let Some(phase)=parsed.as_ref().and_then(|value|value["phase"].as_str()).filter(|phase|matches!(*phase,"commentary"|"final_answer")){value["phase"]=json!(phase);}
                    Some(value)
                }
                ContentBlock::ToolCall(call)=>{
                    let mut parts=call.id.as_str().split('|');let id=parts.next().unwrap_or_default();
                    let mut value=json!({"type":"function_call","call_id":id,"name":call.name,"arguments":call.arguments.to_string()});
                    if let Some(item)=parts.next().filter(|id|id.starts_with("fc_") && !different_model){value["id"]=json!(item);}
                    if same && let Some(namespace)=&call.namespace{value["namespace"]=json!(namespace);}
                    Some(value)
                }
                _=>None,
            }).collect()
        }
        Message::ToolResult(result) => {
            let blocks = input_content(&result.content, vision, true);
            let has_images = vision
                && result
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Image(_)));
            let output = if has_images {
                json!(blocks)
            } else {
                let text = blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                json!(if text.trim().is_empty() {
                    "(no tool output)"
                } else {
                    &text
                })
            };
            vec![
                json!({"type":"function_call_output","call_id":result.tool_call_id.as_str().split('|').next().unwrap_or_default(),"output":output}),
            ]
        }
    }
}
fn input_content(blocks: &[ContentBlock], vision: bool, tool: bool) -> Vec<Value> {
    let mut result = Vec::new();
    let mut previous_image = false;
    for block in blocks {
        match block {
            ContentBlock::Text(text)=>{result.push(json!({"type":"input_text","text":text.text}));previous_image=false;}
            ContentBlock::Image(image) if vision=>result.push(json!({"type":"input_image","detail":"auto","image_url":format!("data:{};base64,{}",image.mime_type,image.data)})),
            ContentBlock::Image(_) if !previous_image=>{result.push(json!({"type":"input_text","text":if tool{"(tool image omitted: model does not support images)"}else{"(image omitted: model does not support images)"}}));previous_image=true;}
            _=>{}
        }
    }
    result
}
