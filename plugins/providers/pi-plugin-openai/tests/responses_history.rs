use async_trait::async_trait;
use futures::StreamExt;
use pi_core::{AbortHandle, AbortSignal, Message, ModelSpec, ThinkingLevel, UserMessage};
use pi_plugin::{Provider, ProviderCallContext, ProviderRequest};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiResponsesCompatibleProvider};
use pi_provider::{HttpResponse, HttpTransport, TransportError};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

struct Transport {
    payload: Mutex<Option<Value>>,
    events: Vec<Value>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn post_json(
        &self,
        _: &str,
        _: &BTreeMap<String, String>,
        body: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        *self.payload.lock().unwrap() = Some(body.clone());
        let bytes = self
            .events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>()
            .into_bytes();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            content_type: Some("text/event-stream".into()),
            body: Box::pin(futures::stream::iter([Ok(bytes)])),
        })
    }
}
fn request(messages: Vec<Message>, level: ThinkingLevel) -> ProviderRequest {
    let mut model = ModelSpec::new("openai", "model", "Model", "openai-responses");
    model.reasoning = true;
    model.base_url = Some("https://api.openai.com/v1".into());
    model.compat =
        Some(json!({"supportsMidConvoEffort":true, "supportsMidConvoSystemMessages":true}));
    ProviderRequest {
        model: model.id.clone(),
        model_spec: Some(model),
        system_prompt: String::new(),
        messages,
        tools: vec![],
        thinking_level: level,
        thinking_budgets: None,
        max_output_tokens: None,
        headers: BTreeMap::new(),
        sampling_params: BTreeMap::new(),
        session_id: None,
    }
}
async fn run(
    request: ProviderRequest,
    key: &str,
    events: Vec<Value>,
) -> (Value, pi_core::AssistantMessage) {
    let transport = Arc::new(Transport {
        payload: Mutex::new(None),
        events,
    });
    let provider = OpenAiResponsesCompatibleProvider::with_transport(
        OpenAiCompatibleConfig::new("https://api.openai.com/v1", key).provider_id("openai"),
        transport.clone(),
    )
    .unwrap();
    let (_, signal) = AbortHandle::new();
    let context = ProviderCallContext::without_plugins("/", "openai".into(), request.model.clone());
    let mut stream = provider.stream(request, context, signal).await.unwrap();
    let mut assembler = pi_agent::StreamAssembler::new();
    while let Some(event) = stream.next().await {
        assembler.push(event.unwrap()).unwrap();
    }
    let message = assembler.finish().unwrap();
    let payload = transport.payload.lock().unwrap().take().unwrap();
    (payload, message)
}
fn text_events() -> Vec<Value> {
    vec![
        json!({"type":"response.output_text.delta","delta":"answer"}),
        json!({"type":"response.completed","response":{"status":"completed"}}),
    ]
}
fn user(text: &str) -> Message {
    Message::User(UserMessage::text(text, 1))
}
fn update(level: &str) -> Value {
    json!({"type":"configuration_update","reasoning":{"effort":level}})
}

#[tokio::test]
async fn system_inside_tool_batch_waits_for_results_and_failed_replies_are_not_replayed() {
    let call:Message=serde_json::from_value(json!({"role":"assistant","content":[{"type":"toolCall","id":"call|fc_1","name":"lookup","arguments":{}}],"provider":"openai","api":"openai-responses","model":"model","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":2})).unwrap();
    let system = Message::System(pi_core::SystemMessage::text("new instruction", 3));
    let result:Message=serde_json::from_value(json!({"role":"toolResult","toolCallId":"call|fc_1","toolName":"lookup","content":[],"isError":false,"timestamp":4})).unwrap();
    for answered in [true, false] {
        let mut messages = vec![user("one"), call.clone(), system.clone()];
        if answered {
            messages.push(result.clone());
        }
        messages.push(user("two"));
        let (body, _) = run(
            request(messages, ThinkingLevel::Off),
            "sk-fixture",
            text_events(),
        )
        .await;
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(
            input[2]["output"],
            if answered {
                "(no tool output)"
            } else {
                "No result provided"
            }
        );
        assert_eq!(input[3]["role"], "developer");
        assert_eq!(
            input
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .count(),
            1
        );
    }
    let Message::Assistant(mut failed) = call else {
        unreachable!()
    };
    Arc::make_mut(&mut failed).stop_reason = pi_core::StopReason::Error;
    let (body, _) = run(
        request(
            vec![user("one"), Message::Assistant(failed), user("retry")],
            ThinkingLevel::Off,
        ),
        "sk-fixture",
        text_events(),
    )
    .await;
    assert_eq!(body["input"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn truncated_response_streams_do_not_become_successful_tool_calls() {
    let (_, signal) = AbortHandle::new();
    let bytes=b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\ndata: [DONE]\n\n".to_vec();
    let events = pi_plugin_openai::responses::stream(
        "openai".into(),
        "model".into(),
        "openai-responses",
        Box::pin(futures::stream::iter([Ok(bytes)])),
        signal,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(Result::is_err));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(pi_core::StreamEvent::Done { .. })))
    );
}

#[tokio::test]
async fn streamed_items_preserve_signatures_phases_and_terminal_reasoning_backfill() {
    let reason = json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thought"}]});
    let mut terminal = reason.clone();
    terminal["encrypted_content"] = json!("opaque");
    let text = json!({"type":"message","id":"msg_1","phase":"commentary","content":[{"type":"output_text","text":"before"}]});
    let call = json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{}","namespace":"tools"});
    let (_,answer)=run(request(vec![user("one")],ThinkingLevel::High),"sk-fixture",vec![
        json!({"type":"response.output_item.added","output_index":0,"item":reason}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"thought"}),
        json!({"type":"response.reasoning_summary_part.done","output_index":0}),
        json!({"type":"response.output_item.done","output_index":0,"item":reason}),
        json!({"type":"response.output_item.added","output_index":1,"item":text}),
        json!({"type":"response.output_item.done","output_index":1,"item":text}),
        json!({"type":"response.output_item.added","output_index":2,"item":call}),
        json!({"type":"response.output_item.done","output_index":2,"item":call}),
        json!({"type":"response.completed","response":{"status":"completed","output":[terminal,text,call]}}),
    ]).await;
    assert_eq!(answer.content.len(), 3);
    let saved = serde_json::to_value(&answer).unwrap();
    let signature: Value =
        serde_json::from_str(saved["content"][0]["thinkingSignature"].as_str().unwrap()).unwrap();
    assert_eq!(signature, terminal);
    assert_eq!(saved["content"][1]["text"], "before");
    let signature: Value =
        serde_json::from_str(saved["content"][1]["textSignature"].as_str().unwrap()).unwrap();
    assert_eq!(signature, json!({"v":1,"id":"msg_1","phase":"commentary"}));
    assert_eq!(answer.tool_calls()[0].id.as_str(), "call_1|fc_1");
}

#[tokio::test]
async fn replay_keeps_signed_reasoning_block_order_and_pairs_compound_tool_ids() {
    let reasoning =
        json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"});
    let assistant:Message=serde_json::from_value(json!({"role":"assistant","content":[
        {"type":"text","text":"before","textSignature":"{\"v\":1,\"id\":\"msg_1\",\"phase\":\"commentary\"}"},
        {"type":"thinking","thinking":"","thinkingSignature":reasoning.to_string()},
        {"type":"toolCall","id":"call_1|fc_1","name":"lookup","arguments":{},"namespace":"tools"}
    ],"provider":"openai","api":"openai-responses","model":"model","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":2})).unwrap();
    let result:Message=serde_json::from_value(json!({"role":"toolResult","toolCallId":"call_1|fc_1","toolName":"lookup","content":[{"type":"text","text":"result"}],"isError":false,"timestamp":3})).unwrap();
    for same in [true, false] {
        let mut request = request(
            vec![user("one"), assistant.clone(), result.clone()],
            ThinkingLevel::Off,
        );
        if !same {
            request.model = "other".into();
            request.model_spec.as_mut().unwrap().id = "other".into();
        }
        let (body, _) = run(request, "sk-fixture", text_events()).await;
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.iter().any(|item| item == &reasoning), same);
        let text = input
            .iter()
            .find(|item| item["role"] == "assistant")
            .unwrap();
        assert_eq!(text.get("phase"), same.then_some(&json!("commentary")));
        let call = input
            .iter()
            .find(|item| item["type"] == "function_call")
            .unwrap();
        assert_eq!(call["call_id"], "call_1");
        assert_eq!(call.get("id"), same.then_some(&json!("fc_1")));
        assert_eq!(call.get("namespace"), same.then_some(&json!("tools")));
        assert_eq!(input.last().unwrap()["call_id"], "call_1");
        if same {
            assert_eq!(input[2], reasoning);
        }
    }
}

#[tokio::test]
async fn system_updates_stay_in_place_or_fold_sections_without_becoming_user_messages() {
    let head:Message=serde_json::from_value(json!({"role":"system","content":"base","sections":{"style":"old","temporary":"remove me"},"timestamp":0})).unwrap();
    let change:Message=serde_json::from_value(json!({"role":"system","content":"extra","sections":{"style":"new","temporary":null},"timestamp":2})).unwrap();
    for supported in [false, true] {
        let mut request = request(
            vec![head.clone(), user("one"), change.clone(), user("two")],
            ThinkingLevel::Off,
        );
        request.model_spec.as_mut().unwrap().compat =
            Some(json!({"supportsMidConvoSystemMessages":supported}));
        let (body, _) = run(request, "sk-fixture", text_events()).await;
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), if supported { 4 } else { 3 });
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(
            input[0]["content"],
            if supported {
                "base\n\nold\n\nremove me"
            } else {
                "base\n\nextra\n\nnew"
            }
        );
        if supported {
            assert_eq!(input[2]["role"], "developer");
            assert_eq!(
                input[2]["content"],
                "extra\n\nUpdated system prompt section \"style\":\n\nnew\n\nRemoved system prompt section \"temporary\"."
            );
        }
    }
}

#[tokio::test]
async fn effort_changes_preserve_previous_request_prefix_and_stamp_only_managed_replies() {
    let (first, answer) = run(
        request(vec![user("one")], ThinkingLevel::High),
        "sk-fixture",
        text_events(),
    )
    .await;
    assert_eq!(answer.provider_thinking_level.as_deref(), Some("high"));
    let saved = serde_json::to_vec(&Message::assistant(answer)).unwrap();
    let restored: Message = serde_json::from_slice(&saved).unwrap();
    let (second, answer) = run(
        request(
            vec![user("one"), restored.clone(), user("two")],
            ThinkingLevel::Low,
        ),
        "sk-fixture",
        text_events(),
    )
    .await;
    assert_eq!(first["reasoning"]["effort"], "medium");
    assert_eq!(second["reasoning"]["effort"], "medium");
    let prefix = first["input"].as_array().unwrap();
    assert_eq!(&second["input"].as_array().unwrap()[..prefix.len()], prefix);
    assert_eq!(
        second["input"].as_array().unwrap().last(),
        Some(&update("low"))
    );
    assert_eq!(answer.provider_thinking_level.as_deref(), Some("low"));
    for (key, enabled, level) in [
        ("chatgpt-token", true, ThinkingLevel::Low),
        ("sk-fixture", false, ThinkingLevel::Low),
        ("sk-fixture", true, ThinkingLevel::Off),
    ] {
        let mut request = request(vec![user("one"), restored.clone(), user("two")], level);
        request.model_spec.as_mut().unwrap().compat =
            Some(json!({"supportsMidConvoEffort":enabled}));
        let (body, answer) = run(request, key, text_events()).await;
        assert!(answer.provider_thinking_level.is_none());
        assert!(
            body["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] != "configuration_update")
        );
        assert_eq!(
            body["reasoning"]["effort"],
            if level == ThinkingLevel::Off {
                "none"
            } else {
                "low"
            }
        );
    }
}
