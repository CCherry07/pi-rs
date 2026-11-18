use super::*;

#[test]
fn chained_js_content_patch_clears_earlier_structured_replacement() {
    let mut aggregate: JsToolResultPatch = serde_json::from_value(json!({
        "structuredContent":{"old":true}
    }))
    .unwrap();
    let next =
        serde_json::from_value(json!({"content":[{"type":"text","text":"changed"}]})).unwrap();
    aggregate.merge(&next);
    let mut result = ToolResult::text("old");
    result.structured_content = Some(json!({"original":true}));
    aggregate.into_core().apply(&mut result);
    assert_eq!(result.structured_content, None);
    assert_eq!(result.content, ToolResult::text("changed").content);
}

#[test]
fn structured_replacement_can_be_json_null_or_can_follow_a_content_patch() {
    let null: JsToolResultPatch =
        serde_json::from_value(json!({"structuredContent":null})).unwrap();
    assert_eq!(null.structured_content, Some(Value::Null));
    let mut aggregate: JsToolResultPatch = serde_json::from_value(json!({
        "content":[{"type":"text","text":"changed"}]
    }))
    .unwrap();
    aggregate
        .merge(&serde_json::from_value(json!({"structuredContent":{"changed":true}})).unwrap());
    let mut result = ToolResult::text("old");
    aggregate.into_core().apply(&mut result);
    assert_eq!(result.structured_content, Some(json!({"changed":true})));
}
