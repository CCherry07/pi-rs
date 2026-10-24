use pi_agent::{AgentOptions, FnTurnControl};
use pi_core::{CustomMessageContent, Message, ModelId, PluginId, ProviderId};
use pi_plugin::{
    AgentBeforeSettleEvent, AgentPluginContext, AgentSettledEvent, BoundaryResult, Plugin,
    PluginError, SessionBoundaryDraft, TurnEndEvent,
};
use pi_runtime::{PiRuntime, SystemPrompt};
use pi_session::{
    AgentSession, AgentSessionOptions, CompactionEntry, CompactionSettings, ContextEditEntry,
    ContextEditReplacement, CustomMessageEntry, SessionEntry, SessionStore,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

type Turn = dyn Fn(TurnEndEvent) -> BoundaryResult + Send + Sync;
type Settle = dyn Fn(AgentBeforeSettleEvent) -> BoundaryResult + Send + Sync;
struct BoundaryPlugin {
    id: &'static str,
    turn: Box<Turn>,
    settle: Box<Settle>,
    settled: Arc<AtomicUsize>,
    started: Arc<AtomicUsize>,
}
impl BoundaryPlugin {
    fn turn(
        id: &'static str,
        callback: impl Fn(TurnEndEvent) -> BoundaryResult + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            turn: Box::new(callback),
            settle: Box::new(|_| BoundaryResult::default()),
            settled: Arc::default(),
            started: Arc::default(),
        }
    }
}
#[pi_plugin::plugin]
impl Plugin for BoundaryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(self.id)
    }
    async fn before_agent_start(
        &self,
        _: AgentPluginContext,
        event: pi_plugin::BeforeAgentStartEvent,
    ) -> Result<pi_plugin::BeforeAgentStartPatch, PluginError> {
        let call = self.started.fetch_add(1, Ordering::SeqCst);
        Ok(pi_plugin::BeforeAgentStartPatch {
            system_prompt: Some(format!("{}|prepared-{call}", event.system_prompt)),
            messages: vec![],
        })
    }
    async fn turn_end(
        &self,
        _: AgentPluginContext,
        event: TurnEndEvent,
    ) -> Result<BoundaryResult, PluginError> {
        Ok((self.turn)(event))
    }
    async fn agent_before_settle(
        &self,
        _: AgentPluginContext,
        event: AgentBeforeSettleEvent,
    ) -> Result<BoundaryResult, PluginError> {
        Ok((self.settle)(event))
    }
    async fn agent_settled(
        &self,
        _: AgentPluginContext,
        _: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        self.settled.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn input(text: &str) -> SessionBoundaryDraft {
    SessionBoundaryDraft::CustomMessage(CustomMessageEntry {
        custom_type: "boundary-input".into(),
        content: CustomMessageContent::Text(text.into()),
        display: false,
        details: None,
    })
}
fn replacement(text: &str) -> Option<ContextEditReplacement> {
    Some(ContextEditReplacement {
        content: CustomMessageContent::Text(text.into()),
    })
}
fn result(entries: Vec<SessionBoundaryDraft>, resume: bool) -> BoundaryResult {
    BoundaryResult {
        entries: Some(entries),
        continue_run: Some(resume),
    }
}
fn runtime(
    provider: ScriptedProviderPlugin,
    plugins: Vec<BoundaryPlugin>,
    stop: bool,
) -> PiRuntime {
    let mut builder = PiRuntime::builder()
        .provider_plugin(provider)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            turn_control: if stop {
                Arc::new(
                    FnTurnControl::new().with_should_stop_after_turn(|_, _| async { Ok(true) }),
                )
            } else {
                Arc::new(FnTurnControl::new())
            },
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Final("test".into()));
    for plugin in plugins {
        builder = builder.plugin(plugin);
    }
    builder.build().unwrap()
}
fn options() -> AgentSessionOptions {
    AgentSessionOptions::default().compaction(CompactionSettings {
        enabled: false,
        ..CompactionSettings::default()
    })
}
fn text(messages: &[Message]) -> String {
    serde_json::to_string(messages).unwrap()
}

#[tokio::test]
async fn turn_boundary_commits_edits_before_the_next_request_and_chains_previews() {
    let dir = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("first".into()),
        ScriptedTurn::Text("second".into()),
    ]);
    let provider = provider_plugin.provider();
    let path = dir.path().join("session.jsonl");
    let persisted = path.clone();
    let first = BoundaryPlugin::turn("first", move |event| {
        let (_, document) = SessionStore::open(&persisted).unwrap();
        assert!(
            document
                .entries
                .iter()
                .any(|entry| Some(&entry.id) == event.message_entry_id.as_ref())
        );
        if event.turn_index != 0 {
            return BoundaryResult::default();
        }
        result(
            vec![
                SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                    target_id: event.message_entry_id.unwrap(),
                    replacement: replacement("edited answer"),
                }),
                input("continue work"),
            ],
            true,
        )
    });
    let second = BoundaryPlugin::turn("second", |event| {
        if event.turn_index == 0 {
            let state = event.boundary.unwrap();
            assert_eq!(state.entries.len(), 2);
            assert!(text(&state.context.llm_messages).contains("edited answer"));
            assert!(state.context.can_continue);
            assert!(
                state
                    .context
                    .context_entries
                    .iter()
                    .all(|entry| entry.source_entry.timestamp_ms > 0)
            );
        }
        BoundaryResult::default()
    });
    let session = AgentSession::create(
        runtime(provider_plugin, vec![first, second], false),
        &path,
        options(),
    )
    .await
    .unwrap();
    session.prompt("start").await.unwrap();
    assert_eq!(provider.requests().len(), 2);
    assert!(text(&provider.requests()[1].messages).contains("edited answer"));
    assert!(text(&provider.requests()[1].messages).contains("continue work"));
    let (_, document) = SessionStore::open(&path).unwrap();
    assert!(document.entries.iter().any(|entry| matches!(&entry.entry, SessionEntry::Message(value) if serde_json::to_string(value).unwrap().contains("first"))));
}

#[tokio::test]
async fn before_settle_continues_once_and_settled_fires_after_the_last_run() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut plugin = BoundaryPlugin::turn("settle", |_| BoundaryResult::default());
    let settled = plugin.settled.clone();
    let started = plugin.started.clone();
    let count = calls.clone();
    let slot = Arc::new(std::sync::Mutex::new(None::<std::sync::Weak<AgentSession>>));
    let handle = slot.clone();
    plugin.settle = Box::new(move |event| {
        assert_eq!(
            event.boundary.outcome,
            pi_plugin::AgentActivityOutcome::Completed
        );
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            handle
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap()
                .enqueue_message(
                    Message::User(pi_core::UserMessage::text("before settle steering", 99)),
                    pi_session::QueueKind::Steer,
                )
                .unwrap();
            result(vec![input("verify result")], true)
        } else {
            BoundaryResult::default()
        }
    });
    let p = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Error("connection lost".into()),
        ScriptedTurn::Text("draft".into()),
        ScriptedTurn::Text("verified".into()),
    ]);
    let provider = p.provider();
    let session = AgentSession::create(
        runtime(p, vec![plugin], false),
        dir.path().join("s.jsonl"),
        options().retry(pi_session::AutoRetrySettings {
            enabled: true,
            max_retries: 1,
            base_delay_ms: 0,
        }),
    )
    .await
    .unwrap();
    *slot.lock().unwrap() = Some(Arc::downgrade(&session));
    session.prompt("start").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].system_prompt,
        provider.requests()[1].system_prompt
    );
    assert_eq!(provider.requests().len(), 3);
    assert!(provider.requests()[2].messages.iter().all(|message| !matches!(message, Message::Assistant(value) if value.stop_reason == pi_core::StopReason::Error)));
    assert!(text(&provider.requests()[2].messages).contains("verify result"));
    assert!(text(&provider.requests()[2].messages).contains("before settle steering"));
}

#[tokio::test]
async fn invalid_drafts_are_atomic_and_later_handlers_can_repair_them() {
    for repair in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let first = BoundaryPlugin::turn("invalid", |_| {
            result(
                vec![
                    input("must not leak"),
                    SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                        target_id: "missing".into(),
                        replacement: None,
                    }),
                ],
                true,
            )
        });
        let second = BoundaryPlugin::turn("repair", move |event| {
            assert_eq!(event.boundary.unwrap().entries.len(), 2);
            if repair {
                result(vec![input("repaired")], false)
            } else {
                BoundaryResult::default()
            }
        });
        let p = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("answer".into())]);
        let provider = p.provider();
        let session = AgentSession::create(
            runtime(p, vec![first, second], false),
            dir.path().join("s.jsonl"),
            options(),
        )
        .await
        .unwrap();
        session.prompt("start").await.unwrap();
        assert_eq!(provider.requests().len(), 1);
        let entries = session.history().branch_entries().unwrap();
        let wire = serde_json::to_string(&entries).unwrap();
        assert!(!wire.contains("must not leak"));
        assert_eq!(wire.contains("repaired"), repair);
    }
}

#[tokio::test]
async fn invalid_continuation_and_controller_stop_cannot_spin() {
    for stop in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let plugin = BoundaryPlugin::turn("continue", move |_| {
            result(if stop { vec![input("more")] } else { vec![] }, true)
        });
        let p = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]);
        let provider = p.provider();
        let session = AgentSession::create(
            runtime(p, vec![plugin], stop),
            dir.path().join("s.jsonl"),
            options(),
        )
        .await
        .unwrap();
        session.prompt("start").await.unwrap();
        assert_eq!(provider.requests().len(), 1);
        if !stop {
            assert!(
                session
                    .catalog()
                    .plugin_diagnostics()
                    .iter()
                    .any(|d| d.message.contains("without runnable"))
            );
        }
    }
}

#[tokio::test]
async fn edits_are_branch_local_survive_resume_and_preserve_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let p = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("answer".into())]);
    let session = AgentSession::create(runtime(p, vec![], false), &path, options())
        .await
        .unwrap();
    session.prompt("original").await.unwrap();
    let branch = session.history().branch_entries().unwrap();
    let target = branch
        .iter()
        .find(
            |record| matches!(&record.entry, SessionEntry::Message(m) if m.message.role()=="user"),
        )
        .unwrap()
        .id
        .clone();
    let original_leaf = branch.last().unwrap().id.clone();
    session
        .edit_context(&target, replacement("replacement"))
        .unwrap();
    let edited_leaf = session
        .history()
        .branch_entries()
        .unwrap()
        .last()
        .unwrap()
        .id
        .clone();
    session.edit_context(&target, None).unwrap();
    assert!(!text(&session.snapshot().agent.messages).contains("replacement"));
    session.checkout(Some(&original_leaf)).await.unwrap();
    assert!(text(&session.snapshot().agent.messages).contains("original"));
    session.checkout(Some(&edited_leaf)).await.unwrap();
    assert!(
        text(&session.snapshot().agent.messages).contains("replacement"),
        "snapshot={:?} context={:?}",
        session.snapshot().agent.messages,
        session.history().context().unwrap()
    );
    let (log, _) = SessionStore::open(&path).unwrap();
    assert!(
        serde_json::to_string(&log.context().unwrap().messages)
            .unwrap()
            .contains("replacement")
    );
    assert!(session.edit_context("missing", None).is_err());
    assert!(session.edit_context(&edited_leaf, None).is_err());
}

#[tokio::test]
async fn editing_retained_messages_after_repeated_compaction_keeps_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let p = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("answer".into())]);
    let session = AgentSession::create(
        runtime(p, vec![], false),
        dir.path().join("s.jsonl"),
        options(),
    )
    .await
    .unwrap();
    session.prompt("keep me").await.unwrap();
    let target = session
        .history()
        .branch_entries()
        .unwrap()
        .iter()
        .find(
            |record| matches!(&record.entry, SessionEntry::Message(m) if m.message.role()=="user"),
        )
        .unwrap()
        .id
        .clone();
    for replacement_text in ["first edit", "second edit"] {
        let retained_tail = session
            .history()
            .context()
            .unwrap()
            .messages
            .into_iter()
            .filter(|message| message.role() == "user")
            .collect();
        session
            .append_compaction(CompactionEntry {
                summary: "checkpoint".into(),
                retained_tail,
                retained_entry_ids: vec![],
                tokens_before: 10,
                details: Some(json!("opaque plugin metadata")),
                usage: None,
            })
            .await
            .unwrap();
        session
            .edit_context(&target, replacement(replacement_text))
            .unwrap();
        assert!(
            text(&session.snapshot().agent.messages).contains(replacement_text),
            "expected={replacement_text} snapshot={:?} branch={:?}",
            session.snapshot().agent.messages,
            session.history().branch_entries().unwrap()
        );
    }
    let (_, document) = SessionStore::open(session.history().path()).unwrap();
    let context = pi_session::build_session_context(
        &document
            .branch()
            .unwrap()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        &Default::default(),
    );
    assert!(
        serde_json::to_string(&context.messages)
            .unwrap()
            .contains("second edit")
    );
}

struct BlockingBoundary(Arc<tokio::sync::Notify>);
#[pi_plugin::plugin]
impl Plugin for BlockingBoundary {
    fn id(&self) -> PluginId {
        PluginId::new("blocking-boundary")
    }
    async fn agent_before_settle(
        &self,
        context: AgentPluginContext,
        _: AgentBeforeSettleEvent,
    ) -> Result<BoundaryResult, PluginError> {
        self.0.notify_one();
        context.signal().wait().await;
        Ok(result(vec![input("cancelled continuation")], true))
    }
}
#[tokio::test]
async fn abort_during_before_settle_signals_the_hook_and_prevents_another_request() {
    let dir = tempfile::tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let p = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]);
    let provider = p.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(p)
        .plugin(BlockingBoundary(started.clone()))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Final("test".into()))
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, dir.path().join("s.jsonl"), options())
        .await
        .unwrap();
    let run_session = session.clone();
    let run = tokio::spawn(async move { run_session.prompt("start").await });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    session.abort();
    let pi_session::PromptOutcome::Agent(outcome) =
        tokio::time::timeout(std::time::Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    else {
        panic!("expected an agent run");
    };
    assert_eq!(outcome.stop, pi_agent::AgentLoopStop::Aborted);
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn explicit_boundary_continuation_delivers_steering_before_deferred_follow_up() {
    let dir = tempfile::tempdir().unwrap();
    let slot = Arc::new(std::sync::Mutex::new(None::<std::sync::Weak<AgentSession>>));
    let handle = slot.clone();
    let plugin = BoundaryPlugin::turn("queue", move |event| {
        if event.turn_index != 0 {
            return BoundaryResult::default();
        }
        let session = handle.lock().unwrap().as_ref().unwrap().upgrade().unwrap();
        session
            .enqueue_message(
                Message::User(pi_core::UserMessage::text("steering input", 10)),
                pi_session::QueueKind::Steer,
            )
            .unwrap();
        session
            .enqueue_message(
                Message::User(pi_core::UserMessage::text("follow-up input", 11)),
                pi_session::QueueKind::FollowUp,
            )
            .unwrap();
        result(vec![input("boundary input")], true)
    });
    let p = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("first".into()),
        ScriptedTurn::Text("second".into()),
        ScriptedTurn::Text("third".into()),
    ]);
    let provider = p.provider();
    let session = AgentSession::create(
        runtime(p, vec![plugin], false),
        dir.path().join("s.jsonl"),
        options(),
    )
    .await
    .unwrap();
    *slot.lock().unwrap() = Some(Arc::downgrade(&session));
    session.prompt("start").await.unwrap();
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(text(&requests[1].messages).contains("steering input"));
    assert!(text(&requests[1].messages).contains("boundary input"));
    assert!(!text(&requests[1].messages).contains("follow-up input"));
    assert!(text(&requests[2].messages).contains("follow-up input"));
}

#[test]
fn content_edits_preserve_wire_metadata_and_reject_cross_branch_batches() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionStore::create(
        dir.path().join("s.jsonl"),
        pi_session::SessionHeader::new("s", dir.path()),
    )
    .unwrap();
    let original = pi_session::AgentMessage::custom(json!({"role":"user","content":"original","timestamp":1,"signature":"signed","extra":{"version":9}})).unwrap();
    let target = log
        .append_session_record(SessionEntry::Message(pi_session::MessageEntry {
            message: original.clone(),
            terminate: false,
        }))
        .unwrap();
    let edit = ContextEditEntry {
        target_id: target.id.clone(),
        replacement: replacement("changed"),
    };
    let projected = serde_json::to_value(edit.apply(&original).unwrap()).unwrap();
    assert_eq!(projected["signature"], "signed");
    assert_eq!(projected["extra"], json!({"version":9}));
    log.move_lane(pi_session::MAIN_LANE, None).unwrap();
    let before = log.load().unwrap().entries.len();
    assert!(
        log.append_batch([
            SessionEntry::Custom(pi_session::CustomEntry {
                custom_type: "no-partial-write".into(),
                data: None
            }),
            SessionEntry::ContextEdit(edit)
        ])
        .is_err()
    );
    assert_eq!(log.load().unwrap().entries.len(), before);
    assert!(serde_json::from_value::<ContextEditEntry>(json!({"targetId":"a"})).is_err());
}

#[tokio::test]
async fn tool_results_are_durable_and_editable_before_the_next_provider_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tools.jsonl");
    let durable_path = path.clone();
    let plugin = BoundaryPlugin::turn("edit-tool", move |event| {
        if event.turn_index != 0 {
            return BoundaryResult::default();
        }
        assert_eq!(event.tool_result_entry_ids.len(), 1);
        let (log, _) = SessionStore::open(&durable_path).unwrap();
        assert!(
            log.get_entry(event.message_entry_id.as_ref().unwrap())
                .is_some()
        );
        assert!(log.get_entry(&event.tool_result_entry_ids[0]).is_some());
        result(
            vec![SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                target_id: event.tool_result_entry_ids[0].clone(),
                replacement: replacement("sanitized result"),
            })],
            false,
        )
    });
    let p = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![pi_core::ToolCall::new(
            "call",
            "echo",
            json!({"text":"secret"}),
        )]),
        ScriptedTurn::Text("done".into()),
    ]);
    let provider = p.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(p)
        .plugin(plugin)
        .plugin(pi_test_support::TestToolsPlugin::new())
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["echo".into()],
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Final("test".into()))
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, options())
        .await
        .unwrap();
    session.prompt("use tool").await.unwrap();
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::ToolResult(value) if serde_json::to_string(value).unwrap().contains("sanitized result"))));
    assert!(session.history().branch_entries().unwrap().iter().any(|entry| matches!(&entry.entry, SessionEntry::Message(m) if m.message.role()=="toolResult" && serde_json::to_string(&m.message).unwrap().contains("secret"))));
}

struct BoundaryCompactionProbe {
    edits: bool,
    calls: Arc<AtomicUsize>,
}
#[pi_plugin::plugin]
impl Plugin for BoundaryCompactionProbe {
    fn id(&self) -> PluginId {
        PluginId::new("boundary-compaction")
    }
    async fn turn_end(
        &self,
        _: AgentPluginContext,
        event: TurnEndEvent,
    ) -> Result<BoundaryResult, PluginError> {
        if event.turn_index != 0 {
            return Ok(BoundaryResult::default());
        }
        Ok(if self.edits {
            result(
                vec![SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                    target_id: event.message_entry_id.unwrap(),
                    replacement: replacement("small answer"),
                })],
                false,
            )
        } else {
            result(
                vec![
                    input(&"unsent boundary input ".repeat(1000)),
                    SessionBoundaryDraft::Custom(pi_session::CustomEntry {
                        custom_type: "metadata after input".into(),
                        data: None,
                    }),
                ],
                true,
            )
        })
    }
    async fn session_before_compact(
        &self,
        _: &pi_plugin::SessionPluginContext,
        event: &pi_plugin::SessionBeforeCompactEvent,
    ) -> Result<Option<pi_plugin::SessionBeforeCompactResult>, PluginError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            !serde_json::to_string(&event.preparation.messages_to_summarize)
                .unwrap()
                .contains("unsent boundary input")
        );
        Ok(Some(pi_plugin::SessionBeforeCompactResult {
            cancel: false,
            compaction: Some(CompactionEntry {
                summary: "summary".into(),
                retained_tail: vec![],
                retained_entry_ids: vec![],
                tokens_before: 1,
                details: None,
                usage: None,
            }),
        }))
    }
}
fn large_usage_turn() -> ScriptedTurn {
    use pi_core::{ResponseMetadata, StopReason, StreamEvent, Usage};
    ScriptedTurn::Events(vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                i64::MAX,
            ),
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "first response".into(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Stop,
            usage: Usage {
                input: 10_000,
                total_tokens: 10_000,
                ..Default::default()
            },
        },
    ])
}
#[tokio::test]
async fn edits_invalidate_stale_usage_and_unsent_boundary_input_survives_compaction() {
    for edits in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let p = ScriptedProviderPlugin::scripted([
            large_usage_turn(),
            ScriptedTurn::Text("done".into()),
        ]);
        let provider = p.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(p)
            .plugin(BoundaryCompactionProbe {
                edits,
                calls: calls.clone(),
            })
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..Default::default()
            })
            .system_prompt(SystemPrompt::Final("test".into()))
            .build()
            .unwrap();
        let session = AgentSession::create(
            runtime,
            dir.path().join("s.jsonl"),
            AgentSessionOptions::default()
                .context_window(1000)
                .compaction(CompactionSettings {
                    enabled: true,
                    reserve_tokens: 100,
                    keep_recent_tokens: 100,
                }),
        )
        .await
        .unwrap();
        session.prompt("start").await.unwrap();
        if edits {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(provider.requests().len(), 1);
        } else {
            assert!(calls.load(Ordering::SeqCst) > 0);
            assert_eq!(provider.requests().len(), 2);
            assert!(
                text(&provider.requests()[1].messages)
                    .contains(&"unsent boundary input ".repeat(1000))
            );
        }
    }
}

#[tokio::test]
async fn boundary_compaction_previews_and_commits_retained_edits_or_summary_only() {
    for retain in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let plugin = BoundaryPlugin::turn("compact-boundary", move |event| {
            if event.turn_index != 0 {
                return BoundaryResult::default();
            }
            let user = event
                .boundary
                .unwrap()
                .context
                .context_entries
                .iter()
                .find(|entry| {
                    entry
                        .messages
                        .iter()
                        .any(|message| message.role() == "user")
                })
                .unwrap()
                .source_entry
                .id
                .clone();
            let mut entries = vec![
                SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                    target_id: user.clone(),
                    replacement: replacement("edited retained input"),
                }),
                SessionBoundaryDraft::Compaction(pi_plugin::BoundaryCompaction {
                    summary: "boundary checkpoint".into(),
                    first_kept_entry_id: retain.then_some(user),
                    details: None,
                    usage: None,
                }),
            ];
            if retain {
                entries.push(SessionBoundaryDraft::ContextEdit(ContextEditEntry {
                    target_id: event.message_entry_id.unwrap(),
                    replacement: None,
                }));
            }
            result(entries, true)
        });
        let p = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("omit first answer".into()),
            ScriptedTurn::Text("done".into()),
        ]);
        let provider = p.provider();
        let session = AgentSession::create(
            runtime(p, vec![plugin], false),
            dir.path().join("s.jsonl"),
            options(),
        )
        .await
        .unwrap();
        session.prompt("original input").await.unwrap();
        assert_eq!(provider.requests().len(), 2);
        let context = text(&provider.requests()[1].messages);
        assert!(context.contains("boundary checkpoint"));
        assert!(!context.contains("omit first answer"));
        assert_eq!(context.contains("edited retained input"), retain);
    }
}
