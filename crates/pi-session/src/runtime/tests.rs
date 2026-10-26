use crate::OperationIntent;
fn generic_system_prompt() -> SystemPrompt {
    SystemPrompt::dynamic(|_: &pi_core::WorkspaceSnapshot| {
        Ok(pi_runtime::PreparedSystemPrompt::new(
            |_: pi_runtime::PromptContext<'_>| {
                Ok(pi_runtime::PromptOutput::new(
                    "Assist with the user's task.",
                ))
            },
        ))
    })
}

use async_trait::async_trait;
use pi_agent::{AgentLoopStop, AgentOptions};
use pi_core::{
    AgentEvent, ContentBlock, CustomMessage, CustomMessageContent, Message, PluginId,
    ResponseMetadata, StreamEvent, TextContent, Usage, UserMessage,
};
use pi_plugin::{
    AgentPluginContext, AgentSettledEvent, BeforeAgentStartEvent, BeforeAgentStartPatch, Command,
    CommandContext, CommandError, CommandOutcome, CommandSpec, MessageEndEvent, MessageEndPatch,
    Plugin, PluginError, RegisterContext,
};
use pi_runtime::SystemPrompt;
use pi_test_support::TestToolsPlugin;
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

use super::compaction::prepare_compaction;
use super::navigation::{branch_summary_input_messages, tree_preparation};
use super::options::AutoRetrySettings;
use super::shell_executor::SessionShellExecutor;
use super::{AgentSession, PromptOutcome, SessionInput, ShellExecutionOptions};
use crate::{
    ActiveToolsEntry, AgentMessage, CompactionEntry, CompactionSettings, CustomEntry,
    LaneRecordEntry, MAIN_LANE, ModelChangeEntry, NewLaneRecord, QueueKind,
    SessionBeforeCompactEvent, SessionBeforeTreeEvent, SessionCompactEvent, SessionEntry,
    SessionError, SessionShutdownEvent, SessionStore, ThinkingLevelEntry, now_ms,
};
use crate::{
    AgentSessionEvent, AgentSessionOptions, EntryOrder, EntryQuery, GenericCompactionPolicy,
    OperationError, OperationOutcome, ProvisionedEntry, SUMMARIZATION_SYSTEM_PROMPT,
    SessionBeforeCompactResult, SessionBeforeTreeResult, SessionEntryType, SessionHeader,
    SessionPluginContext, SessionStartEvent, SessionStartReason,
};
use pi_core::{ImageContent, ModelId, ProviderId, StopReason, ThinkingLevel};
use pi_runtime::PiRuntime;
use pi_shell::{ShellChunk, ShellRequest, ShellResult};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

fn text_turn_with_usage(text: &str, total_tokens: u64) -> ScriptedTurn {
    ScriptedTurn::Events(vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new(
                "scripted".into(),
                "test".into(),
                "scripted",
                now_ms().saturating_add(60_000),
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
                input: total_tokens,
                total_tokens,
                ..Usage::default()
            },
        },
    ])
}

struct PersistMessageEndReplacement;

struct FailingAgentSettledPlugin;

struct BlockingAgentSettledPlugin {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

struct CountingAgentLifecyclePlugin {
    ends: Arc<AtomicUsize>,
    settled: Arc<AtomicUsize>,
}

#[pi_plugin::plugin]
impl Plugin for FailingAgentSettledPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("failing-agent-settled")
    }

    async fn agent_settled(
        &self,
        _context: AgentPluginContext,
        _event: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        Err(PluginError::Hook {
            plugin_id: self.id(),
            hook: "agent_settled",
            message: "intentional settled failure".to_string(),
        })
    }
}

#[pi_plugin::plugin]
impl Plugin for BlockingAgentSettledPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("blocking-agent-settled")
    }

    async fn agent_settled(
        &self,
        _context: AgentPluginContext,
        _event: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[pi_plugin::plugin]
impl Plugin for CountingAgentLifecyclePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("counting-agent-lifecycle")
    }

    async fn agent_end(
        &self,
        _context: AgentPluginContext,
        _event: pi_plugin::AgentEndEvent,
    ) -> Result<(), PluginError> {
        self.ends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn agent_settled(
        &self,
        _context: AgentPluginContext,
        _event: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        self.settled.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[pi_plugin::plugin]
impl Plugin for PersistMessageEndReplacement {
    fn id(&self) -> PluginId {
        PluginId::new("persist-message-end-replacement")
    }

    async fn message_end(
        &self,
        _context: AgentPluginContext,
        event: MessageEndEvent,
    ) -> Result<MessageEndPatch, PluginError> {
        let message = match event.message {
            Message::User(mut user) => {
                user.content = vec![ContentBlock::Text(TextContent::new("persisted user"))];
                Message::User(user)
            }
            Message::Assistant(assistant) => {
                let mut assistant = (*assistant).clone();
                assistant.content =
                    vec![ContentBlock::Text(TextContent::new("persisted assistant"))];
                Message::assistant(assistant)
            }
            message => message,
        };
        Ok(MessageEndPatch {
            message: Some(message),
        })
    }
}

fn scripted_runtime(turns: impl IntoIterator<Item = ScriptedTurn>) -> PiRuntime {
    scripted_runtime_in(turns, std::path::Path::new("."))
}

fn scripted_runtime_in(
    turns: impl IntoIterator<Item = ScriptedTurn>,
    cwd: &std::path::Path,
) -> PiRuntime {
    PiRuntime::builder()
        .workspace(pi_core::WorkspaceSpec::from_cwd(cwd))
        .provider_plugin(ScriptedProviderPlugin::scripted(turns))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap()
}

#[tokio::test]
async fn settled_open_restores_the_selected_context() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settled-session.jsonl");
    let runtime = scripted_runtime_in([], directory.path());
    let state = runtime.agent().state();
    let store = SessionStore::create(
        &path,
        SessionHeader::new("settled-session", directory.path()),
    )
    .unwrap();
    store
        .append_batch([
            SessionEntry::ModelChange(ModelChangeEntry {
                provider: state.provider_id,
                model_id: state.model_id,
            }),
            SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                thinking_level: state.thinking_level.as_str().to_string(),
            }),
            SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                active_tool_names: state.active_tools,
            }),
            SessionEntry::message(Message::User(UserMessage::text("remember me", 1))),
        ])
        .unwrap();
    drop(store);

    let session = AgentSession::open(runtime, &path, Default::default())
        .await
        .unwrap();

    assert_eq!(session.runtime().agent().state().messages.len(), 1);
    assert_eq!(
        session
            .history()
            .context()
            .unwrap()
            .runtime_messages()
            .len(),
        1
    );
}

#[tokio::test]
async fn initial_model_fallback_is_reported_to_the_frontend() {
    let directory = tempfile::tempdir().unwrap();
    let session = AgentSession::create(
        scripted_runtime([]),
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default()
            .initial_model_fallback_message(Some("using fallback model".to_string())),
    )
    .await
    .unwrap();

    let mut subscription = session.subscribe();
    let event = subscription.events.recv().await.unwrap();
    assert!(matches!(
        event.event,
        AgentSessionEvent::PluginNotice {
            message,
            level: crate::NoticeLevel::Warning,
        } if message == "using fallback model"
    ));
}

#[tokio::test]
async fn extension_messages_and_entries_update_live_and_resumed_session_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let provider_plugin =
        ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();
    let custom = CustomMessage {
        custom_type: "fixture-context".to_string(),
        content: CustomMessageContent::Text("remember this".to_string()),
        display: true,
        details: Some(serde_json::json!({"source": "extension"})),
        timestamp_ms: now_ms(),
    };

    let custom_id = session.append_custom_message(custom.clone()).unwrap();
    assert_eq!(
        session
            .set_name_immediate(Some("  Core\r\n\n session  ".to_string()))
            .unwrap()
            .as_deref(),
        Some("Core  session")
    );
    assert!(session.runtime().agent().state().messages.iter().any(
        |message| matches!(message, Message::Custom(value)
                if value.custom_type == custom.custom_type
                    && value.content == custom.content
                    && value.display == custom.display
                    && value.details == custom.details)
    ));
    let state_id = session
        .append_custom_entry("fixture-state", Some(serde_json::json!({"count": 1})))
        .unwrap();
    session
        .set_label(&custom_id, Some("checkpoint".to_string()))
        .unwrap();
    session.prompt("continue").await.unwrap();

    assert!(provider.requests()[0].messages.iter().any(|message| {
        matches!(message, Message::User(user)
            if user.content.iter().any(|block| matches!(block,
                ContentBlock::Text(text) if text.text == "remember this")))
    }));
    let document = session.store().load().unwrap();
    assert_eq!(document.name.as_deref(), Some("Core  session"));
    assert_eq!(
        document.labels.get(&custom_id).map(String::as_str),
        Some("checkpoint")
    );
    assert!(document.entries.iter().any(|record| {
        record.id == state_id
            && matches!(&record.entry, SessionEntry::Custom(entry)
                if entry.custom_type == "fixture-state")
    }));

    let queued = Message::custom(CustomMessage {
        custom_type: "fixture-next".to_string(),
        content: CustomMessageContent::Text("next run".to_string()),
        display: false,
        details: None,
        timestamp_ms: now_ms(),
    });
    session.enqueue_message(queued, QueueKind::NextRun).unwrap();
    drop(session);

    let resumed_plugin =
        ScriptedProviderPlugin::scripted([ScriptedTurn::Text("resumed".to_string())]);
    let resumed_provider = resumed_plugin.provider();
    let resumed_runtime = PiRuntime::builder()
        .provider_plugin(resumed_plugin)
        .build()
        .unwrap();
    let reopened = AgentSession::open(resumed_runtime, &path, Default::default())
        .await
        .unwrap();
    assert_eq!(reopened.snapshot().queue.follow_up, vec!["next run"]);
    reopened.prompt("continue again").await.unwrap();
    let requests = resumed_provider.requests();
    assert!(requests[0].messages.iter().any(|message| {
        matches!(message, Message::User(user)
                if user.content.iter().any(|block| matches!(block,
                    ContentBlock::Text(text) if text.text == "remember this")))
    }));
}

#[tokio::test]
async fn session_preserves_an_explicit_agent_message_converter() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
    let provider = plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(plugin)
        .agent_options(AgentOptions {
            convert_to_llm: pi_agent::ConvertToLlm::new(|messages| async move {
                messages
                    .into_iter()
                    .filter(|message| !matches!(message, Message::Custom(_)))
                    .collect()
            }),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("explicit-converter.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let custom = CustomMessage {
        custom_type: "notification".to_string(),
        content: CustomMessageContent::Text("internal notification".to_string()),
        display: false,
        details: None,
        timestamp_ms: 1,
    };
    session.append_custom_message(custom.clone()).unwrap();
    let stored = session.runtime().agent().state().messages[0].clone();
    session.prompt("hello").await.unwrap();

    let requests = provider.requests();
    assert!(matches!(&requests[0].messages[..], [Message::User(user)]
        if matches!(&user.content[..], [ContentBlock::Text(text)] if text.text == "hello")));
    assert_eq!(session.runtime().agent().state().messages[0], stored);
}

#[tokio::test]
async fn new_session_options_preserve_the_parent_session_id() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("child.jsonl");
    let session = AgentSession::create(
        scripted_runtime([]),
        &path,
        AgentSessionOptions {
            parent_session_id: Some("parent-id".to_string()),
            ..AgentSessionOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        session.store().header().parent_session_id.as_deref(),
        Some("parent-id")
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn new_session_options_preserve_an_exact_session_id_without_materializing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("exact-id.jsonl");
    let session = AgentSession::create(
        scripted_runtime([]),
        &path,
        AgentSessionOptions {
            session_id: Some("botmux-session".to_string()),
            ..AgentSessionOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(session.store().header().id, "botmux-session");
    session
        .set_name(Some("BotMux task".to_string()))
        .await
        .unwrap();
    assert_eq!(session.store().name().as_deref(), Some("BotMux task"));
    assert!(!path.exists());
}

#[tokio::test]
async fn summarized_tree_navigation_uses_a_standalone_provider_completion() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime([
        ScriptedTurn::Text("root answer".to_string()),
        ScriptedTurn::Text("branch answer".to_string()),
        ScriptedTurn::Text("## Goal\nPreserve the branch".to_string()),
    ]);
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    session.prompt("root").await.unwrap();
    let target = session.store().leaf_id().unwrap();
    session.prompt("branch work").await.unwrap();

    let summary_id = session
        .summarize_branch_and_checkout(
            &target,
            Some("Focus on decisions".to_string()),
            false,
            Some("abandoned work".to_string()),
        )
        .await
        .unwrap();
    let document = session.store().load().unwrap();
    assert_eq!(
        document.labels.get(&summary_id).map(String::as_str),
        Some("abandoned work")
    );
    assert!(document.entries.iter().any(|record| {
        record.id == summary_id
            && matches!(&record.entry, SessionEntry::BranchSummary(summary)
                if summary.summary.starts_with(
                    "The user explored a different conversation branch"
                ) && summary.summary.contains("Preserve the branch"))
    }));
}

struct ExpandingCommandPlugin;

struct BeforeStartInjectionPlugin;

#[test]
fn tree_preparation_collects_only_the_abandoned_branch_in_chronological_order() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::create(
        directory.path().join("session.jsonl"),
        SessionHeader::new("session", directory.path()),
    )
    .unwrap();
    let root = store
        .append_message(Message::User(UserMessage::text("root", 1)))
        .unwrap();
    let common = store
        .append_message(Message::User(UserMessage::text("common", 2)))
        .unwrap();
    let abandoned = [
        store
            .append_message(Message::User(UserMessage::text("abandoned one", 3)))
            .unwrap(),
        store
            .append_message(Message::User(UserMessage::text("abandoned two", 4)))
            .unwrap(),
    ];
    store.create_lane("target", Some(&common)).unwrap();
    let target = store
        .append_to_lane(
            SessionEntry::message(Message::User(UserMessage::text("target", 5))),
            "target",
        )
        .unwrap();

    let preparation = tree_preparation(&store.load().unwrap(), Some(&target), true).unwrap();
    assert_eq!(
        preparation.common_ancestor_id.as_deref(),
        Some(common.as_str())
    );
    assert_eq!(
        preparation
            .entries_to_summarize
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        abandoned.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(
        !preparation
            .entries_to_summarize
            .iter()
            .any(|entry| entry.id == root)
    );
    assert!(preparation.user_wants_summary);

    let empty_log = SessionStore::create(
        directory.path().join("empty.jsonl"),
        SessionHeader::new("empty", directory.path()),
    )
    .unwrap();
    empty_log.create_lane("target", None).unwrap();
    let only_target = empty_log
        .append_to_lane(
            SessionEntry::message(Message::User(UserMessage::text("target", 1))),
            "target",
        )
        .unwrap();
    let empty = tree_preparation(&empty_log.load().unwrap(), Some(&only_target), true).unwrap();
    assert!(empty.entries_to_summarize.is_empty());
    assert!(empty.common_ancestor_id.is_none());
}

#[test]
fn branch_summary_reserve_keeps_newest_messages_within_model_budget() {
    let selected = branch_summary_input_messages(
        vec![
            AgentMessage::from(Message::User(UserMessage::text("old ".repeat(200), 1))),
            AgentMessage::from(Message::User(UserMessage::text("new", 2))),
        ],
        100,
        90,
    );

    assert_eq!(selected.len(), 1);
    assert!(matches!(
        selected[0].as_standard(),
        Some(Message::User(user))
            if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "new")
    ));
}

#[pi_plugin::plugin]
impl Plugin for BeforeStartInjectionPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("before-start-injection")
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!("{}|injected", event.system_prompt)),
            messages: vec![Message::custom(CustomMessage {
                custom_type: "fixture-context".to_string(),
                content: CustomMessageContent::Text("injected context".to_string()),
                display: false,
                details: Some(serde_json::json!({"source": "fixture"})),
                timestamp_ms: 1,
            })],
        })
    }
}

#[pi_plugin::plugin]
impl Plugin for ExpandingCommandPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("expanding-command")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_command(Arc::new(ExpandingCommand))
    }
}

#[tokio::test]
async fn before_agent_start_custom_messages_follow_the_user_and_persist_as_custom_entries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("before-start.jsonl");
    let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .plugin(BeforeStartInjectionPlugin)
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();

    session.prompt("hello").await.unwrap();

    let requests = provider.requests();
    assert!(requests[0].system_prompt.ends_with("|injected"));
    assert!(matches!(
        &requests[0].messages[..2],
        [Message::User(user), Message::User(injected)]
            if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "hello")
                && matches!(&injected.content[0], ContentBlock::Text(text) if text.text == "injected context")
    ));

    let (_, document) = SessionStore::open(&path).unwrap();
    let branch = document.branch().unwrap();
    let conversation = branch
        .into_iter()
        .filter(|record| {
            matches!(
                &record.entry,
                SessionEntry::Message(_) | SessionEntry::CustomMessage(_)
            )
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        &conversation[0].entry,
        SessionEntry::Message(message) if message.message.role() == "user"
    ));
    assert!(matches!(
        &conversation[1].entry,
        SessionEntry::CustomMessage(message)
            if message.custom_type == "fixture-context"
                && message.details == Some(serde_json::json!({"source": "fixture"}))
    ));
    assert!(matches!(
        &session.runtime().agent().state().messages[1],
        Message::Custom(message) if message.custom_type == "fixture-context"
    ));
}

#[tokio::test]
async fn session_input_images_reach_the_provider_and_persist() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("multimodal.jsonl");
    let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();
    let image = ImageContent {
        data: "aW1hZ2U=".to_string(),
        mime_type: "image/png".to_string(),
    };

    session
        .prompt(SessionInput::new("inspect").with_images([image.clone()]))
        .await
        .unwrap();

    assert!(matches!(
        &provider.requests()[0].messages[0],
        Message::User(user)
            if matches!(&user.content[..], [ContentBlock::Text(text), ContentBlock::Image(actual)]
                if text.text == "inspect" && actual == &image)
    ));
    let document = session.store().load().unwrap();
    assert!(matches!(
        document.messages()[0].as_standard(),
        Some(Message::User(user))
            if matches!(&user.content[..], [ContentBlock::Text(text), ContentBlock::Image(actual)]
                if text.text == "inspect" && actual == &image)
    ));
}

#[tokio::test]
async fn message_end_replacements_are_the_messages_persisted_and_restored() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let runtime = PiRuntime::builder()
        .plugin(PersistMessageEndReplacement)
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "provider assistant".to_string(),
        )]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();

    session.prompt("submitted user").await.unwrap();
    drop(session);

    let (_, document) = SessionStore::open(&path).unwrap();
    let messages = document.context().unwrap().runtime_messages();
    assert!(matches!(&messages[0], Message::User(user)
        if matches!(&user.content[0], ContentBlock::Text(text)
            if text.text == "persisted user")));
    assert!(matches!(&messages[1], Message::Assistant(assistant)
        if matches!(&assistant.content[0], ContentBlock::Text(text)
            if text.text == "persisted assistant")));

    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.runtime().agent().state().messages.as_ref(),
        &messages
    );
}

#[tokio::test]
async fn agent_settled_hooks_finish_before_the_product_event_and_isolate_failures() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent-settled.jsonl");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let runtime = PiRuntime::builder()
        .plugin(FailingAgentSettledPlugin)
        .plugin(BlockingAgentSettledPlugin {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "done".to_string(),
        )]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();
    let mut subscription = session.subscribe();

    let submit = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("hello").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    while let Ok(event) = subscription.events.try_recv() {
        assert!(!matches!(event.event, AgentSessionEvent::AgentSettled));
    }

    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), submit)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut saw_settled = false;
    while let Ok(event) = subscription.events.try_recv() {
        saw_settled |= matches!(event.event, AgentSessionEvent::AgentSettled);
    }
    assert!(saw_settled);
    assert!(
        session
            .runtime()
            .plugin_diagnostics()
            .iter()
            .any(|diagnostic| {
                diagnostic.plugin_id == PluginId::new("failing-agent-settled")
                    && diagnostic.hook.as_str() == "agent_settled"
                    && diagnostic.message.contains("intentional settled failure")
            })
    );
}

struct ExpandingCommand;

struct ExplicitCommandPlugin(Arc<AtomicUsize>);
struct ExplicitCommand(Arc<AtomicUsize>);

#[pi_plugin::plugin]
impl Plugin for ExplicitCommandPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("explicit-command")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_command(Arc::new(ExplicitCommand(Arc::clone(&self.0))))
    }
}

#[async_trait]
impl Command for ExplicitCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "ui-action".into(),
            description: "Execute a plugin action".into(),
            argument_hint: None,
        }
    }

    async fn execute(
        &self,
        _context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(if arguments == "transform" {
            CommandOutcome::TransformInput("/ui-action literal model input".into())
        } else {
            CommandOutcome::Handled
        })
    }
}

fn explicit_command_runtime(calls: Arc<AtomicUsize>, turn: ScriptedTurn) -> PiRuntime {
    PiRuntime::builder()
        .plugin(ExplicitCommandPlugin(calls))
        .provider_plugin(ScriptedProviderPlugin::scripted([turn]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap()
}

#[tokio::test]
async fn explicit_commands_run_immediately_during_active_turn_without_queuing_or_prompting() {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let session = AgentSession::create(
        explicit_command_runtime(Arc::clone(&calls), ScriptedTurn::WaitForAbort),
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("parent working").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !session.snapshot().agent.is_running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        session.invoke_command("ui-action", "stop child"),
    )
    .await
    .expect("a control must not wait for the parent turn")
    .unwrap();
    assert!(matches!(outcome, PromptOutcome::Handled));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(session.snapshot().queue.steering.is_empty());
    assert!(
        session
            .invoke_command("missing", "never prompt this")
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.snapshot().agent.messages.len(), 1);

    let outcome = session
        .invoke_command("ui-action", "transform")
        .await
        .unwrap();
    assert!(matches!(outcome, PromptOutcome::Queued { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.snapshot().queue.steering, ["/ui-action transform"]);
    session.abort();
    running.await.unwrap().unwrap();
    session.shutdown().await;
    assert!(matches!(
        session.invoke_command("ui-action", "stop child").await,
        Err(SessionError::Closed)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn explicit_command_transform_runs_once_and_unknown_idle_command_is_not_model_input() {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let session = AgentSession::create(
        explicit_command_runtime(Arc::clone(&calls), ScriptedTurn::Text("done".into())),
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    assert!(
        session
            .invoke_command("missing", "never prompt this")
            .await
            .is_err()
    );
    assert!(session.snapshot().agent.messages.is_empty());
    assert!(!session.store().path().exists());
    session
        .invoke_command("ui-action", "transform")
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let document = session.store().load().unwrap();
    let user = document
        .messages()
        .into_iter()
        .find(|message| message.role() == "user")
        .unwrap();
    assert_eq!(user.display_text(), Some("/ui-action transform"));
    assert!(matches!(user.as_standard(), Some(Message::User(message))
        if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "/ui-action literal model input")));
    session.shutdown().await;
}

#[async_trait]
impl Command for ExpandingCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "review".to_string(),
            description: "Expand a review request".to_string(),
            argument_hint: Some("[focus]".to_string()),
        }
    }

    async fn execute(
        &self,
        _context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        Ok(CommandOutcome::TransformInput(format!(
            "Run the private review prompt for {}",
            arguments.trim()
        )))
    }
}

#[tokio::test]
async fn transformed_command_persists_model_text_and_original_display_text() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let runtime = PiRuntime::builder()
        .plugin(ExpandingCommandPlugin)
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "done".to_string(),
        )]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();

    session.prompt("/review accessibility").await.unwrap();

    let (_, document) = SessionStore::open(&path).unwrap();
    let user = document
        .messages()
        .into_iter()
        .find(|message| message.role() == "user")
        .unwrap();
    assert!(matches!(
        user.as_standard(),
        Some(Message::User(message))
            if matches!(&message.content[0], ContentBlock::Text(text)
                if text.text == "Run the private review prompt for accessibility")
    ));
    assert_eq!(user.display_text(), Some("/review accessibility"));
}

#[tokio::test]
async fn transformed_command_publishes_the_original_text_to_product_frontends() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = PiRuntime::builder()
        .plugin(ExpandingCommandPlugin)
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "done".to_string(),
        )]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe();

    session.prompt("/review accessibility").await.unwrap();

    let displayed = std::iter::from_fn(|| subscription.events.try_recv().ok()).find_map(|event| {
        match event.event {
            crate::AgentSessionEvent::Agent(event) => match *event {
                AgentEvent::MessageStart {
                    message: Message::User(user),
                } => Some(
                    user.content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            },
            _ => None,
        }
    });
    assert_eq!(displayed.as_deref(), Some("/review accessibility"));
}

#[tokio::test]
async fn queued_command_restores_the_original_text_after_process_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("queued-command.jsonl");
    let runtime = PiRuntime::builder()
        .plugin(ExpandingCommandPlugin)
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::WaitForAbort,
        ]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();
    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("first").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !session.snapshot().agent.is_running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    session.steer("/review accessibility").await.unwrap();
    assert_eq!(
        session.snapshot().queue.steering,
        vec!["/review accessibility".to_string()]
    );
    session.abort();
    running.await.unwrap().unwrap();
    session.shutdown().await;
    drop(session);

    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.snapshot().queue.follow_up,
        vec!["/review accessibility".to_string()]
    );
}

#[tokio::test]
async fn new_session_is_saved_only_after_the_first_assistant_message() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions/session.jsonl");
    let session = AgentSession::create(
        scripted_runtime([ScriptedTurn::Text("first answer".to_string())]),
        &path,
        AgentSessionOptions::default().shell_executor(Arc::new(FixtureShellExecutor)),
    )
    .await
    .unwrap();

    assert!(!path.exists());
    assert!(!session.store().is_materialized());
    session
        .execute_shell("printf shell-only", ShellExecutionOptions::default())
        .await
        .unwrap();
    assert!(!path.exists());
    assert_eq!(
        session.snapshot().agent.messages.as_ref(),
        &session.history().context().unwrap().runtime_messages()
    );
    assert!(!session.snapshot().agent.messages.is_empty());

    session.prompt("hello").await.unwrap();

    assert!(path.exists());
    assert!(session.store().is_materialized());
    let (_, document) = SessionStore::open(&path).unwrap();
    assert!(document.messages().iter().any(|message| {
        matches!(
            message.as_standard(),
            Some(Message::User(user))
                if matches!(
                    &user.content[0],
                    ContentBlock::Text(text) if text.text == "hello"
                )
        )
    }));
    assert!(
        document
            .messages()
            .iter()
            .any(|message| { matches!(message.as_standard(), Some(Message::Assistant(_))) })
    );
}

#[tokio::test]
async fn generic_session_requires_an_explicit_shell_executor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let session = AgentSession::create(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    let before = session.store().load().unwrap().entries.len();
    let error = session
        .execute_shell("ignored", ShellExecutionOptions::default())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("shell execution is not configured")
    );
    assert_eq!(session.store().load().unwrap().entries.len(), before);
    assert!(session.snapshot().bash.is_none());
    assert!(!path.exists());
}

struct FixtureShellExecutor;

#[async_trait::async_trait]
impl SessionShellExecutor for FixtureShellExecutor {
    async fn execute(&self, request: ShellRequest) -> Result<ShellResult, pi_shell::ShellError> {
        if let Some(on_chunk) = request.on_chunk {
            on_chunk(ShellChunk {
                stream: pi_shell::ShellStream::Stdout,
                text: "shell-only".to_string(),
            });
        }
        Ok(ShellResult {
            output: "shell-only".to_string(),
            exit_code: Some(0),
            cancelled: false,
            timed_out: false,
            truncated: false,
            truncation: None,
            full_output_path: None,
        })
    }
}

struct LifecyclePlugin {
    value: usize,
    contexts: Arc<Mutex<Vec<SessionPluginContext>>>,
    events: Arc<Mutex<Vec<String>>>,
    cancel_tree: bool,
    replace_compaction: bool,
}

impl LifecyclePlugin {
    fn record(&self, event: impl Into<String>) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.into());
    }

    fn record_context(&self, context: &SessionPluginContext) {
        self.contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(context.clone());
    }
}

#[pi_plugin::plugin]
impl Plugin for LifecyclePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("lifecycle")
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        event: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        self.record_context(context);
        self.record(format!("start:{}:{:?}", self.value, event.reason));
        Ok(())
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        event: &SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        self.record_context(context);
        self.record(format!("shutdown:{}:{:?}", self.value, event.reason));
        Ok(())
    }

    async fn session_before_tree(
        &self,
        context: &SessionPluginContext,
        _event: &SessionBeforeTreeEvent,
    ) -> Result<Option<SessionBeforeTreeResult>, PluginError> {
        self.record_context(context);
        self.record(format!("before_tree:{}", self.value));
        Ok(self.cancel_tree.then_some(SessionBeforeTreeResult {
            cancel: true,
            ..SessionBeforeTreeResult::default()
        }))
    }

    async fn session_before_compact(
        &self,
        context: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, PluginError> {
        self.record_context(context);
        self.record(format!("before_compact:{}", self.value));
        if !self.replace_compaction {
            return Ok(None);
        }
        let compaction = CompactionEntry {
            retained_entry_ids: Vec::new(),
            summary: "extension summary".to_string(),
            retained_tail: event.preparation.retained_tail.clone(),
            tokens_before: event.preparation.tokens_before,
            details: None,
            usage: None,
        };
        Ok(Some(SessionBeforeCompactResult {
            cancel: false,
            compaction: Some(compaction),
        }))
    }

    async fn session_compact(
        &self,
        context: &SessionPluginContext,
        event: &SessionCompactEvent,
    ) -> Result<(), PluginError> {
        self.record_context(context);
        self.record(format!(
            "compact:{}:{}:{}",
            self.value, event.compaction_entry.summary, event.from_extension
        ));
        Ok(())
    }
}

#[tokio::test]
async fn isolated_seed_is_deferred_and_does_not_bill_ancestor_responses() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = |tokens| {
        PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([text_turn_with_usage(
                "answer", tokens,
            )]))
            .agent_options(AgentOptions {
                provider_id: "scripted".into(),
                model_id: "test".into(),
                ..Default::default()
            })
            .build()
            .unwrap()
    };
    let parent = AgentSession::create(
        runtime(100),
        directory.path().join("parent.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    parent.prompt("parent request").await.unwrap();
    let small_path = directory.path().join("too-small.jsonl");
    let small = AgentSession::prepare_create(
        runtime(7),
        &small_path,
        AgentSessionOptions {
            context_window: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let error = small
        .session()
        .initialize_isolated_context(parent.isolated_context_seed(None).unwrap())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("compact the parent or use fresh")
    );
    assert!(!small_path.exists());
    assert!(
        small
            .session()
            .store()
            .append_session_record(SessionEntry::Custom(CustomEntry {
                custom_type: crate::history::ISOLATED_CONTEXT_TYPE.into(),
                data: Some(serde_json::json!({"messages": "corrupt"})),
            }))
            .is_err()
    );
    assert!(
        small
            .session()
            .store()
            .load()
            .unwrap()
            .context()
            .unwrap()
            .messages
            .is_empty()
    );
    let path = directory.path().join("child.jsonl");
    let prepared =
        AgentSession::prepare_create(runtime(7), &path, AgentSessionOptions::default()).unwrap();
    let child = prepared.session();
    child
        .initialize_isolated_context(parent.isolated_context_seed(None).unwrap())
        .unwrap();
    assert!(!path.exists());
    assert_eq!(
        crate::aggregate_document_usage(&child.store().load().unwrap()).total_tokens,
        0
    );
    assert_eq!(child.runtime().agent().state().messages.len(), 2);
    assert_eq!(child.snapshot().agent.messages.len(), 2);
    let document = child.store().load().unwrap();
    let context = document.context().unwrap();
    let inherited_estimate =
        crate::estimate_session_context_tokens(&document.entries, &context.messages);
    assert_eq!(inherited_estimate.usage_tokens, 0);
    assert!(crate::current_session_context_tokens(&document.entries, &context.messages).is_none());
    let preparation = prepare_compaction(
        &document.entries,
        CompactionSettings::default(),
        &Default::default(),
        &GenericCompactionPolicy,
    )
    .unwrap();
    let compacted_messages = preparation
        .messages_to_summarize
        .iter()
        .chain(&preparation.turn_prefix_messages)
        .chain(&preparation.retained_tail)
        .collect::<Vec<_>>();
    assert_eq!(
        compacted_messages,
        context.messages.iter().collect::<Vec<_>>()
    );
    let child = prepared
        .activate(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })
        .await;
    let pi_session::PromptOutcome::Agent(outcome) = child.prompt("child request").await.unwrap()
    else {
        panic!("expected an agent run");
    };
    assert_eq!(outcome.new_messages.len(), 2);
    assert!(path.exists());
    let document = crate::SessionStore::open(&path).unwrap().1;
    assert_eq!(document.context().unwrap().messages.len(), 4);
    assert_eq!(crate::aggregate_document_usage(&document).total_tokens, 7);
    assert_eq!(
        crate::aggregate_document_usage(&parent.store().load().unwrap()).total_tokens,
        100
    );
    child.shutdown().await;
    parent.shutdown().await;
}

#[tokio::test]
async fn create_records_only_v4_configuration_and_messages() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "done".to_string(),
        )]))
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::High,
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();

    let pi_session::PromptOutcome::Agent(outcome) = session.prompt("hello").await.unwrap() else {
        panic!("expected an agent run");
    };
    assert_eq!(outcome.new_messages.len(), 2);
    let document = session.store().load().unwrap();
    assert_eq!(document.header.version, 4);
    assert_eq!(document.messages().len(), 2);
    assert_eq!(document.context().unwrap().thinking_level, "high");

    let values = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values[0]["version"], 4);
    assert_eq!(values[1]["type"], "model_change");
    assert_eq!(values[2]["type"], "thinking_level_change");
    assert_eq!(values[3]["type"], "active_tools_change");
}

#[tokio::test]
async fn prompt_emits_typed_product_run_and_provider_request_spans() {
    let directory = tempfile::tempdir().unwrap();
    let sink = Arc::new(pi_telemetry::InMemoryTelemetrySink::default());
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "done".to_string(),
        )]))
        .agent_options(AgentOptions {
            telemetry: pi_telemetry::TelemetryContext::new(sink.clone()),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("telemetry.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();

    session.prompt("trace me").await.unwrap();

    let records = sink.records();
    assert_eq!(records.len(), 4);
    assert!(matches!(
        &records[0],
        pi_telemetry::TelemetryRecord::Start { name, attributes, .. }
            if name == "pi.harness.run"
                && attributes["pi.operation.kind"] == "run"
                && attributes["pi.operation.recovery"] == false
    ));
    assert!(matches!(
        &records[1],
        pi_telemetry::TelemetryRecord::Start { name, .. } if name == "pi.ai.request"
    ));
    assert!(matches!(
        &records[2],
        pi_telemetry::TelemetryRecord::End { name, .. } if name == "pi.ai.request"
    ));
    assert!(matches!(
        &records[3],
        pi_telemetry::TelemetryRecord::End { name, attributes, status, .. }
            if name == "pi.harness.run"
                && attributes["pi.operation.outcome"] == "completed"
                && *status == pi_telemetry::SpanStatus::Ok
    ));
}

#[tokio::test]
async fn durable_queue_revisions_survive_abort_and_process_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("queued-session.jsonl");
    let session = Arc::new(
        AgentSession::create(
            scripted_runtime([ScriptedTurn::WaitForAbort]),
            &path,
            Default::default(),
        )
        .await
        .unwrap(),
    );
    let mut subscription = session.subscribe();
    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("first").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !session.snapshot().agent.is_running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let queued_image = ImageContent {
        data: "cXVldWVk".to_string(),
        mime_type: "image/webp".to_string(),
    };
    let queued = session
        .steer(SessionInput::new("keep this draft").with_images([queued_image.clone()]))
        .await
        .unwrap();
    assert!(matches!(
        queued,
        PromptOutcome::Queued {
            kind: QueueKind::Steer,
            ..
        }
    ));
    assert_eq!(
        session.snapshot().queue.steering,
        vec!["keep this draft".to_string()]
    );
    let document = session.store().load().unwrap();
    let queued_message = document.records.iter().find_map(|record| {
        let LaneRecordEntry::QueueEnqueued { target, .. } = &record.record else {
            return None;
        };
        let SessionEntry::Message(message) = &target.entry else {
            return None;
        };
        message.message.as_standard()
    });
    assert!(matches!(
        queued_message,
        Some(Message::User(user))
            if matches!(&user.content[..], [ContentBlock::Text(text), ContentBlock::Image(actual)]
                if text.text == "keep this draft" && actual == &queued_image)
    ));
    session.abort();
    let outcome = running.await.unwrap().unwrap();
    assert!(matches!(outcome, PromptOutcome::Agent(_)));
    assert_eq!(
        session.snapshot().queue.steering,
        vec!["keep this draft".to_string()]
    );

    let mut revisions = Vec::new();
    let mut saw_queue = false;
    let mut saw_settled = false;
    while let Ok(event) = subscription.events.try_recv() {
        if event.revision <= subscription.snapshot.revision {
            continue;
        }
        revisions.push(event.revision);
        saw_queue |= matches!(event.event, crate::AgentSessionEvent::QueueUpdate { .. });
        saw_settled |= matches!(event.event, crate::AgentSessionEvent::AgentSettled);
    }
    assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(saw_queue && saw_settled);
    session.shutdown().await;
    drop(session);

    let reopened = AgentSession::open(scripted_runtime([]), &path, Default::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.snapshot().queue.follow_up,
        vec!["keep this draft".to_string()]
    );
    let restored = reopened.clear_queue().unwrap();
    assert_eq!(restored.follow_up, vec!["keep this draft".to_string()]);
    assert!(reopened.snapshot().queue.follow_up.is_empty());
    let document = reopened.store().load().unwrap();
    crate::validate_record_log(&crate::RecordLogSlice {
        lane: MAIN_LANE.to_string(),
        open_operations: reopened
            .store()
            .find_open_operations(MAIN_LANE, None)
            .unwrap(),
        records: document.records,
        entries: document.entries,
    })
    .unwrap();
}

#[tokio::test]
async fn clearing_multiple_queue_items_is_durable_and_failed_clears_publish_no_update() {
    for fail in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.jsonl");
        let session = AgentSession::create(
            scripted_runtime([ScriptedTurn::Text("saved".into())]),
            &path,
            Default::default(),
        )
        .await
        .unwrap();
        session.prompt("start").await.unwrap();
        for index in 0..4 {
            session
                .enqueue_message(
                    Message::User(UserMessage::text(format!("queued-{index}"), index)),
                    QueueKind::NextRun,
                )
                .unwrap();
        }
        let before = session.snapshot().queue.clone();
        let before_clear = std::fs::read(&path).unwrap();
        let mut updates = session.subscribe();
        let recovery_path = if fail {
            let moved = directory.path().join("original.jsonl");
            std::fs::rename(&path, &moved).unwrap();
            std::fs::write(&path, "replacement\n").unwrap();
            moved
        } else {
            path.clone()
        };
        let result = session.clear_queue();
        let queue_updates = std::iter::from_fn(|| updates.events.try_recv().ok())
            .filter(|update| matches!(update.event, crate::AgentSessionEvent::QueueUpdate { .. }))
            .count();
        if fail {
            assert!(result.is_err());
            assert_eq!(session.snapshot().queue.follow_up, before.follow_up);
            assert_eq!(queue_updates, 0);
        } else {
            assert_eq!(result.unwrap().follow_up, before.follow_up);
            assert!(session.snapshot().queue.follow_up.is_empty());
            assert_eq!(queue_updates, 1);
            let cancellations = session
                .store()
                .load()
                .unwrap()
                .records
                .into_iter()
                .filter(|record| matches!(record.record, LaneRecordEntry::QueueCancelled { .. }))
                .collect::<Vec<_>>();
            assert_eq!(cancellations.len(), 4);
            assert!(
                cancellations
                    .windows(2)
                    .all(|pair| pair[0].seq + 1 == pair[1].seq)
            );
            // One write is not a filesystem transaction. Recovery must accept
            // only the complete records preceding a torn batch suffix.
            let committed = std::fs::read(&path).unwrap();
            let mut prefix = before_clear;
            prefix.extend(
                committed[prefix.len()..]
                    .split_inclusive(|byte| *byte == b'\n')
                    .take(2)
                    .flatten(),
            );
            prefix.extend_from_slice(b"{\"kind\":\"record\",\"seq\":");
            let partial_path = directory.path().join("partial-clear.jsonl");
            std::fs::write(&partial_path, prefix).unwrap();
            let partial =
                AgentSession::open(scripted_runtime([]), &partial_path, Default::default())
                    .await
                    .unwrap();
            assert_eq!(partial.snapshot().queue.follow_up, before.follow_up[2..]);
            assert_eq!(
                partial.clear_queue().unwrap().follow_up,
                before.follow_up[2..]
            );
            partial.shutdown().await;
        }
        let restored = AgentSession::open(scripted_runtime([]), &recovery_path, Default::default())
            .await
            .unwrap();
        assert_eq!(
            restored.snapshot().queue.follow_up.len(),
            if fail { 4 } else { 0 }
        );
        restored.shutdown().await;
        session.shutdown().await;
    }
}

#[tokio::test]
async fn open_reconciles_interrupted_run_from_reducer_without_replaying_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interrupted-session.jsonl");
    let store = SessionStore::create(
        &path,
        SessionHeader::new("interrupted-session", directory.path()),
    )
    .unwrap();
    let user = AgentMessage::from(Message::User(UserMessage::text("recover me", 1)));
    let initial = ProvisionedEntry {
        id: "initial-user".to_string(),
        entry: SessionEntry::message(user.clone()),
    };
    store
        .append_record(NewLaneRecord {
            id: "interrupted-run".to_string(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: None,
                intent: OperationIntent::Run {
                    original_prompt: vec![user],
                    initial_messages: vec![initial],
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        })
        .unwrap();
    store
        .append_record(NewLaneRecord {
            id: "accepted-write".to_string(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::WriteDeferred {
                run_id: "interrupted-run".to_string(),
                target: ProvisionedEntry {
                    id: "accepted-entry".to_string(),
                    entry: SessionEntry::Custom(CustomEntry {
                        custom_type: "accepted".to_string(),
                        data: Some(serde_json::json!({ "value": true })),
                    }),
                },
            },
        })
        .unwrap();
    drop(store);

    let reopened = AgentSession::open(
        scripted_runtime_in([], directory.path()),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.snapshot().queue.follow_up,
        vec!["recover me".to_string()]
    );
    let document = reopened.store().load().unwrap();
    assert!(
        document
            .entries
            .iter()
            .any(|entry| entry.id == "accepted-entry")
    );
    assert!(
        reopened
            .store()
            .find_open_operations(MAIN_LANE, None)
            .unwrap()
            .is_empty()
    );
    let interrupted_finishes = document
        .records
        .iter()
        .filter(|record| {
            matches!(
                &record.record,
                LaneRecordEntry::OperationFinished {
                    run_id,
                    outcome: OperationOutcome::Aborted,
                    error: Some(OperationError { code, .. }),
                } if run_id == "interrupted-run" && code == "interrupted"
            )
        })
        .count();
    assert_eq!(interrupted_finishes, 1);
    reopened.shutdown().await;
    drop(reopened);

    let reopened_again = AgentSession::open(
        scripted_runtime_in([], directory.path()),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened_again.snapshot().queue.follow_up,
        vec!["recover me".to_string()]
    );
    assert_eq!(
        reopened_again
            .store()
            .load()
            .unwrap()
            .records
            .iter()
            .filter(|record| matches!(
                &record.record,
                LaneRecordEntry::OperationFinished { run_id, .. }
                    if run_id == "interrupted-run"
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn open_and_checkout_restore_the_selected_v4_branch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let runtime = PiRuntime::builder()
        .plugin(TestToolsPlugin::new())
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("first answer".to_string()),
            ScriptedTurn::Text("abandoned answer".to_string()),
        ]))
        .agent_options(AgentOptions {
            active_tools: vec!["echo".to_string()],
            cwd: directory.path().to_path_buf(),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();
    session.prompt("first").await.unwrap();
    let first_assistant = session
        .store()
        .find_entries(&EntryQuery {
            entry_type: Some(SessionEntryType::Message),
            order: EntryOrder::OldestFirst,
            ..EntryQuery::default()
        })
        .unwrap()
        .into_iter()
        .find(|record| {
            matches!(
                &record.entry,
                SessionEntry::Message(message)
                    if matches!(message.message.as_standard(), Some(Message::Assistant(_)))
            )
        })
        .unwrap()
        .id;
    session.prompt("abandoned").await.unwrap();
    let context = session.checkout(Some(&first_assistant)).await.unwrap();
    assert_eq!(context.messages.len(), 2);

    drop(session);
    let runtime = PiRuntime::builder()
        .plugin(TestToolsPlugin::new())
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .agent_options(AgentOptions {
            active_tools: vec!["echo".to_string()],
            cwd: directory.path().to_path_buf(),
            ..AgentOptions::default()
        })
        .system_prompt(generic_system_prompt())
        .build()
        .unwrap();
    let reopened = AgentSession::open(runtime, &path, Default::default())
        .await
        .unwrap();
    assert_eq!(reopened.runtime().agent().state().messages.len(), 2);
    assert_eq!(
        reopened.store().leaf_id().as_deref(),
        Some(first_assistant.as_str())
    );
}

#[tokio::test]
async fn compaction_uses_persisted_retained_tail() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    session
        .store()
        .append_message(Message::User(UserMessage::text("old", 1)))
        .unwrap();
    session
        .append_compaction(CompactionEntry {
            retained_entry_ids: Vec::new(),
            summary: "short".to_string(),
            retained_tail: vec![Message::User(UserMessage::text("retained", 2)).into()],
            tokens_before: 100,
            details: None,
            usage: None,
        })
        .await
        .unwrap();
    let messages = &session.runtime().agent().state().messages;
    assert_eq!(messages.len(), 2);
    assert!(matches!(&messages[0], Message::User(user)
        if matches!(&user.content[0], ContentBlock::Text(text) if text.text.contains("short"))));
}

#[tokio::test]
async fn lifecycle_hooks_can_replace_compaction_and_cancel_tree_navigation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let builder = PiRuntime::builder().provider_plugin(ScriptedProviderPlugin::scripted([]));
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let runtime = builder
        .plugin(LifecyclePlugin {
            value: 1,
            contexts,
            events: Arc::clone(&events),
            cancel_tree: true,
            replace_compaction: true,
        })
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, AgentSessionOptions::default())
        .await
        .unwrap();
    let leaf = session
        .store()
        .append_message(Message::User(UserMessage::text("branch", 1)))
        .unwrap();

    let error = session.checkout(None).await.unwrap_err();
    assert!(matches!(error, SessionError::Cancelled(_)));
    assert_eq!(session.store().leaf_id().as_deref(), Some(leaf.as_str()));

    session
        .append_compaction(CompactionEntry {
            retained_entry_ids: Vec::new(),
            summary: "default summary".to_string(),
            retained_tail: Vec::new(),
            tokens_before: 10,
            details: None,
            usage: None,
        })
        .await
        .unwrap();
    let document = session.store().load().unwrap();
    assert!(document.entries.iter().any(|record| {
        matches!(
            &record.entry,
            SessionEntry::Compaction(entry) if entry.summary == "extension summary"
        )
    }));
    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![
            "start:1:Startup".to_string(),
            "before_tree:1".to_string(),
            "before_compact:1".to_string(),
            "compact:1:extension summary:true".to_string(),
        ]
    );
}

#[tokio::test]
async fn new_session_preserves_a_custom_model_outside_the_catalog() {
    struct Catalog;

    #[pi_plugin::provider_plugin]
    impl pi_plugin::ProviderPlugin for Catalog {
        fn id(&self) -> pi_core::PluginId {
            pi_core::PluginId::new("catalog")
        }

        fn register(
            &self,
            context: &mut pi_plugin::ProviderRegisterContext<'_>,
        ) -> pi_plugin::Result<()> {
            context.register_model(pi_core::ModelSpec::new(
                "scripted",
                "registered",
                "Registered",
                "test",
            ))
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .provider_plugin(Catalog)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("unlisted"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();

    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let state = session.runtime().agent().state();

    assert_eq!(state.provider_id.as_str(), "scripted");
    assert_eq!(state.model_id.as_str(), "unlisted");
}

#[tokio::test]
async fn active_context_window_tracks_catalog_model_switches() {
    struct Catalog;

    #[pi_plugin::provider_plugin]
    impl pi_plugin::ProviderPlugin for Catalog {
        fn id(&self) -> pi_core::PluginId {
            pi_core::PluginId::new("catalog")
        }

        fn register(
            &self,
            context: &mut pi_plugin::ProviderRegisterContext<'_>,
        ) -> pi_plugin::Result<()> {
            let mut small = pi_core::ModelSpec::new("scripted", "small", "Small", "test");
            small.context_window = 100;
            context.register_model(small)?;
            let mut large = pi_core::ModelSpec::new("scripted", "large", "Large", "test");
            large.context_window = 1_000;
            context.register_model(large)
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .provider_plugin(Catalog)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("small"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();

    assert_eq!(session.active_context_window(), Some(100));
    session
        .set_model(ProviderId::new("scripted"), ModelId::new("large"))
        .unwrap();
    assert_eq!(session.active_context_window(), Some(1_000));
}

#[tokio::test]
async fn thinking_changes_and_model_switches_clamp_to_catalog_capabilities() {
    struct Catalog;

    #[pi_plugin::provider_plugin]
    impl pi_plugin::ProviderPlugin for Catalog {
        fn id(&self) -> pi_core::PluginId {
            pi_core::PluginId::new("thinking-catalog")
        }

        fn register(
            &self,
            context: &mut pi_plugin::ProviderRegisterContext<'_>,
        ) -> pi_plugin::Result<()> {
            let mut sparse = pi_core::ModelSpec::new("scripted", "sparse", "Sparse", "test");
            sparse.reasoning = true;
            sparse.thinking_level_map.insert("off".to_string(), None);
            sparse
                .thinking_level_map
                .insert("minimal".to_string(), None);
            sparse.thinking_level_map.insert("low".to_string(), None);
            sparse.thinking_level_map.insert("medium".to_string(), None);
            context.register_model(sparse)?;
            context.register_model(pi_core::ModelSpec::new(
                "scripted", "plain", "Plain", "test",
            ))
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .provider_plugin(Catalog)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("sparse"),
            thinking_level: ThinkingLevel::High,
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();

    session.set_thinking_level(ThinkingLevel::Low).unwrap();
    assert_eq!(
        session.runtime().agent().state().thinking_level,
        ThinkingLevel::High
    );

    session
        .set_model(ProviderId::new("scripted"), ModelId::new("plain"))
        .unwrap();
    assert_eq!(
        session.runtime().agent().state().thinking_level,
        ThinkingLevel::Off
    );
    let document = session.store().load().unwrap();
    let thinking_entries = document
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::ThinkingLevelChange(entry) => Some(entry.thinking_level.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(thinking_entries, ["high", "off"]);
}

#[tokio::test]
async fn manual_compaction_uses_an_isolated_tool_free_completion() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("answer".to_string()),
        ScriptedTurn::Text("## Original Request\nKeep going".to_string()),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        }),
    )
    .await
    .unwrap();

    session.prompt("do the work").await.unwrap();
    let compaction = session.compact(None).await.unwrap();

    assert!(compaction.summary.contains("Keep going"));
    assert!(!compaction.retained_tail.is_empty());
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].system_prompt, SUMMARIZATION_SYSTEM_PROMPT);
    assert_eq!(
        requests[1].max_output_tokens,
        Some(CompactionSettings::default().reserve_tokens / 2)
    );
    assert!(
        session
            .store()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
    );
}

#[tokio::test]
async fn manual_compaction_uses_active_model_reasoning_and_output_limits() {
    struct LimitedCatalog;

    #[pi_plugin::provider_plugin]
    impl pi_plugin::ProviderPlugin for LimitedCatalog {
        fn id(&self) -> pi_core::PluginId {
            pi_core::PluginId::new("limited-catalog")
        }

        fn register(
            &self,
            context: &mut pi_plugin::ProviderRegisterContext<'_>,
        ) -> pi_plugin::Result<()> {
            let mut model = pi_core::ModelSpec::new("scripted", "limited", "Limited", "test");
            model.reasoning = false;
            model.max_tokens = 32;
            context.register_model(model)
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("answer".to_string()),
        ScriptedTurn::Text("## Original Request\nKeep going".to_string()),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .provider_plugin(LimitedCatalog)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("limited"),
            thinking_level: ThinkingLevel::High,
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            reserve_tokens: 1_000,
            keep_recent_tokens: 1,
            enabled: true,
        }),
    )
    .await
    .unwrap();

    session.prompt("do the work").await.unwrap();
    session.compact(None).await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].max_output_tokens, Some(32));
    assert_eq!(requests[1].thinking_level, ThinkingLevel::Off);
}

#[tokio::test]
async fn threshold_compaction_runs_after_a_completed_turn() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        text_turn_with_usage("large answer", 200),
        ScriptedTurn::Text("threshold summary".to_string()),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let settings = CompactionSettings {
        reserve_tokens: 10,
        keep_recent_tokens: 1,
        ..CompactionSettings::default()
    };
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default()
            .compaction(settings)
            .context_window(100),
    )
    .await
    .unwrap();

    let pi_session::PromptOutcome::Agent(outcome) = session.prompt("large request").await.unwrap()
    else {
        panic!("expected an agent run");
    };

    assert_eq!(outcome.stop, AgentLoopStop::Completed);
    assert_eq!(provider.requests().len(), 2);
    assert!(
        session
            .store()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Compaction(entry) if entry.summary.contains("threshold summary")
                )
            })
    );
}

#[tokio::test]
async fn overflow_compaction_drops_the_failed_assistant_and_retries_once() {
    let directory = tempfile::tempdir().unwrap();
    let agent_ends = Arc::new(AtomicUsize::new(0));
    let agent_settled = Arc::new(AtomicUsize::new(0));
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Error("context window token length exceeded".to_string()),
        ScriptedTurn::Text("overflow summary".to_string()),
        ScriptedTurn::Text("recovered answer".to_string()),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .plugin(CountingAgentLifecyclePlugin {
            ends: Arc::clone(&agent_ends),
            settled: Arc::clone(&agent_settled),
        })
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        }),
    )
    .await
    .unwrap();

    let pi_session::PromptOutcome::Agent(outcome) =
        session.prompt("recover this request").await.unwrap()
    else {
        panic!("expected an agent run");
    };

    assert_eq!(outcome.stop, AgentLoopStop::Completed);
    assert_eq!(provider.requests().len(), 3);
    assert_eq!(agent_ends.load(Ordering::SeqCst), 2);
    assert_eq!(agent_settled.load(Ordering::SeqCst), 1);
    assert!(
        matches!(outcome.final_context.messages.last(), Some(Message::Assistant(message))
        if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "recovered answer"))
    );
    let document = session.store().load().unwrap();
    let compaction = document
        .entries
        .iter()
        .find_map(|record| match &record.entry {
            SessionEntry::Compaction(entry) => Some(entry),
            _ => None,
        });
    assert!(compaction.is_some());
    assert!(compaction.unwrap().retained_tail.iter().all(|message| {
        !matches!(message.as_standard(), Some(Message::Assistant(assistant)) if assistant.stop_reason == StopReason::Error)
    }));
}

#[tokio::test]
async fn transient_provider_failure_retries_without_reprojecting_failed_assistant() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Error("503 service unavailable".to_string()),
        ScriptedTurn::Text("recovered".to_string()),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().retry(AutoRetrySettings {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 0,
        }),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe();

    let pi_session::PromptOutcome::Agent(outcome) = session.prompt("recover this").await.unwrap()
    else {
        panic!("expected an agent run");
    };

    assert_eq!(outcome.stop, AgentLoopStop::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().all(|message| {
        !matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Error)
    }));
    let messages = session
        .store()
        .load()
        .unwrap()
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Message(message) => message.message.as_standard(),
            _ => None,
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(messages.iter().any(|message| {
        matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Error)
    }));
    assert!(messages.iter().any(|message| {
        matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Stop)
    }));

    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut agent_ends = Vec::new();
    while let Ok(event) = subscription.events.try_recv() {
        match event.event {
            AgentSessionEvent::AgentEnd { will_retry, .. } => {
                agent_ends.push(will_retry);
            }
            AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                ..
            } => starts.push((attempt, max_attempts, delay_ms)),
            AgentSessionEvent::AutoRetryEnd {
                success, attempt, ..
            } => ends.push((success, attempt)),
            _ => {}
        }
    }
    assert_eq!(starts, vec![(1, 3, 0)]);
    assert_eq!(ends, vec![(true, 1)]);
    assert_eq!(agent_ends, vec![true, false]);
    assert!(session.snapshot().auto_retry.is_none());
}

#[tokio::test]
async fn quota_failure_is_not_retried() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Error(
        "429 insufficient_quota: check billing".to_string(),
    )]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().retry(AutoRetrySettings {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 0,
        }),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe();

    let pi_session::PromptOutcome::Agent(outcome) =
        session.prompt("do not retry quota").await.unwrap()
    else {
        panic!("expected an agent run");
    };

    assert_eq!(outcome.stop, AgentLoopStop::ProviderError);
    assert_eq!(provider.requests().len(), 1);
    assert!(
        !std::iter::from_fn(|| subscription.events.try_recv().ok()).any(|event| {
            matches!(
                event.event,
                AgentSessionEvent::AutoRetryStart { .. } | AgentSessionEvent::AutoRetryEnd { .. }
            )
        })
    );
}

#[tokio::test]
async fn abort_cancels_retry_backoff_without_starting_another_provider_call() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Error("connection lost".to_string()),
        ScriptedTurn::Text("must not run".to_string()),
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().retry(AutoRetrySettings {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 30_000,
        }),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe();
    let prompting = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("cancel retry").await }
    });

    loop {
        let event = subscription.events.recv().await.unwrap();
        if matches!(event.event, AgentSessionEvent::AutoRetryStart { .. }) {
            break;
        }
    }
    assert!(session.snapshot().auto_retry.is_some());
    session.abort();

    let pi_session::PromptOutcome::Agent(outcome) = prompting.await.unwrap().unwrap() else {
        panic!("expected an agent run");
    };
    assert_eq!(outcome.stop, AgentLoopStop::ProviderError);
    assert_eq!(provider.requests().len(), 1);
    assert!(session.snapshot().auto_retry.is_none());
    let retry_end = loop {
        let event = subscription.events.recv().await.unwrap();
        if let AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } = event.event
        {
            break (success, attempt, final_error);
        }
    };
    assert_eq!(retry_end, (false, 1, Some("Retry cancelled".to_string())));
}

#[tokio::test]
async fn manual_compaction_can_be_aborted_without_waiting_for_the_operation_gate() {
    let directory = tempfile::tempdir().unwrap();
    let provider_plugin = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("answer".to_string()),
        ScriptedTurn::WaitForAbort,
    ]);
    let provider = provider_plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider_plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        }),
    )
    .await
    .unwrap();
    session.prompt("work").await.unwrap();

    let compacting = {
        let session = session.clone();
        tokio::spawn(async move { session.compact(None).await })
    };
    for _ in 0..100 {
        if provider.requests().len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(provider.requests().len(), 2);
    session.abort_compaction();

    let error = compacting.await.unwrap().unwrap_err();
    assert!(matches!(
        error,
        SessionError::Cancelled("session compaction")
    ));
    assert!(
        !session
            .store()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| matches!(record.entry, SessionEntry::Compaction(_)))
    );
}
