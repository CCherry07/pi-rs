use pi_core::{CustomMessageContent, Message, Usage, UserMessage};
use pi_session::{
    ActiveToolsEntry, AgentMessage, CompactionEntry, CustomMessageEntry, ForkOptions, ForkPosition,
    ISOLATED_CONTEXT_TYPE, MAIN_LANE, SessionContextBuildOptions, SessionEntry, SessionHeader,
    SessionStore, aggregate_document_usage,
};
use serde_json::json;

fn assert_projection_matches(path: &std::path::Path) {
    let full = SessionStore::read(path).unwrap();
    let transcript = SessionStore::read_transcript(path).unwrap();
    assert_eq!(transcript.header, full.header);
    assert_eq!(transcript.name, full.name);
    assert_eq!(transcript.context, full.context().unwrap());
    assert_eq!(transcript.usage, aggregate_document_usage(&full));
    assert_eq!(
        transcript
            .branch
            .iter()
            .map(|entry| &entry.id)
            .collect::<Vec<_>>(),
        full.branch()
            .unwrap()
            .iter()
            .map(|entry| &entry.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn initial_tools_preserve_absence_empty_and_selection_across_projection_and_fork() {
    let dir = tempfile::tempdir().unwrap();
    for (name, initial) in [
        ("old-v4", None),
        ("empty", Some(vec![])),
        ("restricted", Some(vec!["read".to_string()])),
    ] {
        let path = dir.path().join(format!("{name}.jsonl"));
        let mut header = SessionHeader::new(name, dir.path());
        if let Some(tools) = &initial {
            header.metadata = Some(
                serde_json::from_value(json!({
                    "pi-rs.initialToolSelection": {"schemaVersion": 1, "activeToolNames": tools}
                }))
                .unwrap(),
            );
        }
        let store = SessionStore::create(&path, header).unwrap();
        let root = store
            .append_message(Message::User(UserMessage::text("hello", 1)))
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        assert_eq!(store.context().unwrap().active_tool_names, initial);
        assert_eq!(
            SessionStore::read_transcript(&path)
                .unwrap()
                .context
                .active_tool_names,
            initial
        );
        assert_eq!(
            SessionStore::read(&path)
                .unwrap()
                .context()
                .unwrap()
                .active_tool_names,
            initial
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "read never upgrades old journals"
        );

        for tools in [vec!["write".to_string()], vec![]] {
            store
                .append(SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                    active_tool_names: tools.clone(),
                }))
                .unwrap();
            assert_eq!(store.context().unwrap().active_tool_names, Some(tools));
            assert_projection_matches(&path);
        }
        assert_eq!(
            store
                .context_at_with_options(Some(&root), &SessionContextBuildOptions::default())
                .unwrap()
                .active_tool_names,
            initial
        );
        let document = store.load().unwrap();
        assert_eq!(
            document
                .context_at_with_options(Some(&root), &SessionContextBuildOptions::default())
                .unwrap()
                .active_tool_names,
            initial
        );
        for (kind, options, expected) in [
            (
                "branch",
                ForkOptions::Branch {
                    entry_id: Some(root.clone()),
                    position: Some(ForkPosition::At),
                },
                initial.clone(),
            ),
            ("tree", ForkOptions::Tree, Some(vec![])),
        ] {
            let fork_path = dir.path().join(format!("{name}-{kind}.jsonl"));
            let fork = store
                .fork(
                    &fork_path,
                    SessionHeader::new(format!("{name}-{kind}"), dir.path()),
                    &options,
                )
                .unwrap();
            assert_eq!(fork.context().unwrap().active_tool_names, expected);
            assert_eq!(
                fork.context_at_with_options(Some(&root), &SessionContextBuildOptions::default())
                    .unwrap()
                    .active_tool_names,
                initial
            );
            assert_projection_matches(&fork_path);
        }
    }
}

#[test]
fn malformed_initial_tool_metadata_is_rejected_without_rewriting_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    for (index, value) in [
        json!(null),
        json!({"schemaVersion": 2, "activeToolNames": []}),
        json!({"schemaVersion": 1}),
        json!({"schemaVersion": 1, "activeToolNames": null}),
        json!({"schemaVersion": 1, "activeToolNames": [1]}),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("invalid-{index}.jsonl"));
        let mut header = SessionHeader::new(format!("invalid-{index}"), dir.path());
        header.metadata =
            Some(serde_json::from_value(json!({"pi-rs.initialToolSelection": value})).unwrap());
        let bytes = format!("{}\n", serde_json::to_string(&header).unwrap());
        std::fs::write(&path, &bytes).unwrap();
        assert!(SessionStore::open_handle(&path).is_err());
        assert!(SessionStore::read_transcript(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
}

#[test]
fn transcript_omits_opaque_state_but_preserves_displayed_messages_and_wire_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let store = SessionStore::create(&path, SessionHeader::new("test", dir.path())).unwrap();
    let opaque = json!({"instructions": "large state ".repeat(100_000)});
    store
        .append_custom_entry("extension.state", Some(opaque.clone()))
        .unwrap();
    let wire = json!({
        "role": "user", "timestamp": 2,
        "content": [{"type": "text", "text": "hello", "futureField": true}],
        "futureMessage": {"kept": true}
    });
    store
        .append_message(AgentMessage::custom(wire.clone()).unwrap())
        .unwrap();
    store
        .append(SessionEntry::CustomMessage(CustomMessageEntry {
            custom_type: "extension.notice".into(),
            content: CustomMessageContent::Text("visible notice".into()),
            display: true,
            details: Some(json!({"kept": "details"})),
        }))
        .unwrap();
    store.set_name(Some("saved name".into())).unwrap();
    let before = std::fs::read(&path).unwrap();

    assert_projection_matches(&path);
    let transcript = SessionStore::read_transcript(&path).unwrap();
    assert!(
        matches!(&transcript.branch[0].entry, SessionEntry::Custom(entry) if entry.data.is_none())
    );
    assert_eq!(
        serde_json::to_value(&transcript.context.messages[0]).unwrap(),
        wire
    );
    assert_eq!(transcript.context.runtime_messages().len(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let full = SessionStore::read(&path).unwrap();
    assert!(
        matches!(&full.entries[0].entry, SessionEntry::Custom(entry) if entry.data.as_ref() == Some(&opaque))
    );
}

#[test]
fn transcript_preserves_branch_compaction_and_native_inherited_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let store = SessionStore::create(&path, SessionHeader::new("test", dir.path())).unwrap();
    let seed = store.append_custom_entry(ISOLATED_CONTEXT_TYPE, Some(json!({
        "parentSessionId": "parent", "messages": [{"role": "user", "content": "inherited", "timestamp": 1}]
    }))).unwrap();
    store
        .append_message(Message::User(UserMessage::text("abandoned branch", 2)))
        .unwrap();
    store.branch(Some(&seed)).unwrap();
    store
        .append_message(Message::User(UserMessage::text("selected branch", 3)))
        .unwrap();
    assert_projection_matches(&path);
    assert_eq!(
        SessionStore::read_transcript(&path)
            .unwrap()
            .context
            .runtime_messages()
            .len(),
        2
    );
    store
        .append(SessionEntry::Compaction(CompactionEntry {
            first_kept_entry_id: None,
            from_hook: None,
            system_message: None,
            summary: "summary".into(),
            retained_tail: vec![Message::User(UserMessage::text("retained", 3)).into()],
            retained_entry_ids: vec![],
            tokens_before: 100,
            details: None,
            usage: Some(Usage {
                input: 10,
                output: 5,
                total_tokens: 15,
                ..Usage::default()
            }),
        }))
        .unwrap();
    store
        .append_custom_entry("extension.state", Some(json!({"ignored": "after summary"})))
        .unwrap();
    store
        .append_record(pi_session::NewLaneRecord {
            id: "adjustment".into(),
            lane: MAIN_LANE.into(),
            record: pi_session::LaneRecordEntry::Usage(pi_session::UsageRecord {
                usage: pi_session::SessionUsage {
                    input: 2,
                    total_tokens: 2,
                    ..Default::default()
                },
                attribution: pi_session::UsageAttribution::Adjustment {
                    run_id: None,
                    entry_id: None,
                    details: None,
                },
            }),
        })
        .unwrap();
    assert_projection_matches(&path);
    assert_eq!(
        SessionStore::read_transcript(&path)
            .unwrap()
            .usage
            .total_tokens,
        17
    );
}

#[test]
fn transcript_validates_custom_entry_structure_and_ignores_only_a_torn_final_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let header = serde_json::to_string(&SessionHeader::new("test", dir.path())).unwrap();
    // Sorted keys also exercise alternate field ordering in the fast reader.
    let valid = json!({"kind": "entry", "type": "custom", "customType": "extension.state",
        "seq": 1, "id": "one", "parentId": null, "lane": "main", "timestamp": 1,
        "data": {"nested": [1, {"large": "some state"}]}});
    for change in [
        json!({"seq": 2}),
        json!({"parentId": "missing"}),
        json!({"lane": null}),
        json!({"customType": 42}),
        json!({"timestamp": -1}),
        json!({"type": "unknown"}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let bytes = format!("{header}\n{invalid}\n");
        std::fs::write(&path, &bytes).unwrap();
        assert!(SessionStore::read(&path).is_err(), "{change}");
        assert!(SessionStore::read_transcript(&path).is_err(), "{change}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
    for tail in ["", "\n", "\n{\"kind\":\"entry\",\"data\":{"] {
        let bytes = format!("{header}\n{valid}{tail}");
        std::fs::write(&path, &bytes).unwrap();
        assert_projection_matches(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
    let duplicate = format!("{header}\n{valid}\n{valid}\n");
    std::fs::write(&path, duplicate).unwrap();
    assert!(SessionStore::read_transcript(&path).is_err());
    let invalid_middle = format!("{header}\n{{\"data\":{{\n{valid}\n");
    std::fs::write(&path, invalid_middle).unwrap();
    assert!(SessionStore::read_transcript(&path).is_err());
}

#[test]
fn transcript_preserves_context_edits_and_configuration_across_parallel_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let store =
        SessionStore::create_deferred(&path, SessionHeader::new("test", dir.path())).unwrap();
    let target = store
        .append_message(Message::User(UserMessage::text("original", 1)))
        .unwrap();
    for _ in 0..2_100 {
        store
            .append_custom_entry("extension.state", Some(json!({"data": "x".repeat(600)})))
            .unwrap();
    }
    store
        .append(SessionEntry::ContextEdit(pi_session::ContextEditEntry {
            target_id: target,
            replacement: Some(pi_session::ContextEditReplacement {
                content: CustomMessageContent::Text("edited".into()),
            }),
        }))
        .unwrap();
    store
        .append(SessionEntry::ModelChange(pi_session::ModelChangeEntry {
            provider: pi_core::ProviderId::new("provider"),
            model_id: pi_core::ModelId::new("model"),
        }))
        .unwrap();
    store
        .append(SessionEntry::ThinkingLevelChange(
            pi_session::ThinkingLevelEntry {
                thinking_level: "high".into(),
            },
        ))
        .unwrap();
    store
        .append(SessionEntry::ActiveToolsChange(
            pi_session::ActiveToolsEntry {
                active_tool_names: vec!["read".into()],
            },
        ))
        .unwrap();
    store.materialize().unwrap();
    assert_projection_matches(&path);
    let transcript = SessionStore::read_transcript(&path).unwrap();
    assert!(transcript.branch.iter().all(|record| match &record.entry {
        SessionEntry::Custom(custom) => custom.data.is_none(),
        _ => true,
    }));
    assert_eq!(transcript.context.thinking_level, "high");
    assert_eq!(
        transcript.context.active_tool_names,
        Some(vec!["read".into()])
    );
    assert!(
        matches!(&transcript.context.runtime_messages()[0], Message::User(message)
        if message.content == UserMessage::text("edited", 1).content)
    );
}
