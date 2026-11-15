use pi_core::{Message, session::AgentMessage};
use serde_json::json;

#[test]
fn system_messages_and_provider_effort_survive_typed_and_session_wire_reads() {
    let system = json!({"role":"system","content":"Additional instructions","sections":{"z":"first","a":null},"timestamp":1});
    let typed: Message = serde_json::from_value(system.clone()).expect("typed system message");
    let saved: AgentMessage = serde_json::from_value(system).unwrap();
    assert_eq!(saved.role(), "system");
    assert_eq!(saved.as_standard(), Some(&typed));
    let encoded = serde_json::to_value(&typed).unwrap();
    assert_eq!(encoded["sections"]["z"], "first");
    assert_eq!(encoded["sections"]["a"], serde_json::Value::Null);

    let assistant = json!({"role":"assistant","content":[],"provider":"openai","api":"openai-responses","model":"model","providerThinkingLevel":"high","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":2});
    let typed: Message = serde_json::from_value(assistant).unwrap();
    assert_eq!(
        serde_json::to_value(typed).unwrap()["providerThinkingLevel"],
        "high"
    );
}
