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
        cache_retention: None,
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

fn tool_call(id: &str, stop: pi_core::StopReason) -> Message {
    serde_json::from_value(json!({
        "role": "assistant",
        "content": [{"type": "toolCall", "id": id, "name": "lookup", "arguments": {}}],
        "provider": "openai", "api": "openai-responses", "model": "model",
        "usage": pi_core::Usage::default(), "stopReason": stop, "timestamp": 2
    }))
    .unwrap()
}

fn tool_result(id: &str, text: &str) -> Message {
    serde_json::from_value(json!({
        "role": "toolResult", "toolCallId": id, "toolName": "lookup",
        "content": [{"type": "text", "text": text}], "isError": false, "timestamp": 3
    }))
    .unwrap()
}

#[test]
fn failed_replies_and_their_tool_results_are_excluded_together() {
    for stop in [pi_core::StopReason::Error, pi_core::StopReason::Aborted] {
        let messages = vec![
            user("one"),
            tool_call("call_kept|fc_kept", pi_core::StopReason::ToolUse),
            tool_result("call_kept|fc_kept", "completed result"),
            tool_call("call_failed|fc_failed", stop),
            Message::System(pi_core::SystemMessage::text("new instruction", 4)),
            tool_result("call_failed|fc_failed", "interrupted result"),
            user("retry"),
        ];
        let original = messages.clone();
        let body = pi_plugin_openai::responses::request_body(&request(
            messages.clone(),
            ThinkingLevel::Off,
        ))
        .unwrap();
        for input in [
            body["input"].as_array().unwrap().clone(),
            pi_plugin_openai::responses::input_items(&messages),
        ] {
            let tools: Vec<_> = input
                .iter()
                .filter(|item| item.get("call_id").is_some())
                .collect();
            assert_eq!(tools.len(), 2, "failed tool history must be omitted");
            assert_eq!(tools[0]["type"], "function_call");
            assert_eq!(tools[0]["call_id"], "call_kept");
            assert_eq!(tools[1]["type"], "function_call_output");
            assert_eq!(tools[1]["call_id"], "call_kept");
            assert_eq!(tools[1]["output"], "completed result");
            assert_eq!(input.last().unwrap()["role"], "user");
        }
        assert_eq!(
            messages, original,
            "replay must leave stored history intact"
        );
    }
}

#[test]
fn replay_accepts_one_result_per_pending_call_after_id_normalization() {
    for provider in ["openai", "other-provider"] {
        let mut request = request(
            vec![
                user("one"),
                tool_result("unknown", "orphan result"),
                tool_call("call_kept|fc_kept", pi_core::StopReason::ToolUse),
                tool_result("call_kept|fc_kept", "first result"),
                tool_result("call_kept|fc_kept", "duplicate result"),
                tool_call("call_missing|fc_missing", pi_core::StopReason::ToolUse),
                user("continue"),
                tool_result("call_missing|fc_missing", "late result"),
            ],
            ThinkingLevel::Off,
        );
        request.model_spec.as_mut().unwrap().provider = provider.into();
        let body = pi_plugin_openai::responses::request_body(&request).unwrap();
        let tools: Vec<_> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item.get("call_id").is_some())
            .collect();
        assert_eq!(tools.len(), 4);
        for pair in tools.as_chunks::<2>().0 {
            assert_eq!(pair[0]["type"], "function_call");
            assert_eq!(pair[1]["type"], "function_call_output");
            assert_eq!(pair[0]["call_id"], pair[1]["call_id"]);
        }
        assert_eq!(tools[1]["output"], "first result");
        assert_eq!(tools[3]["output"], "No result provided");
    }
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
async fn incomplete_responses_preserve_terminal_metadata_before_reporting_failure() {
    let (_, signal) = AbortHandle::new();
    let bytes = format!("data: {}\n\n", json!({
        "type":"response.incomplete",
        "response":{"id":"filtered","status":"incomplete","incomplete_details":{"reason":"content_filter"}}
    })).into_bytes();
    let events = pi_plugin_openai::responses::stream(
        "openai".into(),
        "model".into(),
        "openai-responses",
        Box::pin(futures::stream::iter([Ok(bytes)])),
        signal,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(
        matches!(&events[1], Ok(pi_core::StreamEvent::Metadata { patch })
        if patch.response_id.as_deref() == Some("filtered") && patch.raw_stop_reason.as_deref() == Some("incomplete.content_filter"))
    );
    assert!(events.last().unwrap().is_err());
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

fn grammar_tool() -> pi_core::ToolSpec {
    pi_core::ToolSpec {
        name: "execute".into(),
        label: "Execute".into(),
        description: "Execute source".into(),
        parameters: json!({"type":"object","properties":{"source":{"type":"string"}},"required":["source"]}),
        output_schema: None,
        constrained_sampling: Some(pi_core::ConstrainedSampling::Grammar {
            variants: BTreeMap::from([(
                pi_core::GrammarFormat::OpenAiLark,
                "start: /[\\s\\S]*/".into(),
            )]),
        }),
        execution_mode: pi_core::ToolExecutionMode::Parallel,
        prompt_snippet: None,
        prompt_guidelines: vec![],
    }
}

#[tokio::test]
async fn grammar_stream_and_replay_preserve_raw_input_and_custom_item_ids() {
    let source = "text(\"你好\");\nreturn `done`";
    let mut initial = request(vec![user("run")], ThinkingLevel::Off);
    initial.tools = vec![grammar_tool()];
    initial.model_spec.as_mut().unwrap().compat = Some(json!({"supportsOpenAIGrammarTools":true}));
    let added = json!({"type":"custom_tool_call","id":"ctc_1","call_id":"call_1","name":"execute","input":""});
    let mut completed = added.clone();
    completed["input"] = json!(source);
    let (body, answer) = run(initial.clone(), "sk-fixture", vec![
        json!({"type":"response.output_item.added","output_index":0,"item":added}),
        json!({"type":"response.custom_tool_call_input.delta","output_index":0,"delta":"text(\""}),
        json!({"type":"response.custom_tool_call_input.delta","output_index":0,"delta":"你好\");\n"}),
        json!({"type":"response.custom_tool_call_input.done","output_index":0,"input":source}),
        json!({"type":"response.output_item.done","output_index":0,"item":completed}),
        json!({"type":"response.completed","response":{"status":"completed","output":[completed]}}),
    ]).await;
    assert_eq!(body["tools"][0]["type"], "custom");
    assert_eq!(body["tools"][0]["format"]["syntax"], "lark");
    assert_eq!(answer.tool_calls()[0].id.as_str(), "call_1|ctc_1");
    assert_eq!(answer.tool_calls()[0].arguments, json!({"source":source}));
    initial.messages.push(Message::assistant(answer));
    initial.messages.push(serde_json::from_value(json!({"role":"toolResult","toolCallId":"call_1|ctc_1","toolName":"execute","content":[{"type":"text","text":"done"}],"isError":false,"timestamp":3})).unwrap());
    for enabled in [true, false] {
        initial.model_spec.as_mut().unwrap().compat =
            Some(json!({"supportsOpenAIGrammarTools":enabled}));
        let (body, _) = run(initial.clone(), "sk-fixture", text_events()).await;
        let input = body["input"].as_array().unwrap();
        let call = &input[1];
        assert_eq!(
            call["type"],
            if enabled {
                "custom_tool_call"
            } else {
                "function_call"
            }
        );
        assert_eq!(call.get("id"), enabled.then_some(&json!("ctc_1")));
        if enabled {
            assert_eq!(call["input"], source);
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(call["arguments"].as_str().unwrap()).unwrap(),
                json!({"source":source})
            );
        }
        assert_eq!(
            input[2]["type"],
            if enabled {
                "custom_tool_call_output"
            } else {
                "function_call_output"
            }
        );
        assert_eq!(input[2]["call_id"], "call_1");
    }
}

#[tokio::test]
async fn grammar_final_only_empty_input_closes_to_valid_json() {
    let mut initial = request(vec![user("run")], ThinkingLevel::Off);
    initial.tools = vec![grammar_tool()];
    initial.model_spec.as_mut().unwrap().compat = Some(json!({"supportsOpenAIGrammarTools":true}));
    let completed = json!({"type":"custom_tool_call","id":"ctc_1","call_id":"call_1","name":"execute","input":""});
    let (_, answer) = run(
        initial,
        "sk-fixture",
        vec![
            json!({"type":"response.output_item.done","output_index":0,"item":completed}),
            json!({"type":"response.completed","response":{"status":"completed"}}),
        ],
    )
    .await;
    assert_eq!(answer.tool_calls()[0].arguments, json!({"source":""}));
}

#[tokio::test]
async fn strict_tools_convert_safely_and_required_support_fails_before_http() {
    let mut initial = request(vec![user("run")], ThinkingLevel::Off);
    let mut tool = grammar_tool();
    tool.parameters = json!({"type":"object","properties":{"source":{"type":"string"},"limit":{"type":"integer"}},"required":["source"]});
    tool.constrained_sampling = Some(pi_core::ConstrainedSampling::JsonSchema {
        strict: pi_core::ToolSchemaStrict::Prefer,
    });
    initial.tools = vec![tool];
    initial.model_spec.as_mut().unwrap().compat = Some(json!({"supportsStrictMode":true}));
    let (body, _) = run(initial.clone(), "sk-fixture", text_events()).await;
    let tool = &body["tools"][0];
    assert_eq!(tool["strict"], true);
    let mut required = tool["parameters"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect::<Vec<_>>();
    required.sort_unstable();
    assert_eq!(required, ["limit", "source"]);
    assert_eq!(
        tool["parameters"]["properties"]["limit"]["anyOf"][1],
        json!({"type":"null"})
    );
    assert!(
        initial.tools[0].parameters["properties"]["limit"]
            .get("anyOf")
            .is_none()
    );
    initial.tools[0].parameters["additionalProperties"] = json!(true);
    let (body, _) = run(initial.clone(), "sk-fixture", text_events()).await;
    assert_eq!(body["tools"][0]["strict"], false);
    initial.tools[0].constrained_sampling = Some(pi_core::ConstrainedSampling::JsonSchema {
        strict: pi_core::ToolSchemaStrict::Require,
    });
    let error = pi_plugin_openai::responses::request_body(&initial).unwrap_err();
    assert!(error.to_string().contains("additionalProperties"));
    initial.model_spec.as_mut().unwrap().compat = Some(json!({"supportsStrictMode":false}));
    assert!(
        pi_plugin_openai::responses::request_body(&initial)
            .unwrap_err()
            .to_string()
            .contains("strict tools are unsupported")
    );
}
