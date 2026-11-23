//! Current Pi automatic-compaction behavior at the durable Session boundary.
use std::sync::{Arc, Mutex};

use pi_core::{Message, ModelSpec, PluginId, StopReason, StreamEvent, Usage};
use pi_plugin::Plugin;
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, AutoRetrySettings, CompactionEntry, CompactionReason,
    CompactionSettings, PluginError, SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionEntry, SessionPluginContext,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};

struct Catalog;

#[pi_plugin::provider_plugin]
impl pi_plugin::ProviderPlugin for Catalog {
    fn id(&self) -> PluginId {
        PluginId::new("recovery-catalog")
    }
    fn register(
        &self,
        context: &mut pi_plugin::ProviderRegisterContext<'_>,
    ) -> pi_plugin::Result<()> {
        let mut model = ModelSpec::new("scripted", "test", "Test", "scripted");
        model.context_window = 100_000;
        model.max_tokens = 100;
        context.register_model(model)
    }
}

struct SummaryHook {
    reasons: Arc<Mutex<Vec<(CompactionReason, bool)>>>,
    cancel: bool,
}

#[pi_plugin::plugin]
impl Plugin for SummaryHook {
    fn id(&self) -> PluginId {
        PluginId::new("recovery-summary")
    }

    async fn input(
        &self,
        _: pi_plugin::InputContext,
        event: pi_plugin::InputEvent,
    ) -> Result<pi_plugin::InputPatch, PluginError> {
        Ok(if event.text == "handled input" {
            pi_plugin::InputPatch::Handled
        } else {
            pi_plugin::InputPatch::Continue
        })
    }

    async fn session_before_compact(
        &self,
        _: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, PluginError> {
        self.reasons
            .lock()
            .unwrap()
            .push((event.reason, event.will_retry));
        Ok(Some(SessionBeforeCompactResult {
            cancel: self.cancel,
            compaction: Some(CompactionEntry {
                first_kept_entry_id: None,
                from_hook: None,
                system_message: None,
                retained_entry_ids: Vec::new(),
                summary: "prior work summary".into(),
                retained_tail: event.preparation.retained_tail.clone(),
                tokens_before: event.preparation.tokens_before,
                details: None,
                usage: None,
            }),
        }))
    }
}

struct Harness {
    directory: tempfile::TempDir,
    session: Arc<AgentSession>,
    provider: Arc<ScriptedProvider>,
    reasons: Arc<Mutex<Vec<(CompactionReason, bool)>>>,
}

impl Harness {
    async fn new(turns: impl IntoIterator<Item = ScriptedTurn>, cancel: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let plugin = ScriptedProviderPlugin::scripted(turns);
        let provider = plugin.provider();
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let runtime = PiRuntime::builder()
            .provider_plugin(plugin)
            .provider_plugin(Catalog)
            .plugin(pi_test_support::TestToolsPlugin::new())
            .plugin(SummaryHook {
                reasons: reasons.clone(),
                cancel,
            })
            .build()
            .unwrap();
        let session = AgentSession::create(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default()
                .context_window(100_000)
                .compaction(CompactionSettings {
                    reserve_tokens: 100,
                    keep_recent_tokens: 1,
                    ..CompactionSettings::default()
                })
                .retry(AutoRetrySettings {
                    enabled: false,
                    ..AutoRetrySettings::default()
                }),
        )
        .await
        .unwrap();
        Self {
            directory,
            session,
            provider,
            reasons,
        }
    }
}

fn response(reason: StopReason, usage: Usage) -> ScriptedTurn {
    ScriptedTurn::Events(vec![
        StreamEvent::Start {
            metadata: pi_core::ResponseMetadata::new(
                pi_core::ProviderId::new("scripted"),
                pi_core::ModelId::new("test"),
                "scripted",
                i64::MAX,
            ),
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "response".into(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done { reason, usage },
    ])
}

#[derive(Default)]
struct Events(Mutex<Vec<pi_session::AgentSessionEvent>>);
impl pi_session::SessionEventObserver for Events {
    fn on_event(&self, event: &pi_session::RevisionedAgentSessionEvent) -> Result<(), String> {
        self.0.lock().unwrap().push(event.event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn early_length_stops_recover_once_even_far_below_the_context_threshold() {
    let harness = Harness::new(
        [
            ScriptedTurn::Text("previous response".into()),
            response(
                StopReason::Length,
                Usage {
                    input: 20,
                    output: 16,
                    ..Usage::default()
                },
            ),
            response(
                StopReason::Length,
                Usage {
                    input: 20,
                    output: 17,
                    ..Usage::default()
                },
            ),
        ],
        false,
    )
    .await;
    let events = Arc::new(Events::default());
    let _subscription = harness.session.observe_events(events.clone());
    harness.session.prompt("seed history").await.unwrap();
    harness.session.prompt("finish work").await.unwrap();
    assert_eq!(harness.provider.requests().len(), 3);
    assert_eq!(
        *harness.reasons.lock().unwrap(),
        [(CompactionReason::Overflow, true)]
    );
    assert!(events.0.lock().unwrap().iter().any(|event| matches!(event,
        pi_session::AgentSessionEvent::CompactionEnd { error_message: Some(message), will_retry: false, .. }
            if message == "Truncated response recovery failed after one compact-and-retry attempt.")));
}

#[tokio::test]
async fn pre_prompt_check_does_not_reset_the_previous_recovery_budget() {
    let harness = Harness::new(
        [
            ScriptedTurn::Text("previous response".into()),
            response(
                StopReason::Length,
                Usage {
                    input: 20,
                    output: 16,
                    ..Usage::default()
                },
            ),
            response(
                StopReason::Length,
                Usage {
                    input: 20,
                    output: 17,
                    ..Usage::default()
                },
            ),
            ScriptedTurn::Text("new answer".into()),
        ],
        false,
    )
    .await;
    harness.session.prompt("seed history").await.unwrap();
    harness.session.prompt("finish work").await.unwrap();
    harness.session.prompt("new explicit prompt").await.unwrap();
    assert_eq!(harness.provider.requests().len(), 4);
    assert_eq!(
        *harness.reasons.lock().unwrap(),
        [(CompactionReason::Overflow, true)]
    );
}

#[tokio::test]
async fn completed_assistant_tool_turn_resets_the_recovery_budget() {
    let harness = Harness::new(
        [
            ScriptedTurn::Text("previous response".into()),
            response(
                StopReason::Length,
                Usage {
                    input: 20,
                    output: 16,
                    ..Usage::default()
                },
            ),
            ScriptedTurn::ToolCalls(vec![pi_core::ToolCall::new(
                "call",
                "echo",
                serde_json::json!({"text":"done"}),
            )]),
            response(
                StopReason::Length,
                Usage {
                    input: 20,
                    output: 17,
                    ..Usage::default()
                },
            ),
        ],
        false,
    )
    .await;
    harness.session.prompt("seed history").await.unwrap();
    harness.session.prompt("finish work").await.unwrap();
    // This tool-only turn has no further compactable prefix, so no second summary
    // or retry is possible. Omitting its failure still proves recovery was allowed
    // again rather than rejected as a repeated attempt from the original turn.
    assert_eq!(harness.provider.requests().len(), 4);
    let omissions = harness.session.history().load().unwrap().entries.iter().filter(|record|
        matches!(&record.entry, SessionEntry::ContextEdit(edit) if edit.replacement.is_none())).count();
    assert_eq!(omissions, 2);
    assert_eq!(
        *harness.reasons.lock().unwrap(),
        [(CompactionReason::Overflow, true)]
    );
}

#[tokio::test]
async fn length_stop_at_the_desired_model_limit_does_not_recover() {
    let harness = Harness::new(
        [response(
            StopReason::Length,
            Usage {
                input: 20,
                output: 100,
                ..Usage::default()
            },
        )],
        false,
    )
    .await;
    harness.session.prompt("work").await.unwrap();
    assert_eq!(harness.provider.requests().len(), 1);
    assert!(harness.reasons.lock().unwrap().is_empty());
}

#[tokio::test]
async fn threshold_uses_valid_last_response_usage_instead_of_unsent_custom_input() {
    let harness = Harness::new(
        [
            response(
                StopReason::Stop,
                Usage {
                    input: 10,
                    total_tokens: 10,
                    ..Usage::default()
                },
            ),
            response(
                StopReason::Stop,
                Usage {
                    input: 20,
                    total_tokens: 20,
                    ..Usage::default()
                },
            ),
        ],
        false,
    )
    .await;
    harness.session.prompt("seed history").await.unwrap();
    harness
        .session
        .append_custom_message(pi_core::CustomMessage {
            custom_type: "unsent-context".into(),
            content: pi_core::CustomMessageContent::Text("x".repeat(500_000)),
            display: false,
            details: None,
            timestamp_ms: 10,
        })
        .unwrap();
    harness.session.prompt("new work").await.unwrap();
    assert!(harness.reasons.lock().unwrap().is_empty());
    assert_eq!(harness.provider.requests().len(), 2);
}

#[tokio::test]
async fn successful_overflow_compacts_without_retrying_the_completed_response() {
    let harness = Harness::new(
        [response(
            StopReason::Stop,
            Usage {
                input: 100_001,
                output: 1,
                ..Usage::default()
            },
        )],
        false,
    )
    .await;
    harness.session.prompt("work").await.unwrap();
    assert_eq!(harness.provider.requests().len(), 1);
    assert_eq!(
        *harness.reasons.lock().unwrap(),
        [(CompactionReason::Overflow, false)]
    );
    assert!(
        !harness
            .session
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::ContextEdit(_)))
    );
}

#[tokio::test]
async fn handled_input_does_not_start_pre_prompt_compaction() {
    let harness = Harness::new(
        [response(
            StopReason::Stop,
            Usage {
                input: 100_001,
                ..Default::default()
            },
        )],
        false,
    )
    .await;
    harness.session.set_auto_compaction_enabled(false);
    harness.session.prompt("work").await.unwrap();
    harness.session.set_auto_compaction_enabled(true);
    assert!(matches!(
        harness.session.prompt("handled input").await.unwrap(),
        pi_session::PromptOutcome::Handled
    ));
    assert_eq!(harness.provider.requests().len(), 1);
    assert!(harness.reasons.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pre_prompt_recovery_does_not_continue_the_old_turn() {
    let harness = Harness::new(
        [
            ScriptedTurn::Text("previous response".into()),
            ScriptedTurn::Error("413".into()),
            ScriptedTurn::Text("answer to the new prompt".into()),
        ],
        false,
    )
    .await;
    harness.session.set_auto_compaction_enabled(false);
    harness.session.prompt("seed history").await.unwrap();
    harness.session.prompt("failed request").await.unwrap();
    harness.session.set_auto_compaction_enabled(true);
    harness.session.prompt("the new prompt").await.unwrap();
    assert_eq!(harness.provider.requests().len(), 3);
    assert_eq!(
        *harness.reasons.lock().unwrap(),
        [(CompactionReason::Overflow, true)]
    );
    assert!(
        matches!(harness.provider.requests()[2].messages.last(), Some(Message::User(message))
        if message.content.iter().any(|block| matches!(block, pi_core::ContentBlock::Text(text) if text.text == "the new prompt")))
    );
}

struct AbortAtStart(std::sync::Weak<AgentSession>);
impl pi_session::SessionEventObserver for AbortAtStart {
    fn on_event(&self, event: &pi_session::RevisionedAgentSessionEvent) -> Result<(), String> {
        if matches!(
            event.event,
            pi_session::AgentSessionEvent::CompactionStart { .. }
        ) && let Some(session) = self.0.upgrade()
        {
            session.abort_compaction();
        }
        Ok(())
    }
}

#[tokio::test]
async fn cancellation_from_compaction_start_prevents_hooks_and_summary_requests() {
    let harness = Harness::new([ScriptedTurn::Text("previous response".into())], false).await;
    harness.session.prompt("seed history").await.unwrap();
    let _observer = harness
        .session
        .observe_events(Arc::new(AbortAtStart(Arc::downgrade(&harness.session))));
    assert!(matches!(
        harness.session.compact(None).await,
        Err(pi_session::SessionError::Cancelled(_))
    ));
    assert!(harness.reasons.lock().unwrap().is_empty());
    assert_eq!(harness.provider.requests().len(), 1);
    assert!(harness.session.is_idle());
}

struct AbortOnAssistantEnd(std::sync::Weak<AgentSession>);
impl pi_session::SessionEventObserver for AbortOnAssistantEnd {
    fn on_event(&self, event: &pi_session::RevisionedAgentSessionEvent) -> Result<(), String> {
        if matches!(&event.event, pi_session::AgentSessionEvent::Agent(event)
            if matches!(event.as_ref(), pi_core::AgentEvent::MessageEnd { message: Message::Assistant(_) }))
            && let Some(session) = self.0.upgrade()
        {
            session.abort();
        }
        Ok(())
    }
}

#[tokio::test]
async fn abort_at_message_end_never_starts_post_run_compaction() {
    let harness = Harness::new(
        [response(
            StopReason::Stop,
            Usage {
                input: 100_001,
                ..Default::default()
            },
        )],
        false,
    )
    .await;
    let _observer = harness
        .session
        .observe_events(Arc::new(AbortOnAssistantEnd(Arc::downgrade(
            &harness.session,
        ))));
    harness.session.prompt("work").await.unwrap();
    assert!(harness.reasons.lock().unwrap().is_empty());
    assert_eq!(harness.provider.requests().len(), 1);
    assert!(harness.session.is_idle());
}

struct PausedSummary {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    cancel: bool,
}

#[pi_plugin::plugin]
impl Plugin for PausedSummary {
    fn id(&self) -> PluginId {
        PluginId::new("paused-summary")
    }
    async fn session_before_compact(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, PluginError> {
        self.entered.notify_one();
        self.release.notified().await;
        SummaryHook {
            reasons: Arc::default(),
            cancel: self.cancel,
        }
        .session_before_compact(context, event)
        .await
    }
}

#[tokio::test]
async fn steering_during_successful_or_cancelled_compaction_is_delivered_once() {
    for cancel in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("previous answer".into()),
            ScriptedTurn::Error("413".into()),
            ScriptedTurn::Text("answer including steering".into()),
        ]);
        let provider = plugin.provider();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let session = AgentSession::create(
            PiRuntime::builder()
                .provider_plugin(plugin)
                .plugin(PausedSummary {
                    entered: entered.clone(),
                    release: release.clone(),
                    cancel,
                })
                .build()
                .unwrap(),
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default().compaction(CompactionSettings {
                keep_recent_tokens: 1,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        session.prompt("seed history").await.unwrap();
        let running = session.clone();
        let run = tokio::spawn(async move { running.prompt("finish work").await });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        assert!(session.prompt("must not be admitted").await.is_err());
        assert!(matches!(
            session.steer("new direction").await.unwrap(),
            pi_session::PromptOutcome::Queued { .. }
        ));
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let requests = provider.requests();
        assert_eq!(requests.len(), 3, "cancel={cancel}");
        let delivered = requests[2]
            .messages
            .iter()
            .filter(|message| {
                matches!(message,
            Message::User(message) if message.content.iter().any(|block|
                matches!(block, pi_core::ContentBlock::Text(text) if text.text == "new direction")))
            })
            .count();
        assert_eq!(delivered, 1, "cancel={cancel}");
        assert!(session.is_idle());
    }
}

#[tokio::test]
async fn recovery_budget_resets_when_a_queued_user_message_starts() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("previous answer".into()),
        response(
            StopReason::Length,
            Usage {
                input: 20,
                output: 16,
                ..Usage::default()
            },
        ),
        response(
            StopReason::Length,
            Usage {
                input: 20,
                output: 17,
                ..Usage::default()
            },
        ),
        ScriptedTurn::Text("recovered new direction".into()),
    ]);
    let provider = plugin.provider();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let session = AgentSession::create(
        PiRuntime::builder()
            .provider_plugin(plugin)
            .provider_plugin(Catalog)
            .plugin(PausedSummary {
                entered: entered.clone(),
                release: release.clone(),
                cancel: false,
            })
            .build()
            .unwrap(),
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    session.prompt("seed history").await.unwrap();
    let running = session.clone();
    let run = tokio::spawn(async move { running.prompt("finish work").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    session.steer("new direction").await.unwrap();
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .expect("the new user message must have its own compact-and-retry budget");
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(provider.requests().len(), 4);
    assert!(session.is_idle());
}

#[tokio::test]
async fn prompt_too_long_recovers_without_requiring_the_word_context() {
    let harness = Harness::new(
        [
            ScriptedTurn::Text("previous response".into()),
            ScriptedTurn::Error("prompt is too long: 213462 tokens > 200000 maximum".into()),
            ScriptedTurn::Text("recovered answer".into()),
        ],
        false,
    )
    .await;
    harness.session.prompt("seed history").await.unwrap();
    harness.session.prompt("finish work").await.unwrap();
    assert_eq!(harness.provider.requests().len(), 3);
    assert_eq!(
        *harness.reasons.lock().unwrap(),
        [(CompactionReason::Overflow, true)]
    );
}

#[tokio::test]
async fn resumed_recovery_omits_the_failed_tool_call_and_its_result_before_the_new_prompt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let store = pi_session::SessionStore::create(
        &path,
        pi_session::SessionHeader::new("resumed-recovery", directory.path()),
    )
    .unwrap();
    store
        .append_message(Message::User(pi_core::UserMessage::text(
            "unfinished work",
            1,
        )))
        .unwrap();
    let failed: Message = serde_json::from_value(serde_json::json!({
        "role": "assistant", "api": "scripted", "provider": "scripted", "model": "test",
        "content": [{"type":"toolCall", "id":"failed-call", "name":"echo", "arguments":{}}],
        "usage": Usage::default(), "stopReason":"error", "errorMessage":"prompt is too long", "timestamp":2,
    })).unwrap();
    let failed_id = store.append_message(failed).unwrap();
    let result: Message = serde_json::from_value(serde_json::json!({
        "role":"toolResult", "toolCallId":"failed-call", "toolName":"echo",
        "content":[{"type":"text", "text":"partial tool result"}], "isError":false, "timestamp":3,
    }))
    .unwrap();
    let result_id = store.append_message(result).unwrap();
    let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("new answer".into())]);
    let provider = plugin.provider();
    let session = AgentSession::open(
        PiRuntime::builder()
            .provider_plugin(plugin)
            .provider_plugin(Catalog)
            .agent_options(pi_agent::AgentOptions {
                cwd: directory.path().to_path_buf(),
                ..Default::default()
            })
            .plugin(SummaryHook {
                reasons: Arc::default(),
                cancel: true,
            })
            .build()
            .unwrap(),
        &path,
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        }),
    )
    .await
    .unwrap();
    session.prompt("new explicit request").await.unwrap();
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        1,
        "pre-prompt recovery must not continue the old turn"
    );
    assert!(!requests[0].messages.iter().any(|message| matches!(message,
        Message::ToolResult(result) if result.tool_call_id.as_str() == "failed-call")));
    let omitted = session
        .history()
        .load()
        .unwrap()
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::ContextEdit(edit) if edit.replacement.is_none() => {
                Some(edit.target_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(omitted, [failed_id, result_id]);
}

#[tokio::test]
async fn failed_summary_does_not_resurrect_the_recovery_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("previous answer".into()),
        ScriptedTurn::Error("413".into()),
        ScriptedTurn::Error("summary generator unavailable".into()),
    ]);
    let provider = plugin.provider();
    let session = AgentSession::create(
        PiRuntime::builder()
            .provider_plugin(plugin)
            .build()
            .unwrap(),
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default()
            .compaction(CompactionSettings {
                keep_recent_tokens: 1,
                ..CompactionSettings::default()
            })
            .retry(AutoRetrySettings {
                enabled: false,
                ..AutoRetrySettings::default()
            }),
    )
    .await
    .unwrap();
    session.prompt("seed history").await.unwrap();
    session.prompt("finish work").await.unwrap();
    assert_eq!(provider.requests().len(), 3);
    assert!(
        !session
            .snapshot()
            .agent
            .messages
            .iter()
            .any(|message| matches!(message,
        Message::Assistant(message) if message.stop_reason == StopReason::Error))
    );
    assert!(
        !session
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
    );
    assert!(
        session
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::ContextEdit(_)))
    );
}

#[tokio::test]
async fn cancelled_recovery_omission_is_durable_even_without_a_summary() {
    let harness = Harness::new(
        [
            ScriptedTurn::Text("previous response".into()),
            ScriptedTurn::Error("context length exceeded".into()),
        ],
        true,
    )
    .await;
    harness.session.prompt("seed history").await.unwrap();
    harness.session.prompt("finish work").await.unwrap();

    let raw = harness.session.history().load().unwrap();
    assert!(raw.entries.iter().any(|record| matches!(&record.entry,
        SessionEntry::Message(entry) if matches!(entry.message.as_standard(),
            Some(Message::Assistant(message)) if message.stop_reason == StopReason::Error))));
    assert!(raw.entries.iter().any(|record| matches!(&record.entry,
        SessionEntry::ContextEdit(edit) if edit.replacement.is_none())));
    assert!(
        !raw.entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
    );
    assert!(
        !harness
            .session
            .snapshot()
            .agent
            .messages
            .iter()
            .any(|message| matches!(message,
        Message::Assistant(message) if message.stop_reason == StopReason::Error))
    );
    harness.session.shutdown().await;
    let reopened = AgentSession::open(
        PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .build()
            .unwrap(),
        harness.directory.path().join("session.jsonl"),
        AgentSessionOptions::default(),
    )
    .await
    .unwrap();
    assert!(
        !reopened
            .snapshot()
            .agent
            .messages
            .iter()
            .any(|message| matches!(message,
        Message::Assistant(message) if message.stop_reason == StopReason::Error))
    );
}
