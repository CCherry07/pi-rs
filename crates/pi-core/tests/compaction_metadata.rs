use pi_core::session::{BranchSummaryEntry, CompactionEntry};
use serde_json::{Value, json};

fn system_snapshot() -> Value {
    json!({
        "role":"system", "content":"", "sections":{"empty":"","instruction":"keep"},
        "toolsAdded":[{"name":"kept","description":"kept","parameters":{"type":"object"},"futureTool":{"keep":true}}],
        "toolsRemoved":[{"name":"removed"}], "timestamp":123,
        "futureMessage":{"keep":false}
    })
}

#[test]
fn compaction_metadata_retains_missing_and_false_hook_flags_and_full_system_snapshot() {
    for from_hook in [None, Some(false), Some(true)] {
        let mut input = json!({
            "summary":"", "retainedTail":[], "tokensBefore":100,
            "systemMessage":system_snapshot()
        });
        if let Some(from_hook) = from_hook {
            input["fromHook"] = json!(from_hook);
        }
        let entry: CompactionEntry = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(entry).unwrap(), input);

        let mut branch = json!({"fromId":"from","summary":""});
        if let Some(from_hook) = from_hook {
            branch["fromHook"] = json!(from_hook);
        }
        let entry: BranchSummaryEntry = serde_json::from_value(branch.clone()).unwrap();
        assert_eq!(serde_json::to_value(entry).unwrap(), branch);
    }
    let old = json!({"summary":"old","retainedTail":[],"tokensBefore":1});
    let entry: CompactionEntry = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(entry).unwrap(), old);
}

#[test]
fn compaction_system_snapshot_rejects_non_system_messages_and_invalid_shapes() {
    for snapshot in [
        json!("not a SystemMessage"),
        json!({"role":"user","content":"wrong role","timestamp":1}),
        json!({"role":"system","content":12,"timestamp":1}),
    ] {
        let value =
            json!({"summary":"s","retainedTail":[],"tokensBefore":1,"systemMessage":snapshot});
        assert!(serde_json::from_value::<CompactionEntry>(value).is_err());
    }
}
