use super::*;

#[tokio::test]
async fn started_delivery_keeps_its_journal_identity_until_commit_and_restores_only_if_uncommitted()
{
    for commit in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("in-flight.jsonl");
        let session = AgentSession::create(
            scripted_runtime([ScriptedTurn::Text("saved".into())]),
            &path,
            Default::default(),
        )
        .await
        .unwrap();
        session.prompt("materialize").await.unwrap();
        let mut events = session.subscribe();
        let agent = session.runtime.agent();
        let guard = session
            .activity
            .begin(crate::runtime::activity::ActivityPhase::Running)
            .await;
        let input = Message::User(UserMessage::text("original", 1));
        let run = session
            .activity
            .begin_messages(1, std::slice::from_ref(&input))
            .unwrap();
        session.activity.message_started(&input, &agent.downgrade());
        session.activity.message_ended(&input, None).unwrap();
        let queued = Message::User(UserMessage::text("queued", 2));
        let other = Message::User(UserMessage::text("other", 3));
        let PromptOutcome::Queued { entry_id: id, .. } = session
            .activity
            .enqueue_message(agent, queued.clone(), QueueKind::FollowUp)
            .unwrap()
        else {
            panic!("queued")
        };
        let PromptOutcome::Queued {
            entry_id: other_id, ..
        } = session
            .activity
            .enqueue_message(agent, other.clone(), QueueKind::FollowUp)
            .unwrap()
        else {
            panic!("queued")
        };
        // Simulate the Agent's drain, keeping the second input live/editable.
        assert!(
            agent
                .replace_identified_queue(
                    pi_agent::QueueDestination::FollowUp,
                    &[id.clone(), other_id.clone()],
                    vec![(other_id, other)],
                    || Ok::<_, SessionError>(())
                )
                .unwrap()
        );
        let consumed = session
            .activity
            .message_started(&queued, &agent.downgrade())
            .unwrap();
        assert_eq!(consumed.follow_up, ["other"]);
        session.events.publish_queue(consumed);
        assert!(
            session.history().get_entry(&id).is_none(),
            "start must not persist an unpatched input"
        );
        session.clear_queue().unwrap();
        assert!(session.snapshot().queue.items.is_empty());
        if commit {
            let patched = Message::User(UserMessage::text("hook-patched delivery", 2));
            let result = session.activity.message_ended(&patched, None).unwrap();
            assert_eq!(result.entry.id, id);
            assert!(
                result.queue.is_none(),
                "there must be no second consumption event"
            );
            assert_eq!(
                serde_json::to_value(&result.entry.entry).unwrap()["message"]["content"][0]["text"],
                "hook-patched delivery"
            );
        }
        session
            .activity
            .finish_run(
                agent,
                &run,
                &Err(SessionError::Runtime("delivery interrupted".into())),
            )
            .unwrap();
        drop(guard);
        let expected = if commit {
            Vec::<&str>::new()
        } else {
            vec!["queued"]
        };
        assert_eq!(session.snapshot().queue.follow_up, expected);
        if !commit {
            let mut restored = None;
            while let Ok(event) = events.events.try_recv() {
                if let AgentSessionEvent::QueueUpdate { origin, items, .. } = event.event {
                    restored = Some((origin, items));
                }
            }
            let (origin, items) = restored.unwrap();
            assert_eq!(origin, crate::QueueUpdateOrigin::Mutation);
            assert_eq!(items.len(), 1);
            assert_ne!(items[0].entry_id, id);
        }
        session.shutdown().await;
        drop(session);
        let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
            .await
            .unwrap();
        assert_eq!(reopened.snapshot().queue.follow_up, expected);
        assert!(
            reopened
                .snapshot()
                .queue
                .items
                .iter()
                .all(|item| item.kind == QueueKind::NextRun)
        );
    }
}

#[tokio::test]
async fn aborted_follow_up_waits_for_an_explicit_prompt_then_keeps_its_delivery_mode() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("aborted-follow-up.jsonl");
    let plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::WaitForAbort,
        ScriptedTurn::Text("resumed answer".into()),
        ScriptedTurn::Text("queued answer".into()),
    ]);
    let provider = plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(plugin)
        .build()
        .unwrap();
    let session = Arc::new(
        AgentSession::create(runtime, &path, Default::default())
            .await
            .unwrap(),
    );
    let mut events = session.subscribe();
    let running = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("first prompt").await.unwrap() })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while provider.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    session.follow_up("surviving follow-up").await.unwrap();
    let original_id = session.snapshot().queue.items[0].entry_id.clone();
    session.abort_and_wait().await;
    running.await.unwrap();
    assert!(!session.snapshot().agent.is_running);
    assert_eq!(
        provider.requests().len(),
        1,
        "abort must not implicitly restart a request"
    );
    assert_eq!(session.snapshot().queue.follow_up, ["surviving follow-up"]);
    assert_ne!(session.snapshot().queue.items[0].entry_id, original_id);
    let mut mutations = 0;
    let mut rebound = Vec::new();
    while let Ok(event) = events.events.try_recv() {
        if let AgentSessionEvent::QueueUpdate { origin, items, .. } = event.event {
            match origin {
                crate::QueueUpdateOrigin::Mutation => mutations += 1,
                crate::QueueUpdateOrigin::Retimed => rebound.push(items),
            }
        }
    }
    assert_eq!(
        mutations, 1,
        "only the explicit enqueue changes the Pi queue"
    );
    assert_eq!(
        rebound,
        [session.snapshot().queue.items],
        "native observers receive the new editable identities"
    );

    session.prompt("resume after abort").await.unwrap();
    assert_eq!(
        provider.requests().len(),
        3,
        "the explicit prompt must allow the queued follow-up turn"
    );
    assert!(session.snapshot().queue.items.is_empty());
    let mut origins = Vec::new();
    let mut consumption = Vec::new();
    while let Ok(event) = events.events.try_recv() {
        match event.event {
            AgentSessionEvent::QueueUpdate { origin, .. } => {
                if origin == crate::QueueUpdateOrigin::Mutation {
                    consumption.push("queue_empty");
                }
                origins.push(origin);
            }
            AgentSessionEvent::Agent(event) => match *event {
                AgentEvent::MessageStart {
                    message: Message::User(user),
                } if user.content
                    == pi_core::UserMessage::text("surviving follow-up", 0).content =>
                {
                    consumption.push("user_start");
                }
                AgentEvent::MessageEnd {
                    message: Message::User(user),
                } if user.content
                    == pi_core::UserMessage::text("surviving follow-up", 0).content =>
                {
                    consumption.push("user_end");
                }
                _ => {}
            },
            _ => {}
        }
    }
    assert_eq!(
        consumption,
        ["queue_empty", "user_start", "user_end"],
        "consumption is visible before the queued message starts"
    );
    assert_eq!(
        origins,
        [
            crate::QueueUpdateOrigin::Retimed,
            crate::QueueUpdateOrigin::Mutation
        ],
        "binding is not a second enqueue; consumption changes the queue"
    );
    let texts = session
        .snapshot()
        .agent
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => match user.content.first() {
                Some(ContentBlock::Text(text)) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        ["first prompt", "resume after abort", "surviving follow-up"]
    );
    assert!(
        matches!(provider.requests()[2].messages.last(), Some(Message::User(user)) if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "surviving follow-up"))
    );
    session.shutdown().await;
    drop(session);
    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert!(
        reopened.snapshot().queue.items.is_empty(),
        "delivered input must not replay on resume"
    );
}

#[tokio::test]
async fn aborted_live_queues_keep_modes_in_either_arrival_order_without_admitting_next_run() {
    for steer_first in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("aborted-mixed-queue.jsonl");
        let plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::WaitForAbort,
            ScriptedTurn::Text("resumed answer".into()),
            ScriptedTurn::Text("follow-up answer".into()),
        ]);
        let provider = plugin.provider();
        let session = Arc::new(
            AgentSession::create(
                PiRuntime::builder()
                    .provider_plugin(plugin)
                    .build()
                    .unwrap(),
                &path,
                Default::default(),
            )
            .await
            .unwrap(),
        );
        let running = {
            let session = session.clone();
            tokio::spawn(async move { session.prompt("first").await.unwrap() })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while provider.requests().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Equal payloads still represent two distinct user operations and modes.
        if steer_first {
            session.steer("same").await.unwrap();
            session.follow_up("same").await.unwrap();
        } else {
            session.follow_up("same").await.unwrap();
            session.steer("same").await.unwrap();
        }
        session.queue_next_run("parked explicitly").await.unwrap();
        let original = session.snapshot().queue;
        session.abort_and_wait().await;
        running.await.unwrap();
        assert_eq!(provider.requests().len(), 1);
        let retimed = session.snapshot().queue;
        assert_eq!(retimed.steering, ["same"]);
        assert_eq!(retimed.follow_up, ["parked explicitly", "same"]);
        for (old, new) in original.items.iter().zip(&retimed.items) {
            assert_eq!(old.kind, new.kind);
            if old.kind == QueueKind::NextRun {
                assert_eq!(old.entry_id, new.entry_id);
            } else {
                assert_ne!(old.entry_id, new.entry_id);
            }
        }
        let mut deliveries = session.subscribe();
        session.prompt("resume").await.unwrap();
        let mut consumption = Vec::new();
        while let Ok(event) = deliveries.events.try_recv() {
            if let AgentSessionEvent::QueueUpdate {
                origin: crate::QueueUpdateOrigin::Mutation,
                items,
                ..
            } = event.event
            {
                consumption.push(items);
            }
        }
        assert_eq!(consumption.len(), 2);
        assert_eq!(
            consumption[0]
                .iter()
                .filter(|item| item.kind == QueueKind::Steer)
                .count(),
            0,
            "steering consumes its own identity even if equal-text follow-up arrived first"
        );
        assert_eq!(
            consumption[0]
                .iter()
                .filter(|item| item.kind == QueueKind::FollowUp)
                .count(),
            1
        );
        assert_eq!(provider.requests().len(), 3);
        let snapshot = session.snapshot();
        assert_eq!(snapshot.queue.follow_up, ["parked explicitly"]);
        assert_eq!(snapshot.queue.items[0].kind, QueueKind::NextRun);
        let users = snapshot
            .agent
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::User(user) => match user.content.first() {
                    Some(ContentBlock::Text(text)) => Some(text.text.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(users, ["first", "resume", "same", "same"]);
        session.shutdown().await;
        drop(session);
        let replay_plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("fresh".into())]);
        let replay_provider = replay_plugin.provider();
        let reopened = AgentSession::open(
            PiRuntime::builder()
                .provider_plugin(replay_plugin)
                .build()
                .unwrap(),
            &path,
            Default::default(),
        )
        .await
        .unwrap();
        reopened.prompt("not a queue admission").await.unwrap();
        assert_eq!(replay_provider.requests().len(), 1);
        assert_eq!(reopened.snapshot().queue.follow_up, ["parked explicitly"]);
        assert_eq!(reopened.snapshot().queue.items[0].kind, QueueKind::NextRun);
    }
}

#[tokio::test]
async fn idle_user_queues_keep_delivery_modes_and_join_the_next_explicit_prompt() {
    use pi_plugin::InputStreamingBehavior;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("idle-queue.jsonl");
    let session = AgentSession::create(
        scripted_runtime([
            ScriptedTurn::Text("answer".into()),
            ScriptedTurn::Text("follow-up answer".into()),
        ]),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe();
    session
        .queue_user_input("steering", InputStreamingBehavior::Steer)
        .await
        .unwrap();
    session
        .queue_user_input("follow-up", InputStreamingBehavior::FollowUp)
        .await
        .unwrap();
    assert_eq!(session.snapshot().queue.steering, ["steering"]);
    assert_eq!(session.snapshot().queue.follow_up, ["follow-up"]);
    assert!(
        session
            .history()
            .load()
            .unwrap()
            .records
            .iter()
            .filter_map(|record| match &record.record {
                LaneRecordEntry::QueueEnqueued { queue, run_id, .. } => Some((*queue, run_id)),
                _ => None,
            })
            .all(|(queue, run_id)| queue == QueueKind::NextRun && run_id.is_none())
    );
    assert!(!path.exists(), "idle queue does not materialize a session");
    session.prompt("start explicitly").await.unwrap();
    assert!(session.snapshot().queue.items.is_empty());
    let user_texts = session
        .snapshot()
        .agent
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => user.content.first().and_then(|block| match block {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_texts, ["start explicitly", "steering", "follow-up"]);
    let mut settled = Vec::new();
    while let Ok(event) = events.events.try_recv() {
        if let AgentSessionEvent::AgentSettled { aborted } = event.event {
            settled.push(aborted);
        }
    }
    assert_eq!(settled, [false]);
    session.shutdown().await;
    drop(session);
    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert!(
        reopened.snapshot().queue.items.is_empty(),
        "consumed idle input must not reappear during recovery"
    );
}

#[tokio::test]
async fn idle_user_queue_clear_and_recovery_keep_v4_next_run_semantics() {
    use pi_plugin::InputStreamingBehavior;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("idle-recovery.jsonl");
    let session = AgentSession::create(
        scripted_runtime([ScriptedTurn::Text("saved".into())]),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    session.prompt("save").await.unwrap();
    session
        .queue_user_input("steering", InputStreamingBehavior::Steer)
        .await
        .unwrap();
    session
        .queue_user_input("follow-up", InputStreamingBehavior::FollowUp)
        .await
        .unwrap();
    let cleared = session.clear_queue().unwrap();
    assert_eq!(cleared.steering, ["steering"]);
    assert_eq!(cleared.follow_up, ["follow-up"]);
    session
        .queue_user_input("recover explicitly", InputStreamingBehavior::Steer)
        .await
        .unwrap();
    session.shutdown().await;
    drop(session);
    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(reopened.snapshot().queue.follow_up, ["recover explicitly"]);
    assert!(
        reopened
            .snapshot()
            .queue
            .items
            .iter()
            .all(|item| item.kind == QueueKind::NextRun)
    );
    assert_eq!(
        reopened
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry, SessionEntry::Message(_)))
            .count(),
        2
    );
}

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
            .all(|item| item.kind == QueueKind::FollowUp),
        "the live queue retains its mode for the next explicit prompt"
    );
    drop(session);
    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(reopened.snapshot().queue.follow_up, ["last", "same"]);
    assert!(
        reopened
            .snapshot()
            .queue
            .items
            .iter()
            .all(|item| item.kind == QueueKind::NextRun)
    );
}
