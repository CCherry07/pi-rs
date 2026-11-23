//! Exact current-Pi source cut IDs survive generation, persistence, forks and RPC.
use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId};
use pi_rpc::json_wire::{session_document_entries_json, session_event_json};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentMessage, AgentSession, AgentSessionEvent, AgentSessionOptions, ForkOptions, SessionEntry,
    SessionHeader, SessionRecord, SessionStore,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

#[tokio::test]
async fn current_oracle_invisible_cut_survives_live_persisted_rpc_replay_and_fork() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../pi-session/tests/fixtures/compaction-current-oracle.json"
    ))
    .unwrap();
    for name in [
        "metadata-before-turn-start",
        "recovery-two-turn-split",
        "repeated-compaction-retains-invisible-metadata-cut",
        "repeated-compaction-retains-omitted-recovery-cut",
    ] {
        let case = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let summary = fixture["summaries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let boundary = summary["firstKeptEntryId"].as_str().unwrap();
        assert_eq!(summary["persistedFirstKeptEntryId"], boundary);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let store =
            SessionStore::create(&path, SessionHeader::new(name, directory.path())).unwrap();
        let records: Vec<SessionRecord> =
            serde_json::from_value(case["rustEntries"].clone()).unwrap();
        for mut record in records {
            if let SessionEntry::Message(message) = &mut record.entry
                && message.message.role() == "assistant"
            {
                let mut value = serde_json::to_value(&message.message).unwrap();
                value["provider"] = json!("scripted");
                value["model"] = json!("test");
                message.message = AgentMessage::custom(value).unwrap();
            }
            store
                .append_entry(record.provisioned(), pi_session::MAIN_LANE)
                .unwrap();
        }
        drop(store);
        let plugin = ScriptedProviderPlugin::scripted(
            summary["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|_| ScriptedTurn::Text("summary".into())),
        );
        let runtime = PiRuntime::builder()
            .provider_plugin(plugin)
            .agent_options(AgentOptions {
                cwd: directory.path().to_path_buf(),
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::open(
            runtime,
            &path,
            AgentSessionOptions::default()
                .compaction(serde_json::from_value(case["settings"].clone()).unwrap()),
        )
        .await
        .unwrap();
        let mut events = session.subscribe();
        let generated = session.compact(None).await.unwrap();
        assert_eq!(generated.first_kept_entry_id.as_deref(), Some(boundary));
        // Neither invisible cut is recoverable by matching the first retained snapshot.
        assert_ne!(
            generated
                .retained_entry_ids
                .first()
                .and_then(Option::as_deref),
            Some(boundary)
        );
        let mut end = None;
        while let Ok(event) = events.events.try_recv() {
            if matches!(&event.event, AgentSessionEvent::CompactionEnd { .. }) {
                end = session_event_json(event.event, &session).unwrap();
            }
        }
        assert_eq!(end.unwrap()["result"]["firstKeptEntryId"], boundary);
        let live_context = session.history().context().unwrap().messages;
        let document = session.history().load().unwrap();
        let checkpoint = document.entries.last().unwrap();
        assert_eq!(
            serde_json::to_value(checkpoint).unwrap()["firstKeptEntryId"],
            boundary
        );
        assert_eq!(
            session_document_entries_json(&document)
                .unwrap()
                .last()
                .unwrap()["firstKeptEntryId"],
            boundary
        );
        session.shutdown().await;

        let (replayed, document) = SessionStore::open(&path).unwrap();
        assert_eq!(replayed.context().unwrap().messages, live_context);
        assert_eq!(
            session_document_entries_json(&document)
                .unwrap()
                .last()
                .unwrap()["firstKeptEntryId"],
            boundary
        );
        // Native forks preserve original entry IDs (only sequence numbers are reassigned).
        for (index, options) in [
            ForkOptions::Tree,
            ForkOptions::Branch {
                entry_id: None,
                position: None,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let fork = replayed
                .fork(
                    directory.path().join(format!("fork-{index}.jsonl")),
                    SessionHeader::new(format!("fork-{index}"), directory.path()),
                    &options,
                )
                .unwrap();
            let document = fork.load().unwrap();
            assert_eq!(fork.context().unwrap().messages, live_context);
            assert!(document.entries.iter().any(|record| record.id == boundary));
            assert_eq!(
                session_document_entries_json(&document)
                    .unwrap()
                    .last()
                    .unwrap()["firstKeptEntryId"],
                boundary
            );
        }
    }
}
