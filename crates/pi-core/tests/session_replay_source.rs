use std::sync::Arc;

use pi_core::session::AgentMessage;
use serde_json::json;

#[test]
fn compact_message_releases_unselected_replay_bytes_and_preserves_wire_fields() {
    let expected = json!({
        "role": "user",
        "content": [{"type": "text", "text": "hello", "extension": {"kept": true}}],
        "timestamp": 1,
        "piRs": {"displayText": "original input"},
        "unknown": [1, 2, 3]
    });
    let mut bytes = vec![b' '; 1024 * 1024];
    let start = bytes.len();
    bytes.extend(serde_json::to_vec(&expected).unwrap());
    let range = start..bytes.len();
    let source = Arc::new(bytes);
    let weak_source = Arc::downgrade(&source);
    let original = AgentMessage::from_shared_raw_json(source.clone(), range).unwrap();
    let compact = original.compact_replay_source();

    assert_eq!(compact.as_standard(), original.as_standard());
    assert_eq!(compact.display_text(), Some("original input"));
    assert_eq!(serde_json::to_value(&compact).unwrap(), expected);
    assert!(!compact.shares_replay_source_with(&original));
    drop(original);
    drop(source);
    assert!(weak_source.upgrade().is_none());
    assert_eq!(serde_json::to_value(compact).unwrap(), expected);
}
