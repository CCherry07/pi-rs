use pi_rpc::json_wire::session_document_entries_json;
use pi_session::{BranchSummaryEntry, CompactionEntry, SessionEntry, SessionHeader, SessionStore};
use serde_json::{Value, json};

#[test]
fn history_projection_preserves_compaction_metadata_and_retain_none_identity() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../pi-session/tests/fixtures/rpc-compaction-oracle.json"
    ))
    .unwrap();
    let raw = &fixture["imported"]["entry"];
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::create(
        directory.path().join("session.jsonl"),
        SessionHeader::new("fixture", directory.path()),
    )
    .unwrap();
    let payload: CompactionEntry = serde_json::from_value(raw.clone()).unwrap();
    // Preserve the imported record identity: its retain-none marker is self-referential.
    let record = store
        .append_entry(
            pi_session::ProvisionedEntry {
                id: raw["id"].as_str().unwrap().into(),
                entry: SessionEntry::Compaction(payload),
            },
            pi_session::MAIN_LANE,
        )
        .unwrap();
    let document = store.load().unwrap();
    let projected = session_document_entries_json(&document).unwrap();
    let entry = projected
        .iter()
        .find(|entry| entry["id"] == record.id)
        .unwrap();
    let mut system = raw["systemMessage"].clone();
    system["timestamp"] = json!(record.timestamp_ms);
    assert_eq!(entry.get("fromHook"), Some(&json!(false)));
    assert_eq!(entry["systemMessage"], system);
    // Pi persists the compaction's own ID when no old entry is retained. Its
    // runtime compact result may still return null; the two shapes are distinct.
    assert_eq!(entry["firstKeptEntryId"], record.id);
    assert!(entry.get("estimatedTokensAfter").is_none());

    for from_hook in [None, Some(false), Some(true)] {
        let record = store
            .append_session_record(SessionEntry::BranchSummary(BranchSummaryEntry {
                from_hook,
                from_id: record.id.clone(),
                summary: "branch".into(),
                details: None,
                usage: None,
            }))
            .unwrap();
        let projected = session_document_entries_json(&store.load().unwrap()).unwrap();
        let entry = projected
            .iter()
            .find(|entry| entry["id"] == record.id)
            .unwrap();
        assert_eq!(
            entry.get("fromHook").cloned(),
            from_hook.map(|value| json!(value))
        );
    }
}
