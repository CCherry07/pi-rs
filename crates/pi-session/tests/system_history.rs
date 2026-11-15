use pi_core::{Message, SystemMessage, session::AgentMessage};
use pi_session::{SessionHeader, SessionStore};
use serde_json::json;

#[test]
fn system_and_provider_effort_survive_jsonl_without_duplicating_tool_results() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let store =
        SessionStore::create(&path, SessionHeader::new("history", directory.path())).unwrap();
    let assistant = json!({"role":"assistant","content":[{"type":"toolCall","id":"call|fc_1","name":"lookup","arguments":{}}],"provider":"openai","api":"openai-responses","model":"model","providerThinkingLevel":"high","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":2,"futureField":{"keep":true}});
    store
        .append_message(AgentMessage::custom(assistant.clone()).unwrap())
        .unwrap();
    store
        .append_message(Message::System(SystemMessage::text("after tools", 3)))
        .unwrap();
    store.append_message(AgentMessage::custom(json!({"role":"toolResult","toolCallId":"call|fc_1","toolName":"lookup","content":[],"isError":false,"timestamp":4})).unwrap()).unwrap();
    drop(store);
    let (_, document) = SessionStore::open(&path).unwrap();
    let context = document.context().unwrap();
    assert_eq!(
        serde_json::to_value(&context.messages[0]).unwrap(),
        assistant
    );
    let messages = context.runtime_messages();
    assert_eq!(messages.len(), 3);
    assert!(
        matches!(&messages[0],Message::Assistant(message) if message.provider_thinking_level.as_deref()==Some("high"))
    );
    assert!(matches!(&messages[1], Message::ToolResult(_)));
    assert!(matches!(&messages[2],Message::System(message) if message.render()=="after tools"));
}
