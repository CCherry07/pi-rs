use pi_core::Message;
use serde_json::json;

#[test]
fn tool_result_message_preserves_optional_execution_duration() {
    for duration in [None, Some(0), Some(42)] {
        let mut input = json!({
            "role":"toolResult", "toolCallId":"call", "toolName":"fixture",
            "content":[{"type":"text","text":"done"}], "isError":false, "timestamp":1
        });
        if let Some(duration) = duration {
            input["durationMs"] = json!(duration);
        }
        let message: Message = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(message).unwrap(), input);
    }
}
