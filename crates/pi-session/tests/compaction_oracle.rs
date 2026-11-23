//! Compare native context replay with the actual pinned legacy SessionManager oracle.
use pi_core::Message;
use pi_session::{
    AgentMessage, BranchSummaryEntry, CompactionEntry, SessionContextBuildOptions, SessionEntry,
    SessionRecord, build_session_context,
};
use serde::Deserialize;
use serde_json::Value;

fn corpus() -> Value {
    serde_json::from_str(include_str!("fixtures/rpc-compaction-oracle.json")).unwrap()
}

fn record(value: &Value, seq: u64, entry: SessionEntry) -> SessionRecord {
    #[derive(Deserialize)]
    struct Time {
        #[serde(with = "pi_core::session::iso_timestamp_ms")]
        timestamp: i64,
    }
    SessionRecord {
        id: value["id"].as_str().unwrap().into(),
        seq,
        parent_id: value["parentId"].as_str().map(str::to_string),
        timestamp_ms: serde_json::from_value::<Time>(value.clone())
            .unwrap()
            .timestamp,
        entry,
    }
}

#[test]
fn compaction_replay_matches_legacy_system_snapshot_summary_and_retained_tail() {
    for case in corpus()["cases"].as_array().unwrap() {
        let mut entries: Vec<_> = case["entries"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, value)| {
                record(
                    value,
                    i as u64 + 1,
                    SessionEntry::message(AgentMessage::custom(value["message"].clone()).unwrap()),
                )
            })
            .collect();
        let mut compaction: CompactionEntry =
            serde_json::from_value(case["record"].clone()).unwrap();
        if let Some(first) = case["firstKeptEntryId"].as_str() {
            let start = entries.iter().position(|entry| entry.id == first).unwrap();
            for entry in &entries[start..] {
                if let SessionEntry::Message(message) = &entry.entry {
                    compaction.retained_tail.push(message.message.clone());
                    compaction.retained_entry_ids.push(Some(entry.id.clone()));
                }
            }
        }
        entries.push(record(
            &case["record"],
            entries.len() as u64 + 1,
            SessionEntry::Compaction(compaction),
        ));
        let context = build_session_context(&entries, &SessionContextBuildOptions::default());
        assert_eq!(
            serde_json::to_value(&context.messages).unwrap(),
            case["context"],
            "case {}",
            case["name"]
        );
        assert!(matches!(
            context.provider_messages().first(),
            Some(Message::System(_))
        ));
        let projected = pi_session::project_session_entries(&entries, &Default::default());
        assert_eq!(projected[0].source_entry.id, "compaction");
        assert_eq!(projected[0].messages.len(), 2);
        if case["firstKeptEntryId"].is_string() {
            let retained = projected
                .iter()
                .find(|entry| entry.source_entry.id == "user")
                .unwrap();
            assert_eq!(retained.messages[0].role(), "user");
            entries.push(SessionRecord {
                id: "edit".into(),
                seq: entries.len() as u64 + 1,
                parent_id: Some("compaction".into()),
                timestamp_ms: 1738368000124,
                entry: SessionEntry::ContextEdit(pi_session::ContextEditEntry {
                    target_id: "user".into(),
                    replacement: Some(
                        serde_json::from_value(serde_json::json!({"content":"edited"})).unwrap(),
                    ),
                }),
            });
            let edited = build_session_context(&entries, &Default::default());
            let user = edited
                .messages
                .iter()
                .find(|message| message.role() == "user")
                .unwrap();
            assert_eq!(serde_json::to_value(user).unwrap()["content"], "edited");
        }
    }
}

#[tokio::test]
async fn live_compaction_captures_the_current_system_snapshot_at_its_committed_timestamp() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let store = pi_session::SessionStore::create(
        &path,
        pi_session::SessionHeader::new("live", directory.path()),
    )
    .unwrap();
    let fixture = corpus();
    for message in fixture["systemFold"]["messages"].as_array().unwrap() {
        store
            .append_message(AgentMessage::custom(message.clone()).unwrap())
            .unwrap();
    }
    store
        .append_message(pi_core::Message::User(pi_core::UserMessage::text(
            "request", 3,
        )))
        .unwrap();
    drop(store);
    let runtime = pi_runtime::PiRuntime::builder()
        .provider_plugin(pi_test_support::ScriptedProviderPlugin::scripted([]))
        .agent_options(pi_agent::AgentOptions {
            cwd: directory.path().to_path_buf(),
            provider_id: "scripted".into(),
            model_id: "test".into(),
            ..Default::default()
        })
        .build()
        .unwrap();
    let session = pi_session::AgentSession::open(runtime, &path, Default::default())
        .await
        .unwrap();
    let mut events = session.subscribe();
    session
        .append_compaction(CompactionEntry {
            first_kept_entry_id: None,
            from_hook: None,
            system_message: None,
            summary: "".into(),
            retained_tail: vec![],
            retained_entry_ids: vec![],
            tokens_before: 100,
            details: None,
            usage: None,
        })
        .await
        .unwrap();
    let document = session.history().load().unwrap();
    let record = document
        .entries
        .iter()
        .rev()
        .find(|entry| matches!(entry.entry, SessionEntry::Compaction(_)))
        .unwrap();
    let SessionEntry::Compaction(compaction) = &record.entry else {
        unreachable!()
    };
    assert_eq!(compaction.from_hook, Some(false));
    let mut expected = fixture["systemFold"]["expected"].clone();
    expected["timestamp"] = serde_json::json!(record.timestamp_ms);
    assert_eq!(
        serde_json::to_value(compaction.system_message.as_ref().unwrap()).unwrap(),
        expected
    );
    let mut estimate = None;
    while let Ok(event) = events.events.try_recv() {
        if let pi_session::AgentSessionEvent::CompactionEnd {
            estimated_tokens_after,
            ..
        } = event.event
        {
            estimate = estimated_tokens_after;
        }
    }
    assert_eq!(
        estimate,
        fixture["cases"][1]["estimatedTokensAfter"].as_u64()
    );
    session.shutdown().await;
}

#[test]
fn imported_system_snapshot_preserves_complete_wire_before_even_an_empty_summary() {
    let fixture = corpus();
    let raw = &fixture["imported"]["entry"];
    let compaction: CompactionEntry = serde_json::from_value(raw.clone()).unwrap();
    let context = build_session_context(
        &[record(raw, 1, SessionEntry::Compaction(compaction))],
        &Default::default(),
    );
    assert_eq!(
        serde_json::to_value(&context.messages).unwrap(),
        fixture["imported"]["context"]
    );
    assert_eq!(
        serde_json::to_value(&context.messages[0]).unwrap(),
        raw["systemMessage"]
    );
    assert!(matches!(
        context.provider_messages().first(),
        Some(Message::System(_))
    ));
}

#[test]
fn empty_branch_summary_is_not_a_context_message_but_metadata_still_round_trips() {
    for case in corpus()["branchCases"].as_array().unwrap() {
        let raw = &case["entry"];
        let summary: BranchSummaryEntry = serde_json::from_value(raw.clone()).unwrap();
        let metadata = serde_json::to_value(&summary).unwrap();
        assert_eq!(metadata.get("fromHook"), raw.get("fromHook"));
        let context = build_session_context(
            &[record(raw, 1, SessionEntry::BranchSummary(summary))],
            &Default::default(),
        );
        assert_eq!(
            serde_json::to_value(&context.messages).unwrap(),
            case["context"]
        );
    }
}
