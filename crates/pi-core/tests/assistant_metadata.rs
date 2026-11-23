use pi_core::{Message, SystemMessage, UsageCost};
use serde_json::json;

#[test]
fn assistant_response_metadata_is_typed_and_survives_direct_serialization() {
    let wire = json!({"role":"assistant", "api":"test", "provider":"test", "model":"test",
        "content":[], "stopReason":"stop", "timestamp":1, "durationMs":17, "thinkingLevel":"off",
        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0,"totalFromProvider":false}}});
    let message: Message = serde_json::from_value(wire.clone()).unwrap();
    let actual = serde_json::to_value(message).unwrap();
    assert_eq!(actual["durationMs"], 17);
    assert_eq!(actual["thinkingLevel"], "off");
    assert_eq!(actual["usage"]["cost"]["totalFromProvider"], false);
    let cost: UsageCost = serde_json::from_value(wire["usage"]["cost"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(cost).unwrap()["totalFromProvider"],
        false
    );
}

#[test]
fn generated_system_text_uses_pi_scalar_content_without_rewriting_replayed_wire() {
    let empty = Message::System(SystemMessage {
        content: vec![],
        sections: vec![("preamble".into(), Some("fixture".into()))],
        timestamp_ms: 1,
    });
    assert_eq!(serde_json::to_value(empty).unwrap()["content"], "");
    let text = Message::System(SystemMessage::text("fixture", 1));
    assert_eq!(serde_json::to_value(text).unwrap()["content"], "fixture");
    let original = json!({"role":"system","content":[],"timestamp":1,"future":{"keep":false}});
    let replay: pi_core::session::AgentMessage = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(replay).unwrap(), original);
}
