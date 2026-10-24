use crate::SessionStore;
use async_trait::async_trait;
use pi_core::{CustomMessageInput, ModelSelection};
use pi_plugin::Plugin;
use pi_plugin::{IsolatedSessionOptions, IsolatedSessionRequest};
use std::sync::atomic::AtomicUsize;

use pi_agent::AgentOptions;
use pi_core::{
    ContentBlock, CustomMessageContent, Message, ModelId, ProviderId, ResponseMetadata, StopReason,
    StreamEvent, Usage,
};
use pi_runtime::PiRuntime;
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

use super::*;
use crate::{
    AgentSessionOptions, PluginError, SessionPluginContext, SessionShutdownEvent, SessionStartEvent,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementPause {
    Prepare,
    Shutdown,
    Start,
}

#[derive(Clone, Default)]
struct ReplacementGates {
    pause: Arc<Mutex<Option<ReplacementPause>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    registered: Arc<tokio::sync::Notify>,
    activation_committed: Arc<AtomicBool>,
    shutdowns: Arc<AtomicUsize>,
}

impl ReplacementGates {
    fn pause_at(&self, phase: ReplacementPause) {
        *self
            .pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(phase);
    }

    async fn wait_at(&self, phase: ReplacementPause) {
        let paused = *self
            .pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if paused == Some(phase) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    fn resume(&self) {
        *self
            .pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.release.notify_one();
    }
}

struct GatedLifecyclePlugin(ReplacementGates);

struct GatedActivation(Arc<AtomicBool>);

impl SessionGenerationActivation for GatedActivation {
    fn commit(self: Box<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

#[pi_plugin::plugin]
impl Plugin for GatedLifecyclePlugin {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("replacement-gate")
    }

    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        self.0.wait_at(ReplacementPause::Start).await;
        Ok(())
    }

    async fn session_shutdown(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        self.0.shutdowns.fetch_add(1, Ordering::AcqRel);
        self.0.wait_at(ReplacementPause::Shutdown).await;
        Ok(())
    }
}

#[derive(Clone)]
struct GatedGenerationFactory(ReplacementGates);

#[async_trait]
impl SessionGenerationFactory for GatedGenerationFactory {
    fn session_registered(&self, _session: &PiSession) {
        self.0.registered.notify_one();
    }

    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        self.0.wait_at(ReplacementPause::Prepare).await;
        let runtime = request
            .generation_overlay
            .apply_to(PiRuntime::builder())
            .plugin(GatedLifecyclePlugin(self.0.clone()))
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                cwd: request.cwd,
                ..AgentOptions::default()
            })
            .build()?;
        let options = AgentSessionOptions::default();
        Ok(PreparedSessionGeneration::new(runtime, options)
            .with_activation(GatedActivation(Arc::clone(&self.0.activation_committed))))
    }
}

fn test_manager() -> MultiSessionManager {
    test_manager_with_turns([])
}

fn text_turn_with_usage(text: &str, total_tokens: u64) -> ScriptedTurn {
    ScriptedTurn::Events(vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new("scripted".into(), "test".into(), "scripted", 0),
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
                input: total_tokens,
                total_tokens,
                ..Usage::default()
            },
        },
    ])
}

fn test_manager_with_turns(turns: impl IntoIterator<Item = ScriptedTurn>) -> MultiSessionManager {
    let turns = turns.into_iter().collect::<Vec<_>>();
    MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let turns = turns.clone();
        async move {
            let runtime = request
                .generation_overlay
                .apply_to(PiRuntime::builder())
                .provider_plugin(ScriptedProviderPlugin::scripted(turns))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd: request.cwd,
                    ..AgentOptions::default()
                })
                .build()?;
            Ok(PreparedSessionGeneration::new(
                runtime,
                AgentSessionOptions::default(),
            ))
        }
    })
}

#[tokio::test]
async fn cancelling_launch_before_readiness_stops_the_unclaimed_child() {
    use std::future::Future;
    use std::task::Poll;

    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("owner.jsonl"),
        ))
        .await
        .unwrap();
    let child = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("child.jsonl"),
        ))
        .await
        .unwrap();
    let id = pi_plugin::IsolatedSessionId::new(child.registration_id().to_string());
    let mut launch = Box::pin(manager.inner.isolated_sessions.launch(
        owner.registration_id().to_string(),
        child.clone(),
        CustomMessageContent::Text("wait".into()),
    ));
    std::future::poll_fn(|context| {
        assert!(launch.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(launch);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        owner.wait_for_isolated_session(&id),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(!child.current().runtime().agent().is_running());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn fresh_isolated_session_runs_without_replacing_its_owner() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([text_turn_with_usage("isolated answer", 37)]);
    let owner_path = directory.path().join("primary.jsonl");
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &owner_path,
        ))
        .await
        .unwrap();
    let owner_id = owner.id();
    let owner_leaf = owner.current().store().leaf_id();

    let isolated_id = owner
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "inspect independently".to_string(),
        )))
        .await
        .unwrap();
    let outcome = owner.wait_for_isolated_session(&isolated_id).await.unwrap();

    assert_eq!(owner.id(), owner_id);
    assert_eq!(owner.path(), owner_path);
    assert_eq!(owner.current().store().leaf_id(), owner_leaf);
    assert!(!owner.current().runtime().agent().is_running());
    assert!(!outcome.aborted);
    assert_eq!(outcome.usage.total_tokens, 37);
    assert!(outcome.messages.iter().any(|message| {
        matches!(message, Message::Assistant(assistant)
        if assistant.content.iter().any(|content| {
            matches!(content, ContentBlock::Text(text) if text.text == "isolated answer")
        }))
    }));
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == isolated_id.as_str())
        .unwrap();
    assert_eq!(outcome.session_id, child.id());
    let observed = owner.observe_isolated_session(&isolated_id).unwrap();
    let usage = observed.usage_snapshot().unwrap();
    assert_eq!(usage.usage.total_tokens, 37);
    assert!(usage.context_tokens.is_some());
    assert_eq!(
        owner.current().runtime().execution_origin(),
        pi_plugin::SessionExecutionOrigin::User
    );
    assert_eq!(
        child.current().runtime().execution_origin(),
        pi_plugin::SessionExecutionOrigin::Subagent
    );
    child.reload().await.unwrap();
    assert_eq!(
        child.current().runtime().execution_origin(),
        pi_plugin::SessionExecutionOrigin::Subagent
    );
    let grandchild_id = child
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "nested inspection".into(),
        )))
        .await
        .unwrap();
    child
        .wait_for_isolated_session(&grandchild_id)
        .await
        .unwrap();
    let grandchild = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == grandchild_id.as_str())
        .unwrap();
    assert_eq!(
        grandchild.current().runtime().execution_origin(),
        pi_plugin::SessionExecutionOrigin::Subagent
    );
    assert_eq!(
        owner.current().runtime().execution_origin(),
        pi_plugin::SessionExecutionOrigin::User
    );
    assert!(
        child
            .path()
            .starts_with(directory.path().join("primary/isolated"))
    );
    assert!(child.path().exists());
    assert!(!owner_path.exists());

    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn isolated_session_reuses_identity_across_turns_and_reports_usage_deltas() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([
        text_turn_with_usage("first answer", 11),
        text_turn_with_usage("follow-up answer", 7),
    ]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("primary.jsonl"),
        ))
        .await
        .unwrap();
    let id = owner
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "first task".into(),
        )))
        .await
        .unwrap();
    let first = owner.wait_for_isolated_session(&id).await.unwrap();
    assert_eq!(first.usage.total_tokens, 11);

    let message = owner
        .send_to_isolated_session(&id, CustomMessageContent::Text("mailbox context".into()))
        .unwrap();
    assert_eq!(
        message.accepted_as,
        pi_plugin::IsolatedMessageDelivery::Mailbox
    );
    let custom = owner
        .send_custom_to_isolated_session(
            &id,
            CustomMessageInput {
                custom_type: "agent_message".into(),
                content: CustomMessageContent::Text("typed mailbox context".into()),
                display: true,
                details: Some(serde_json::json!({"sourceRecordId":"event-1"})),
            },
        )
        .unwrap();
    assert_eq!(
        custom.accepted_as,
        pi_plugin::IsolatedMessageDelivery::Mailbox
    );
    let follow_up = owner
        .follow_up_isolated_session(
            &id,
            CustomMessageContent::Text("continue in the same session".into()),
        )
        .await
        .unwrap();
    assert!(follow_up.started);
    assert_ne!(follow_up.turn_id.as_str(), id.as_str());
    let second = owner
        .isolated_session_turn_waiter(&id, &follow_up.turn_id)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.usage.total_tokens, 7);
    assert!(second.messages.iter().any(|message| {
        matches!(message, Message::Assistant(assistant)
        if assistant.content.iter().any(|content| {
            matches!(content, ContentBlock::Text(text) if text.text == "follow-up answer")
        }))
    }));
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == id.as_str())
        .unwrap();
    let document = child.current().store().load().unwrap();
    let context = document.context().unwrap();
    assert!(format!("{:?}", context.messages).contains("mailbox context"));
    let context = format!("{:?}", context.messages);
    assert!(context.contains("typed mailbox context"));
    assert!(context.contains("event-1"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn isolated_initial_state_is_applied_without_mutating_the_owner() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("primary.jsonl"),
        ))
        .await
        .unwrap();
    let owner_state = owner.current().runtime().agent().state();
    let isolated_id = owner
        .launch_isolated_session(
            IsolatedSessionRequest::new(CustomMessageContent::Text("inspect".to_string())).options(
                IsolatedSessionOptions {
                    active_tools: Some(Vec::new()),
                    model: Some(ModelSelection::new("scripted", "child-model")),
                    thinking_level: Some(pi_core::ThinkingLevel::High),
                    ..IsolatedSessionOptions::default()
                },
            ),
        )
        .await
        .unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == isolated_id.as_str())
        .unwrap();
    let child_state = child.current().runtime().agent().state();

    assert_eq!(child_state.model_id, ModelId::new("child-model"));
    assert_eq!(child_state.thinking_level, pi_core::ThinkingLevel::High);
    assert!(child_state.active_tools.is_empty());
    let child_context = child.current().store().load().unwrap().context().unwrap();
    assert_eq!(
        child_context.model,
        Some(crate::SessionModel {
            provider: ProviderId::new("scripted"),
            model_id: ModelId::new("child-model"),
        })
    );
    assert_eq!(child_context.thinking_level, "high");
    assert_eq!(child_context.active_tool_names, Some(Vec::new()));
    let owner_after = owner.current().runtime().agent().state();
    assert_eq!(owner_after.provider_id, owner_state.provider_id);
    assert_eq!(owner_after.model_id, owner_state.model_id);
    assert_eq!(owner_after.thinking_level, owner_state.thinking_level);
    assert_eq!(owner_after.active_tools, owner_state.active_tools);

    owner.abort_isolated_session(&isolated_id).unwrap();
    owner.wait_for_isolated_session(&isolated_id).await.unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_context_rejects_an_unsaved_parent_before_creating_a_child() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager();
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("unsaved.jsonl"),
        ))
        .await
        .unwrap();
    let error = owner
        .launch_isolated_session(
            IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into())).options(
                IsolatedSessionOptions {
                    context: pi_plugin::IsolatedContextMode::Fork,
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot fork an unsaved session"));
    assert_eq!(manager.sessions().len(), 1);
    assert!(!owner.path().exists());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_turn_limit_keeps_recent_complete_turns_and_rejects_invalid_combinations() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([
        ScriptedTurn::Text("answer one".into()),
        ScriptedTurn::Text("answer two".into()),
        ScriptedTurn::Text("answer three".into()),
        ScriptedTurn::Text("child answer".into()),
    ]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("parent.jsonl"),
        ))
        .await
        .unwrap();
    owner.current().prompt("user one").await.unwrap();
    owner.current().prompt("user two").await.unwrap();
    owner.current().prompt("user three").await.unwrap();

    let id = owner
        .launch_isolated_session(
            IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into())).options(
                IsolatedSessionOptions {
                    context: pi_plugin::IsolatedContextMode::Fork,
                    fork_turns: Some(2),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
    owner.wait_for_isolated_session(&id).await.unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == id.as_str())
        .unwrap();
    let inherited = child
        .current()
        .store()
        .load()
        .unwrap()
        .inherited_context()
        .unwrap()
        .unwrap();
    let inherited = serde_json::to_string(&inherited.messages).unwrap();
    assert!(!inherited.contains("user one"));
    assert!(!inherited.contains("answer one"));
    assert!(inherited.contains("user two"));
    assert!(inherited.contains("answer two"));
    assert!(inherited.contains("user three"));
    assert!(inherited.contains("answer three"));

    let before_count = manager.sessions().len();
    for options in [
        IsolatedSessionOptions {
            context: pi_plugin::IsolatedContextMode::Fresh,
            fork_turns: Some(1),
            ..Default::default()
        },
        IsolatedSessionOptions {
            context: pi_plugin::IsolatedContextMode::Fork,
            fork_turns: Some(0),
            ..Default::default()
        },
    ] {
        assert!(
            owner
                .launch_isolated_session(
                    IsolatedSessionRequest::new(CustomMessageContent::Text("invalid".into()))
                        .options(options)
                )
                .await
                .is_err()
        );
        assert_eq!(manager.sessions().len(), before_count);
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_point_excludes_later_compaction_and_rejects_invalid_sources() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::Text("answer".into())]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("parent.jsonl"),
        ))
        .await
        .unwrap();
    assert!(owner.current().isolated_fork_point().unwrap().is_none());
    owner.current().prompt("original history").await.unwrap();
    let fork_point = owner.current().isolated_fork_point().unwrap().unwrap();
    let expected = owner
        .current()
        .store()
        .load()
        .unwrap()
        .context()
        .unwrap()
        .messages;
    owner
        .current()
        .store()
        .append_session_record(crate::SessionEntry::Compaction(crate::CompactionEntry {
            retained_entry_ids: Vec::new(),
            summary: "later context must not leak".into(),
            retained_tail: vec![],
            tokens_before: 10000,
            details: None,
            usage: None,
        }))
        .unwrap();
    let id = owner
        .launch_isolated_session(
            IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into())).options(
                IsolatedSessionOptions {
                    context: pi_plugin::IsolatedContextMode::Fork,
                    fork_point: Some(fork_point.clone()),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
    owner.wait_for_isolated_session(&id).await.unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == id.as_str())
        .unwrap();
    let document = child.current().store().load().unwrap();
    let inherited = document.inherited_context().unwrap().unwrap();
    assert_eq!(inherited.parent_session_id, owner.id());
    assert_eq!(
        inherited.parent_entry_id.as_deref(),
        Some(fork_point.parent_entry_id.as_str())
    );
    assert_eq!(inherited.messages, expected);
    assert!(
        document
            .entries
            .iter()
            .any(|record| record.id == inherited.snapshot_entry_id)
    );
    assert_eq!(
        serde_json::to_value(&document.context().unwrap().messages[..expected.len()]).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let before_count = manager.sessions().len();
    for (mode, source) in [
        (pi_plugin::IsolatedContextMode::Fresh, fork_point.clone()),
        (
            pi_plugin::IsolatedContextMode::Fork,
            pi_plugin::IsolatedForkPoint {
                parent_session_id: "foreign".into(),
                ..fork_point.clone()
            },
        ),
        (
            pi_plugin::IsolatedContextMode::Fork,
            pi_plugin::IsolatedForkPoint {
                parent_entry_id: "missing".into(),
                ..fork_point
            },
        ),
    ] {
        assert!(
            owner
                .launch_isolated_session(
                    IsolatedSessionRequest::new(CustomMessageContent::Text("invalid".into()))
                        .options(IsolatedSessionOptions {
                            context: mode,
                            fork_point: Some(source),
                            ..Default::default()
                        })
                )
                .await
                .is_err()
        );
        assert_eq!(manager.sessions().len(), before_count);
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn forked_context_survives_reload_without_importing_configuration_or_usage() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::Text("answer".into())]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("parent.jsonl"),
        ))
        .await
        .unwrap();
    owner.current().prompt("parent history").await.unwrap();
    let extension = crate::AgentMessage::custom(serde_json::json!({
        "role": "user", "content": [{"type": "text", "text": "retained user"},
            {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}],
        "timestamp": 2, "futureExtension": {"preserve": true}
    }))
    .unwrap();
    owner
        .current()
        .store()
        .append_session_record(crate::SessionEntry::Compaction(crate::CompactionEntry {
            retained_entry_ids: Vec::new(),
            summary: "parent compacted summary".into(),
            retained_tail: vec![extension.clone()],
            tokens_before: 10000,
            details: None,
            usage: None,
        }))
        .unwrap();
    let before = owner.current().store().load().unwrap();
    let expected = before.context().unwrap().messages;
    let id = owner
        .launch_isolated_session(
            IsolatedSessionRequest::new(CustomMessageContent::Text("child task".into())).options(
                IsolatedSessionOptions {
                    context: pi_plugin::IsolatedContextMode::Fork,
                    active_tools: Some(Vec::new()),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
    let outcome = owner.wait_for_isolated_session(&id).await.unwrap();
    assert_eq!(outcome.messages.len(), 2);
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == id.as_str())
        .unwrap();
    let document = child.current().store().load().unwrap();
    assert_eq!(
        serde_json::to_value(&document.context().unwrap().messages[..expected.len()]).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(document.messages().len(), 2);
    let seed_entry = document.entries.iter().find(|record| matches!(&record.entry, crate::SessionEntry::Custom(custom) if custom.custom_type == crate::history::ISOLATED_CONTEXT_TYPE)).unwrap();
    assert!(crate::session_entry_usage(&seed_entry.entry).is_none());
    let child_history = child.current().runtime().agent().state().messages;
    child.reload().await.unwrap();
    assert_eq!(
        child.current().runtime().agent().state().messages,
        child_history
    );
    assert!(
        child
            .current()
            .runtime()
            .agent()
            .state()
            .active_tools
            .is_empty()
    );
    assert_eq!(
        owner.current().store().load().unwrap().entries,
        before.entries
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn isolated_tools_cannot_exceed_the_calling_session_ceiling() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager();
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("primary.jsonl"),
        ))
        .await
        .unwrap();
    let error = owner
        .launch_isolated_session(
            IsolatedSessionRequest::new(CustomMessageContent::Text("inspect".to_string())).options(
                IsolatedSessionOptions {
                    active_tools: Some(vec!["write".to_string()]),
                    ..IsolatedSessionOptions::default()
                },
            ),
        )
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("not active in the calling session")
    );
    assert_eq!(manager.sessions().len(), 1);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn isolated_session_abort_finishes_with_an_aborted_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("primary.jsonl"),
        ))
        .await
        .unwrap();
    let isolated_id = owner
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "wait".to_string(),
        )))
        .await
        .unwrap();
    owner.abort_isolated_session(&isolated_id).unwrap();
    let outcome = owner.wait_for_isolated_session(&isolated_id).await.unwrap();

    assert!(outcome.aborted);
    assert_eq!(owner.path(), directory.path().join("primary.jsonl"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn isolated_control_is_owner_scoped_and_closed_owners_cannot_launch() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("primary.jsonl"),
        ))
        .await
        .unwrap();
    let other = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("other.jsonl"),
        ))
        .await
        .unwrap();
    let isolated_id = owner
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "wait".to_string(),
        )))
        .await
        .unwrap();

    let observation = owner.observe_isolated_session(&isolated_id).unwrap();
    assert_eq!(observation.isolated_id(), &isolated_id);
    assert_eq!(observation.cwd(), owner.cwd());
    assert!(!observation.session_id().is_empty());
    assert!(observation.snapshot().agent.is_running);
    let subscription = observation.subscribe();
    assert!(subscription.snapshot.revision <= observation.snapshot().revision);

    assert!(other.abort_isolated_session(&isolated_id).is_err());
    assert!(other.observe_isolated_session(&isolated_id).is_err());
    let foreign_wait = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        other.wait_for_isolated_session(&isolated_id),
    )
    .await
    .expect("ownership rejection must not wait for the child");
    assert!(foreign_wait.is_err());

    owner.abort_isolated_session(&isolated_id).unwrap();
    assert!(
        owner
            .wait_for_isolated_session(&isolated_id)
            .await
            .unwrap()
            .aborted
    );
    manager.close_session(&owner).await.unwrap();
    assert!(matches!(
        owner
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "orphan".to_string()
            ),))
            .await,
        Err(MultiSessionManagerError::UnknownSession)
    ));

    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn closing_an_owner_closes_its_isolated_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager_with_turns([ScriptedTurn::WaitForAbort]);
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("primary.jsonl"),
        ))
        .await
        .unwrap();
    let isolated_id = owner
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "wait".to_string(),
        )))
        .await
        .unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.registration_id() == isolated_id.as_str())
        .unwrap();

    manager.close_session(&owner).await.unwrap();

    assert!(owner.current().is_closed());
    assert!(child.current().is_closed());
    assert!(manager.sessions().is_empty());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn manager_owns_multiple_sessions_and_closes_them() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager();
    let first = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("first.jsonl"),
        ))
        .await
        .unwrap();
    let second = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("second.jsonl"),
        ))
        .await
        .unwrap();

    assert_ne!(first.id(), second.id());
    assert_eq!(manager.sessions().len(), 2);

    manager.close_session(&first).await.unwrap();
    assert!(first.current().is_closed());
    assert!(matches!(
        first.reload().await,
        Err(MultiSessionManagerError::UnknownSession)
    ));
    assert!(!second.current().is_closed());
    assert_eq!(manager.sessions().len(), 1);

    manager.shutdown().await.unwrap();
    assert!(second.current().is_closed());
    assert!(manager.sessions().is_empty());
    assert!(matches!(
        manager
            .create_session(pi_session::CreateSessionOptions::new(
                directory.path(),
                directory.path().join("third.jsonl")
            ))
            .await,
        Err(MultiSessionManagerError::Closed)
    ));
}

#[tokio::test]
async fn weak_session_handle_does_not_keep_the_manager_or_session_alive() {
    let directory = tempfile::tempdir().unwrap();
    let manager = test_manager();
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("session.jsonl"),
        ))
        .await
        .unwrap();
    let weak = session.downgrade();

    assert_eq!(weak.upgrade().unwrap().id(), session.id());
    drop(session);
    drop(manager);

    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn opening_an_active_path_reuses_its_handle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let manager = test_manager();
    let created = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &path,
        ))
        .await
        .unwrap();
    created.current().store().materialize().unwrap();

    let opened = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();

    assert_eq!(created.registration_id, opened.registration_id);
    assert_eq!(manager.sessions().len(), 1);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn opening_a_preloaded_log_preserves_the_replayed_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let store = SessionStore::create(
        &path,
        crate::SessionHeader::new("preloaded-session", directory.path()),
    )
    .unwrap();
    let entry_id = store
        .append_custom_entry("preloaded", Some(serde_json::json!({ "ready": true })))
        .unwrap();
    drop(store);

    let replayed = SessionStore::open_handle(&path).unwrap();
    let manager = test_manager();
    let opened = manager
        .open_session(
            SessionSource::Store(replayed),
            OpenSessionOptions {
                generation_overlay: SessionGenerationOverlay::new()
                    .with_execution_origin(pi_plugin::SessionExecutionOrigin::Subagent),
            },
        )
        .await
        .unwrap();

    assert_eq!(opened.id(), "preloaded-session");
    assert_eq!(opened.path(), path);
    assert_eq!(
        opened.current().catalog().execution_origin(),
        pi_plugin::SessionExecutionOrigin::Subagent
    );
    let document = opened.current().store().load().unwrap();
    assert!(document.entries.iter().any(|entry| entry.id == entry_id));
    manager.shutdown().await.unwrap();
}

async fn assert_cancelled_managed_replacement_finishes(phase: ReplacementPause) {
    let directory = tempfile::tempdir().unwrap();
    let gates = ReplacementGates::default();
    let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
    let original_path = directory.path().join("original.jsonl");
    let replacement_path = directory.path().join("replacement.jsonl");
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &original_path,
        ))
        .await
        .unwrap();
    let original = owner.current();
    let mut replacements = owner.subscribe();

    gates.pause_at(phase);
    let replacement = tokio::spawn({
        let owner = owner.clone();
        let cwd = directory.path().to_path_buf();
        let path = replacement_path.clone();
        async move { owner.new_session(cwd, path).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
        .await
        .expect("replacement did not reach the requested pause");

    replacement.abort();
    assert!(replacement.await.unwrap_err().is_cancelled());
    assert_eq!(owner.path(), original_path);

    let mut contender = tokio::spawn({
        let manager = manager.clone();
        let cwd = directory.path().to_path_buf();
        let path = replacement_path.clone();
        async move {
            manager
                .create_session(pi_session::CreateSessionOptions::new(cwd, path))
                .await
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut contender)
            .await
            .is_err(),
        "the replacement must retain the manager path guard after caller cancellation"
    );

    gates.resume();
    tokio::time::timeout(std::time::Duration::from_secs(2), replacements.changed())
        .await
        .expect("detached replacement did not publish")
        .unwrap();
    assert_eq!(owner.path(), replacement_path);
    assert!(original.is_closed());
    assert!(!owner.current().is_closed());

    let contender_result = tokio::time::timeout(std::time::Duration::from_secs(2), contender)
        .await
        .expect("path contender remained blocked after publication")
        .unwrap();
    let error = match contender_result {
        Ok(_) => panic!("path contender unexpectedly acquired the replacement path"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        MultiSessionManagerError::SessionAlreadyActive(_)
    ));
    assert_eq!(manager.sessions().len(), 1);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn manager_gate_serializes_replacements_before_runtime_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let gates = ReplacementGates::default();
    let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
    let original_path = directory.path().join("original.jsonl");
    let replacement_path = directory.path().join("replacement.jsonl");
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &original_path,
        ))
        .await
        .unwrap();

    gates.pause_at(ReplacementPause::Prepare);
    let first = tokio::spawn({
        let owner = owner.clone();
        let cwd = directory.path().to_path_buf();
        let path = replacement_path.clone();
        async move { owner.new_session(cwd, path).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
        .await
        .expect("first replacement did not enter preparation");

    let mut second = tokio::spawn({
        let owner = owner.clone();
        async move { owner.reload().await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "a second replacement must wait before capturing the runtime session"
    );

    gates.resume();
    assert_eq!(
        first.await.unwrap().unwrap(),
        AgentSessionReplacement::Replaced
    );
    second.await.unwrap().unwrap();
    assert_eq!(owner.path(), replacement_path);
    assert!(!owner.current().is_closed());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_managed_replacement_during_prepare_still_publishes_it() {
    assert_cancelled_managed_replacement_finishes(ReplacementPause::Prepare).await;
}

#[tokio::test]
async fn cancelling_managed_replacement_during_shutdown_still_publishes_it() {
    assert_cancelled_managed_replacement_finishes(ReplacementPause::Shutdown).await;
}

#[tokio::test]
async fn cancelling_managed_replacement_during_start_still_publishes_it() {
    assert_cancelled_managed_replacement_finishes(ReplacementPause::Start).await;
}

#[tokio::test]
async fn cancelling_managed_acquisition_during_start_still_registers_it() {
    let directory = tempfile::tempdir().unwrap();
    let gates = ReplacementGates::default();
    gates.pause_at(ReplacementPause::Start);
    let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
    let path = directory.path().join("session.jsonl");

    let acquisition = tokio::spawn({
        let manager = manager.clone();
        let cwd = directory.path().to_path_buf();
        let path = path.clone();
        async move {
            manager
                .create_session(pi_session::CreateSessionOptions::new(cwd, path))
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
        .await
        .expect("acquisition did not reach session_start");
    assert!(gates.activation_committed.load(Ordering::Acquire));
    assert!(manager.sessions().is_empty());

    acquisition.abort();
    let cancellation = match acquisition.await {
        Err(error) => error,
        Ok(_) => panic!("acquisition caller unexpectedly completed"),
    };
    assert!(cancellation.is_cancelled());

    let mut contender = tokio::spawn({
        let manager = manager.clone();
        let cwd = directory.path().to_path_buf();
        let path = path.clone();
        async move {
            manager
                .create_session(pi_session::CreateSessionOptions::new(cwd, path))
                .await
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut contender)
            .await
            .is_err(),
        "the acquisition must retain the manager lifecycle guard after caller cancellation"
    );

    gates.resume();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gates.registered.notified(),
    )
    .await
    .expect("detached acquisition did not register");
    let sessions = manager.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].path(), path);
    assert!(!sessions[0].current().is_closed());

    let contender_result = tokio::time::timeout(std::time::Duration::from_secs(2), contender)
        .await
        .expect("path contender remained blocked after registration")
        .unwrap();
    let error = match contender_result {
        Ok(_) => panic!("path contender unexpectedly acquired the registered path"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        MultiSessionManagerError::SessionAlreadyActive(_)
    ));
    manager.shutdown().await.unwrap();
}

async fn assert_cancelled_isolated_acquisition_closes_unclaimed_child(phase: ReplacementPause) {
    let directory = tempfile::tempdir().unwrap();
    let gates = ReplacementGates::default();
    let manager = MultiSessionManager::new(GatedGenerationFactory(gates.clone()));
    let owner = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("owner.jsonl"),
        ))
        .await
        .unwrap();
    // Consume the owner's registration notification so the next one
    // belongs to the candidate child.
    gates.registered.notified().await;
    gates.activation_committed.store(false, Ordering::Release);
    gates.pause_at(phase);

    let launch = tokio::spawn({
        let owner = owner.clone();
        async move {
            owner
                .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                    "inspect".to_string(),
                )))
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), gates.entered.notified())
        .await
        .expect("isolated acquisition did not reach the requested pause");
    assert_eq!(
        gates.activation_committed.load(Ordering::Acquire),
        phase == ReplacementPause::Start
    );
    assert_eq!(manager.sessions().len(), 1);

    launch.abort();
    assert!(launch.await.unwrap_err().is_cancelled());
    gates.resume();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gates.registered.notified(),
    )
    .await
    .expect("detached child acquisition did not register before cleanup");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gates.shutdowns.load(Ordering::Acquire) != 1 || manager.sessions().len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unclaimed isolated child was not closed");
    assert_eq!(
        manager.sessions()[0].registration_id(),
        owner.registration_id()
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_isolated_acquisition_during_prepare_closes_the_unclaimed_child() {
    assert_cancelled_isolated_acquisition_closes_unclaimed_child(ReplacementPause::Prepare).await;
}

#[tokio::test]
async fn cancelling_isolated_acquisition_during_start_closes_the_unclaimed_child() {
    assert_cancelled_isolated_acquisition_closes_unclaimed_child(ReplacementPause::Start).await;
}

#[tokio::test]
async fn transition_rejects_a_path_owned_by_another_handle() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let second_path = directory.path().join("second.jsonl");
    let manager = test_manager();
    let first = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &first_path,
        ))
        .await
        .unwrap();
    let _second = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &second_path,
        ))
        .await
        .unwrap();

    let error = first.resume_session(&second_path).await.unwrap_err();

    assert!(matches!(
        error,
        MultiSessionManagerError::SessionAlreadyActive(_)
    ));
    assert_eq!(first.path(), first_path);
    manager.shutdown().await.unwrap();
}
