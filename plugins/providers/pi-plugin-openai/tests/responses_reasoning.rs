use futures::StreamExt;
use pi_core::{AbortHandle, AssistantMessage, ContentBlock, ModelId, ProviderId, StopReason};
use pi_plugin::ProviderError;
use pi_plugin_openai::responses;
use pi_provider::HttpBodyStream;
use serde_json::{Value, json};

async fn replay(events: Vec<Value>) -> Result<AssistantMessage, ProviderError> {
    let bytes = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>()
        .into_bytes();
    let body: HttpBodyStream = Box::pin(futures::stream::iter([Ok(bytes)]));
    let (_, signal) = AbortHandle::new();
    let mut stream = responses::stream(
        ProviderId::new("test"),
        ModelId::new("model"),
        responses::OPENAI_RESPONSES_API,
        body,
        signal,
    );
    let mut assembler = pi_agent::StreamAssembler::new();
    while let Some(event) = stream.next().await {
        assembler.push(event?).unwrap();
    }
    Ok(assembler.finish().unwrap())
}

#[tokio::test]
async fn reasoning_summary_part_done_matches_final_content() {
    let item = json!({
        "type": "reasoning", "id": "rs_1",
        "summary": [{"type": "summary_text", "text": "thought"}]
    });
    let message = replay(vec![
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"rs_1","summary_index":0,"delta":"thought"}),
        json!({"type":"response.reasoning_summary_part.done","output_index":0,"item_id":"rs_1","summary_index":0,"part":{"type":"summary_text","text":"thought"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"status":"completed"}}),
    ])
    .await
    .unwrap();
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert!(matches!(
        &message.content[0],
        ContentBlock::Thinking(thinking)
            if thinking.thinking == "thought"
                && thinking.thinking_signature.as_deref() == Some(item.to_string().as_str())
    ));
}

#[tokio::test]
async fn reasoning_without_part_done_matches_final_content() {
    let message = replay(vec![
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"thought"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":"thought"}]}}),
        json!({"type":"response.completed","response":{"status":"completed"}}),
    ])
    .await
    .unwrap();
    assert!(matches!(&message.content[0], ContentBlock::Thinking(t) if t.thinking == "thought"));
}

#[tokio::test]
async fn reasoning_rejects_conflicting_final_content() {
    for (delta, final_text) in [
        ("thought", "changed"),
        ("thought\n", "thought"),
        ("thought\n\n", "thought"),
    ] {
        let error = replay(vec![
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":delta}),
            json!({"type":"response.reasoning_summary_part.done","output_index":0}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":final_text}]}}),
            json!({"type":"response.completed","response":{"status":"completed"}}),
        ])
        .await
        .unwrap_err();
        assert!(
            matches!(error, ProviderError::Protocol(message) if message == "Responses final content contradicts streamed deltas")
        );
    }
}

#[tokio::test]
async fn reasoning_parts_preserve_whitespace_and_allow_tool_continuation() {
    for parts in [
        vec!["first", "second"],
        vec!["first", "", "third"],
        vec!["", "second"],
        vec!["first\n\n", "second \n\n"],
    ] {
        let summary = parts
            .iter()
            .map(|text| json!({"type":"summary_text","text":text}))
            .collect::<Vec<_>>();
        let item = json!({"type":"reasoning","id":"rs_1","summary":summary});
        let mut events = vec![
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
        ];
        for (index, part) in summary.iter().enumerate() {
            events.extend([
                json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":index,"delta":part["text"]}),
                json!({"type":"response.reasoning_summary_part.done","output_index":0,"summary_index":index,"part":part}),
                // Empty deltas must not flush the pending separator.
                json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":index,"delta":""}),
            ]);
        }
        let mut terminal_item = item.clone();
        terminal_item["encrypted_content"] = json!("opaque");
        events.extend([
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{}"}}),
            json!({"type":"response.completed","response":{"status":"completed","output":[terminal_item]}}),
        ]);
        let message = replay(events).await.unwrap();
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert_eq!(message.content.len(), 2);
        assert!(matches!(
            &message.content[0], ContentBlock::Thinking(t)
                if t.thinking == parts.join("\n\n")
                    && t.thinking_signature.as_deref() == Some(terminal_item.to_string().as_str())
        ));
        assert_eq!(message.tool_calls()[0].id.as_str(), "call_1|fc_1");
    }
}

#[tokio::test]
async fn reasoning_final_content_backfills_unstreamed_parts() {
    let message = replay(vec![
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"first"}),
        json!({"type":"response.reasoning_summary_part.done","output_index":0}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":"first"},{"type":"summary_text","text":"second"}]}}),
        json!({"type":"response.completed","response":{"status":"completed"}}),
    ])
    .await
    .unwrap();
    assert!(
        matches!(&message.content[0], ContentBlock::Thinking(t) if t.thinking == "first\n\nsecond")
    );
}
