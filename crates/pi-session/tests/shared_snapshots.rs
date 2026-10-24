use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_core::{
    AbortSignal, AgentEvent, CustomMessage, CustomMessageContent, Message, ModelId, PluginId,
    ProviderId, ResponseMetadata, StopReason, StreamEvent, Usage, UserMessage,
};
use pi_plugin::{
    AgentBeforeSettleEvent, AgentPluginContext, AgentSettledEvent, BoundaryResult, Plugin,
    PluginError, SessionBoundaryDraft, TurnEndEvent,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, BranchSummaryEntry, CompactionEntry, CompactionSettings,
    ContextEditEntry, ContextEditReplacement, SessionEntry, SessionHeader, SessionStore,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

#[tokio::test]
async fn session_stream_snapshots_share_history_and_context_edits_preserve_old_views() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let log =
        SessionStore::create(&path, SessionHeader::new("snapshot-test", directory.path())).unwrap();
    let original_id = log
        .append_message(Message::User(UserMessage::text("seed", 0)))
        .unwrap();
    let session_runtime = runtime(directory.path(), [streamed_turn(32)]);
    let agent = session_runtime.agent().clone();
    let options = AgentSessionOptions::default().compaction(CompactionSettings {
        enabled: false,
        ..CompactionSettings::default()
    });
    let session = AgentSession::open(session_runtime, &path, options.clone())
        .await
        .unwrap();
    let mut subscription = session.subscribe();
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    agent.subscribe(Arc::new({
        let session = Arc::downgrade(&session);
        let snapshots = snapshots.clone();
        move |event: AgentEvent, _: AbortSignal| {
            let session = session.clone();
            let snapshots = snapshots.clone();
            async move {
                if matches!(event, AgentEvent::MessageUpdate { .. }) {
                    snapshots
                        .lock()
                        .unwrap()
                        .push(session.upgrade().unwrap().snapshot());
                }
                Ok(())
            }
        }
    }));
    session.prompt("new input").await.unwrap();
    {
        let snapshots = snapshots.lock().unwrap();
        assert!(snapshots.len() >= 32);
        for pair in snapshots.windows(2) {
            assert!(Arc::ptr_eq(
                &pair[0].agent.messages,
                &pair[1].agent.messages
            ));
            assert!(pair[0].revision < pair[1].revision);
        }
    }
    assert_eq!(subscription.snapshot.agent.messages.len(), 1);
    let before_edit = session.snapshot();
    assert_eq!(before_edit.agent.messages.len(), 3);
    session.edit_context(original_id, None).unwrap();
    let edited = session.snapshot();
    assert_eq!(edited.agent.messages.len(), 2);
    assert_eq!(before_edit.agent.messages.len(), 3);
    assert!(!Arc::ptr_eq(
        &before_edit.agent.messages,
        &edited.agent.messages
    ));

    let mut revision = subscription.snapshot.revision;
    while let Ok(event) = subscription.events.try_recv() {
        assert!(event.revision > revision);
        revision = event.revision;
    }
    assert_eq!(revision, edited.revision);
    let reopened = AgentSession::open(runtime(directory.path(), []), path, options)
        .await
        .unwrap();
    assert_eq!(reopened.snapshot().agent.messages, edited.agent.messages);
    assert_eq!(
        SessionStore::read(log.path())
            .unwrap()
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry, SessionEntry::Message(_)))
            .count(),
        3
    );
}

fn runtime(cwd: &std::path::Path, turns: impl IntoIterator<Item = ScriptedTurn>) -> PiRuntime {
    PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted(turns))
        .agent_options(AgentOptions {
            cwd: cwd.to_path_buf(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap()
}

fn streamed_turn(deltas: usize) -> ScriptedTurn {
    let mut events = vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                1,
            ),
        },
        StreamEvent::TextStart { content_index: 0 },
    ];
    events.extend((0..deltas).map(|_| StreamEvent::TextDelta {
        content_index: 0,
        delta: "chunk".into(),
    }));
    events.extend([
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Stop,
            usage: Usage::default(),
        },
    ]);
    ScriptedTurn::Events(events)
}

#[tokio::test]
async fn a_lagged_stream_subscription_recovers_from_the_current_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let session = AgentSession::create(
        runtime(directory.path(), [streamed_turn(2_048)]),
        directory.path().join("lagged.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe();
    session
        .prompt("stream past the receiver capacity")
        .await
        .unwrap();
    assert!(matches!(
        subscription.events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));

    let recovered = session.snapshot();
    assert!(recovered.revision > subscription.snapshot.revision);
    assert!(subscription.snapshot.agent.messages.is_empty());
    assert_eq!(recovered.agent.messages.len(), 2);
    assert_eq!(
        recovered.agent.messages.as_ref(),
        &session.history().context().unwrap().runtime_messages()
    );
    while let Ok(event) = subscription.events.try_recv() {
        assert!(event.revision <= recovered.revision);
    }

    let id = session
        .append_custom_message(CustomMessage {
            custom_type: "after-lag".into(),
            content: CustomMessageContent::Text("fresh context".into()),
            display: false,
            details: None,
            timestamp_ms: 2,
        })
        .unwrap();
    let event = subscription.events.try_recv().unwrap();
    assert!(event.revision > recovered.revision);
    assert!(matches!(
        event.event,
        pi_session::AgentSessionEvent::EntryAppended { entry } if entry.id == id
    ));
    let updated = session.snapshot();
    assert_eq!(updated.revision, event.revision);
    assert_eq!(updated.agent.messages.len(), 3);
    assert_eq!(recovered.agent.messages.len(), 2);
}

#[tokio::test]
async fn navigation_compaction_and_branch_summary_refresh_the_public_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("navigation.jsonl");
    let log =
        SessionStore::create(&path, SessionHeader::new("navigation", directory.path())).unwrap();
    let root = log
        .append_message(Message::User(UserMessage::text("root", 0)))
        .unwrap();
    let tail = Message::User(UserMessage::text("tail", 1));
    let leaf = log.append_message(tail.clone()).unwrap();
    let session = AgentSession::open(runtime(directory.path(), []), path, Default::default())
        .await
        .unwrap();
    let original = session.snapshot();

    let context = session.checkout(Some(&root)).await.unwrap();
    let checked_out = session.subscribe().snapshot;
    assert_eq!(
        checked_out.agent.messages.as_ref(),
        &context.runtime_messages()
    );
    assert_eq!(checked_out.agent.messages.len(), 1);
    assert!(checked_out.revision > original.revision);
    assert_eq!(original.agent.messages.len(), 2);

    session.checkout(Some(&leaf)).await.unwrap();
    session
        .append_compaction(CompactionEntry {
            summary: "compacted root".into(),
            retained_tail: vec![tail.into()],
            retained_entry_ids: Vec::new(),
            tokens_before: 100,
            details: None,
            usage: None,
        })
        .await
        .unwrap();
    let compacted = session.snapshot();
    assert_eq!(
        compacted.agent.messages.as_ref(),
        &session.history().context().unwrap().runtime_messages()
    );
    assert_ne!(compacted.agent.messages, original.agent.messages);

    session
        .branch_with_summary(
            Some(&root),
            BranchSummaryEntry {
                from_id: String::new(),
                summary: "abandoned branch".into(),
                details: None,
                usage: None,
            },
        )
        .await
        .unwrap();
    let branched = session.subscribe().snapshot;
    assert_eq!(
        branched.agent.messages.as_ref(),
        &session.history().context().unwrap().runtime_messages()
    );
    assert_ne!(branched.agent.messages, compacted.agent.messages);
    assert_eq!(original.agent.messages.len(), 2);
    assert_eq!(checked_out.agent.messages.len(), 1);
    assert!(branched.revision > compacted.revision);

    session
        .append_custom_message(CustomMessage {
            custom_type: "notice".into(),
            content: CustomMessageContent::Text("extra context".into()),
            display: true,
            details: None,
            timestamp_ms: 2,
        })
        .unwrap();
    let appended = session.snapshot();
    assert_eq!(
        appended.agent.messages.as_ref(),
        &session.history().context().unwrap().runtime_messages()
    );
    assert_eq!(
        appended.agent.messages.len(),
        branched.agent.messages.len() + 1
    );
    assert!(appended.revision > branched.revision);
}

struct FinalContextEdit {
    target_id: String,
    before_settle: bool,
    settled: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl FinalContextEdit {
    fn proposal(&self) -> BoundaryResult {
        BoundaryResult {
            entries: Some(vec![SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                target_id: self.target_id.clone(),
                replacement: Some(ContextEditReplacement {
                    content: CustomMessageContent::Text("rewritten input".into()),
                }),
            })]),
            continue_run: Some(false),
        }
    }
}

#[pi_plugin::plugin]
impl Plugin for FinalContextEdit {
    fn id(&self) -> PluginId {
        PluginId::new("final-context-edit")
    }

    async fn turn_end(
        &self,
        _: AgentPluginContext,
        _: TurnEndEvent,
    ) -> Result<BoundaryResult, PluginError> {
        Ok(if self.before_settle {
            BoundaryResult::default()
        } else {
            self.proposal()
        })
    }

    async fn agent_before_settle(
        &self,
        _: AgentPluginContext,
        _: AgentBeforeSettleEvent,
    ) -> Result<BoundaryResult, PluginError> {
        Ok(if self.before_settle {
            self.proposal()
        } else {
            BoundaryResult::default()
        })
    }

    async fn agent_settled(
        &self,
        _: AgentPluginContext,
        _: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        self.settled.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn boundary_edits_without_continuation_are_visible_before_settled_callbacks_finish() {
    for before_settle in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boundary.jsonl");
        let log = SessionStore::create(
            &path,
            SessionHeader::new("boundary-snapshot", directory.path()),
        )
        .unwrap();
        let original = Message::User(UserMessage::text("original input", 0));
        let target_id = log.append_message(original.clone()).unwrap();
        let settled = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let provider_plugin =
            ScriptedProviderPlugin::scripted([ScriptedTurn::Text("answer".into())]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(provider_plugin)
            .plugin(FinalContextEdit {
                target_id,
                before_settle,
                settled: settled.clone(),
                release: release.clone(),
            })
            .agent_options(AgentOptions {
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::open(runtime, &path, Default::default())
            .await
            .unwrap();
        let old = session.snapshot();
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.prompt("continue").await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), settled.notified())
            .await
            .unwrap();

        let updated = session.subscribe().snapshot;
        assert!(!session.is_idle());
        assert_eq!(old.agent.messages.as_ref(), std::slice::from_ref(&original));
        assert_eq!(provider.requests()[0].messages[0], original);
        assert_eq!(
            updated.agent.messages.as_ref(),
            &session.history().context().unwrap().runtime_messages()
        );
        assert_eq!(
            updated.agent.messages[0],
            Message::User(UserMessage::text("rewritten input", 0))
        );
        assert!(updated.revision > old.revision);
        assert!(!Arc::ptr_eq(&old.agent.messages, &updated.agent.messages));

        release.notify_one();
        let pi_session::PromptOutcome::Agent(outcome) =
            tokio::time::timeout(std::time::Duration::from_secs(5), run)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        else {
            panic!("expected an agent run");
        };
        assert_eq!(outcome.final_context.messages, *updated.agent.messages);
        assert_eq!(provider.requests().len(), 1);
        assert!(session.is_idle());
    }
}
