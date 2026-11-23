use pi_session::{
    CompactionEntry, CustomEntry, ProvisionedEntry, SessionEntry, SessionHeader, SessionStore,
};
use serde_json::json;

fn checkpoint(boundary: Option<&str>) -> CompactionEntry {
    serde_json::from_value(json!({
        "summary":"checkpoint", "retainedTail":[], "tokensBefore":5,
        "firstKeptEntryId":boundary,
    }))
    .unwrap()
}

fn metadata() -> SessionEntry {
    SessionEntry::Custom(CustomEntry {
        custom_type: "metadata".into(),
        data: None,
    })
}

#[test]
fn compaction_boundary_must_be_on_selected_branch_before_single_or_batch_append() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let store =
        SessionStore::create(&path, SessionHeader::new("provenance", directory.path())).unwrap();
    let root = store.append_session_record(metadata()).unwrap();
    let foreign = store.append_session_record(metadata()).unwrap();
    store.branch(Some(&root.id)).unwrap();
    store.append_session_record(metadata()).unwrap();
    for id in [&foreign.id, "missing"] {
        let before = std::fs::read(&path).unwrap();
        let count = store.load().unwrap().entries.len();
        let error = store
            .append_session_record(SessionEntry::Compaction(checkpoint(Some(id))))
            .unwrap_err();
        assert!(error.to_string().contains("not an ancestor"), "{error}");
        let error = store
            .append_batch([metadata(), SessionEntry::Compaction(checkpoint(Some(id)))])
            .unwrap_err();
        assert!(error.to_string().contains("not an ancestor"), "{error}");
        assert_eq!(store.load().unwrap().entries.len(), count);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    store
        .append_batch([
            metadata(),
            SessionEntry::Compaction(checkpoint(Some(&root.id))),
        ])
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let (_, replayed) = SessionStore::open(&path).unwrap();
    assert_eq!(replayed.entries, store.load().unwrap().entries);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "replay must not rewrite history"
    );
}

#[test]
fn old_checkpoint_omission_and_imported_retain_none_self_identity_are_lossless() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let store =
        SessionStore::create(&path, SessionHeader::new("provenance", directory.path())).unwrap();
    let old: CompactionEntry = serde_json::from_value(json!({
        "summary":"old", "retainedTail":[], "tokensBefore":5,
    }))
    .unwrap();
    assert_eq!(old.first_kept_entry_id, None);
    assert!(
        serde_json::to_value(&old)
            .unwrap()
            .get("firstKeptEntryId")
            .is_none()
    );
    store
        .append_session_record(SessionEntry::Compaction(old))
        .unwrap();
    let own = store
        .append_entry(
            ProvisionedEntry {
                id: "self".into(),
                entry: SessionEntry::Compaction(checkpoint(Some("self"))),
            },
            pi_session::MAIN_LANE,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(&own).unwrap()["firstKeptEntryId"],
        "self"
    );
    let mut invalid = checkpoint(Some("invalid-self"));
    invalid.retained_tail.push(
        pi_session::AgentMessage::custom(json!({
            "role":"user", "content":"retained", "timestamp":1,
        }))
        .unwrap(),
    );
    assert!(
        store
            .append_entry(
                ProvisionedEntry {
                    id: "invalid-self".into(),
                    entry: SessionEntry::Compaction(invalid),
                },
                pi_session::MAIN_LANE
            )
            .is_err()
    );
    let (_, replayed) = SessionStore::open(&path).unwrap();
    assert_eq!(
        serde_json::to_value(replayed.entries.last().unwrap()).unwrap()["firstKeptEntryId"],
        "self"
    );
}
