use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_agent::{AgentLoopTurnUpdate, AgentOptions, FnTurnControl};
use pi_core::{
    ContentBlock, Message, ModelId, PluginId, ProviderId, ResponseMetadata, StopReason,
    StreamEvent, ToolCall, Usage,
};
use pi_runtime::{PiRuntime, SystemPrompt};
use pi_session::{
    AgentSession, AgentSessionOptions, CompactionEntry, CompactionSettings,
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionEntry, SessionLog, SessionPlugin,
    SessionPluginContext, SessionPluginError, SessionPlugins,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn, TestToolsPlugin};
use serde_json::json;

fn text_turn_with_usage(text: &str, input_tokens: u64) -> ScriptedTurn {
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
            delta: text.to_string(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Stop,
            usage: Usage {
                input: input_tokens,
                total_tokens: input_tokens,
                ..Usage::default()
            },
        },
    ])
}

struct DeterministicCompaction;

#[pi_session::session_plugin]
impl SessionPlugin for DeterministicCompaction {
    fn id(&self) -> PluginId {
        PluginId::new("deterministic-compaction")
    }

    async fn session_before_compact(
        &self,
        _context: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, SessionPluginError> {
        Ok(Some(SessionBeforeCompactResult {
            cancel: false,
            compaction: Some(CompactionEntry {
                summary: "compacted history".to_string(),
                retained_tail: Vec::new(),
                tokens_before: event.preparation.tokens_before,
                details: None,
                usage: None,
            }),
        }))
    }
}

#[tokio::test]
async fn threshold_compaction_between_tool_turns_resumes_with_composed_turn_control() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("old answer".to_string()),
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "call-large",
            "echo",
            json!({ "text": "x".repeat(4_000) }),
        )]),
        text_turn_with_usage("finished after compaction", 100),
    ]);
    let provider = provider_plugin.provider();
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let session_path = directory.path().join("session.jsonl");
    let prior_turn_control = Arc::new(FnTurnControl::new().with_prepare_next_turn({
        let prepare_calls = Arc::clone(&prepare_calls);
        let session_path = session_path.clone();
        move |context, _signal| {
            let prepare_calls = Arc::clone(&prepare_calls);
            let session_path = session_path.clone();
            async move {
                prepare_calls.fetch_add(1, Ordering::SeqCst);
                let (_, document) = SessionLog::open(&session_path).unwrap();
                assert!(
                    document
                        .entries
                        .iter()
                        .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
                );
                let mut next = Arc::unwrap_or_clone(context.context);
                next.system_prompt = "composed after compaction".to_string();
                Ok(Some(AgentLoopTurnUpdate {
                    context: Some(next),
                    ..AgentLoopTurnUpdate::default()
                }))
            }
        }
    }));
    let runtime = PiRuntime::builder()
        .agent_plugin(TestToolsPlugin::new())
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["echo".to_string()],
            cwd: directory.path().to_path_buf(),
            turn_control: prior_turn_control,
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Pi(Box::default()))
        .build()
        .unwrap();
    let session = AgentSession::create_with_options(
        runtime,
        &session_path,
        AgentSessionOptions::default()
            .plugins(SessionPlugins::new().plugin(DeterministicCompaction))
            .compaction(CompactionSettings {
                reserve_tokens: 100,
                keep_recent_tokens: 100,
                ..CompactionSettings::default()
            })
            .context_window(1_000),
    )
    .await
    .unwrap();

    session.prompt("seed old history").await.unwrap();
    let outcome = session.prompt("run the large tool").await.unwrap();
    let requests = provider.requests();

    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].system_prompt, "composed after compaction");
    assert!(matches!(
        outcome.final_context.messages.last(),
        Some(Message::Assistant(message))
            if matches!(message.content.first(), Some(ContentBlock::Text(text)) if text.text == "finished after compaction")
    ));
    assert_eq!(
        session.snapshot().agent.messages,
        outcome.final_context.messages
    );
    assert_eq!(
        session
            .log()
            .load()
            .unwrap()
            .entries
            .iter()
            .filter(|record| matches!(record.entry, SessionEntry::Compaction(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_in_run_compaction_is_best_effort_and_does_not_stop_the_agent() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("old answer".to_string()),
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "call-large",
            "echo",
            json!({ "text": "x".repeat(4_000) }),
        )]),
        ScriptedTurn::Error("summary unavailable".to_string()),
        text_turn_with_usage("finished without compaction", 100),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .agent_plugin(TestToolsPlugin::new())
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["echo".to_string()],
            cwd: directory.path().to_path_buf(),
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Pi(Box::default()))
        .build()
        .unwrap();
    let session = AgentSession::create_with_options(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default()
            .compaction(CompactionSettings {
                reserve_tokens: 100,
                keep_recent_tokens: 100,
                ..CompactionSettings::default()
            })
            .context_window(1_000),
    )
    .await
    .unwrap();

    session.prompt("seed old history").await.unwrap();
    let outcome = session.prompt("run the large tool").await.unwrap();
    let requests = provider.requests();

    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[2].system_prompt,
        pi_session::SUMMARIZATION_SYSTEM_PROMPT
    );
    assert_ne!(
        requests[3].system_prompt,
        pi_session::SUMMARIZATION_SYSTEM_PROMPT
    );
    assert!(matches!(
        outcome.final_context.messages.last(),
        Some(Message::Assistant(message))
            if matches!(message.content.first(), Some(ContentBlock::Text(text)) if text.text == "finished without compaction")
    ));
    assert!(
        !session
            .log()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
    );
}

#[tokio::test]
async fn permanent_turn_control_does_not_keep_a_dropped_session_alive() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "call-after-session-drop",
            "echo",
            json!({ "text": "still running" }),
        )]),
        ScriptedTurn::Text("finished through base control".to_string()),
    ]);
    let base_calls = Arc::new(AtomicUsize::new(0));
    let base_control = Arc::new(FnTurnControl::new().with_prepare_next_turn({
        let base_calls = Arc::clone(&base_calls);
        move |_context, _signal| {
            let base_calls = Arc::clone(&base_calls);
            async move {
                base_calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        }
    }));
    let runtime = PiRuntime::builder()
        .agent_plugin(TestToolsPlugin::new())
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["echo".to_string()],
            cwd: directory.path().to_path_buf(),
            turn_control: base_control,
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::Pi(Box::default()))
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, directory.path().join("session-lifetime.jsonl"))
        .await
        .unwrap();
    let weak_session = Arc::downgrade(&session);
    let agent = session.runtime().agent().clone();

    drop(session);

    assert!(weak_session.upgrade().is_none());
    let outcome = agent.prompt("continue without session").await.unwrap();
    assert_eq!(base_calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        outcome.final_context.messages.last(),
        Some(Message::Assistant(message))
            if matches!(message.content.first(), Some(ContentBlock::Text(text)) if text.text == "finished through base control")
    ));
}
