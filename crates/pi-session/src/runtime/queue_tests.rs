use super::*;

#[tokio::test]
async fn next_run_queue_edits_survive_resume_and_execute_only_on_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("queue.jsonl");
    let session = AgentSession::create(
        scripted_runtime([ScriptedTurn::Text("saved".into())]),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    session.prompt("save").await.unwrap();
    for text in ["same", "same", "last"] {
        session.queue_next_run(text).await.unwrap();
    }
    let items = session.snapshot().queue.items;
    assert!(items.iter().all(|item| item.kind == QueueKind::NextRun));
    assert_ne!(items[0].entry_id, items[1].entry_id);
    session.cancel_queued(&items[0].entry_id).unwrap();
    session
        .move_queued(&items[2].entry_id, Some(&items[1].entry_id))
        .unwrap();
    assert!(
        session.cancel_queued(&items[1].entry_id).is_err(),
        "reordered identities cannot cancel a newer queue"
    );
    let expected = session.snapshot().queue.items;
    assert_eq!(
        expected
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        ["last", "same"]
    );
    let history_len = session.history().load().unwrap().entries.len();
    drop(session);
    let reopened = AgentSession::open(
        scripted_runtime([ScriptedTurn::Text("explicit queued response".into())]),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(reopened.snapshot().queue.items, expected);
    assert_eq!(
        reopened.history().load().unwrap().entries.len(),
        history_len,
        "resume must not run queued input"
    );
    reopened.run_next_queued().await.unwrap();
    assert!(
        reopened
            .history()
            .get_entry(&expected[0].entry_id)
            .is_some()
    );
    assert_eq!(reopened.snapshot().queue.items, expected[1..]);
    drop(reopened);
    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.snapshot().queue.items,
        expected[1..],
        "consumed input must not reappear on recovery"
    );
}

#[tokio::test]
async fn live_queue_edits_keep_distinct_ids_and_publish_only_after_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.jsonl");
    let session = Arc::new(
        AgentSession::create(
            scripted_runtime([
                ScriptedTurn::Text("saved".into()),
                ScriptedTurn::WaitForAbort,
            ]),
            &path,
            Default::default(),
        )
        .await
        .unwrap(),
    );
    session.prompt("save").await.unwrap();
    let running = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("wait").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !session.snapshot().agent.is_running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for text in ["same", "same", "last"] {
        session.follow_up(text).await.unwrap();
    }
    let items = session.snapshot().queue.items;
    session.cancel_queued(&items[0].entry_id).unwrap();
    session
        .move_queued(&items[2].entry_id, Some(&items[1].entry_id))
        .unwrap();
    assert_eq!(session.snapshot().queue.follow_up, ["last", "same"]);
    let agent_queue = session.runtime().agent().peek_queued_messages();
    assert!(
        matches!(&agent_queue[0], Message::User(user) if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "last"))
    );
    let before = session.snapshot();
    let moved = directory.path().join("original.jsonl");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, "foreign file\n").unwrap();
    assert!(
        session
            .cancel_queued(&before.queue.items[0].entry_id)
            .is_err()
    );
    assert_eq!(session.snapshot().revision, before.revision);
    assert_eq!(session.snapshot().queue, before.queue);
    assert_eq!(
        session.runtime().agent().peek_queued_messages(),
        agent_queue
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&moved, &path).unwrap();
    session.abort();
    running.await.unwrap().unwrap();
    assert!(
        session
            .snapshot()
            .queue
            .items
            .iter()
            .all(|item| item.kind == QueueKind::NextRun)
    );
    drop(session);
    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(reopened.snapshot().queue.follow_up, ["last", "same"]);
}
