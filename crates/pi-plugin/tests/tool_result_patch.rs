use pi_plugin::{ToolResult, ToolResultPatch};
use serde_json::json;

#[test]
fn content_replacement_invalidates_stale_structured_output() {
    let mut result = ToolResult::text("original");
    result.structured_content = Some(json!({"value":"original"}));
    ToolResultPatch {
        content: Some(ToolResult::text("changed").content),
        ..Default::default()
    }
    .apply(&mut result);
    assert_eq!(result.content, ToolResult::text("changed").content);
    assert_eq!(result.structured_content, None);
}

#[test]
fn explicit_structured_replacement_wins_and_metadata_only_patches_keep_it() {
    let mut result = ToolResult::text("original");
    result.structured_content = Some(json!({"value":"original"}));
    ToolResultPatch {
        content: Some(ToolResult::text("changed").content),
        structured_content: Some(json!({"value":"changed"})),
        ..Default::default()
    }
    .apply(&mut result);
    ToolResultPatch {
        details: Some(json!({"ui":"only"})),
        ..Default::default()
    }
    .apply(&mut result);
    assert_eq!(result.structured_content, Some(json!({"value":"changed"})));
}
