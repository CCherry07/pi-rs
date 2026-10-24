use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_core::{CustomMessageContent, CustomMessageInput, Message, PluginId, UserMessage};
use pi_plugin::{
    AgentBeforeSettleEvent, AgentPluginContext, AgentSettledEvent, BoundaryResult,
    CommandContextParts, InputContext, InputEvent, InputPatch, MessageDelivery, Plugin,
    PluginContext, PluginContextScope, PluginError, PresentationMode, SendMessageOptions,
    SendUserMessageOptions,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, LaneRecordEntry, MultiSessionManager,
    PiPluginContext, PluginContextBinding, PreparedSessionGeneration, PromptOutcome,
    SessionGenerationRequest,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tokio::sync::Notify;

async fn session(
    path: &std::path::Path,
    provider: ScriptedProviderPlugin,
    plugin: impl Plugin + 'static,
) -> (MultiSessionManager, Arc<AgentSession>) {
    let access = Arc::new(PiPluginContext::new(
        PresentationMode::Print,
        true,
        PluginContextBinding::new(),
    ));
    let context: Arc<dyn PluginContext> = access.clone();
    let runtime = PiRuntime::builder()
        .provider_plugin(provider)
        .plugin(plugin)
        .plugin_context(context)
        .build()
        .unwrap();
    let options = pi_session::CreateSessionOptions::new(runtime.cwd(), path);
    let manager = MultiSessionManager::new(move |_: SessionGenerationRequest| {
        let runtime = runtime.clone();
        let access = access.clone();
        async move {
            Ok(
                PreparedSessionGeneration::new(runtime, AgentSessionOptions::default())
                    .bind_session(move |session| access.bind_generation_session(session)),
            )
        }
    });
    let session = manager.create_session(options).await.unwrap().current();
    (manager, session)
}

struct BlockingSettlement {
    before: bool,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[pi_plugin::plugin]
impl Plugin for BlockingSettlement {
    fn id(&self) -> PluginId {
        PluginId::new("blocking-settlement")
    }

    async fn agent_before_settle(
        &self,
        context: AgentPluginContext,
        _: AgentBeforeSettleEvent,
    ) -> Result<BoundaryResult, PluginError> {
        assert!(!context.session.is_idle()?);
        if self.before {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(BoundaryResult::default())
    }

    async fn agent_settled(
        &self,
        context: AgentPluginContext,
        _: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        assert!(!context.session.is_idle()?);
        if !self.before {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

#[tokio::test]
async fn session_and_plugin_wait_cover_both_settlement_phases() {
    for before in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (_manager, session) = session(
            &directory.path().join("session.jsonl"),
            ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]),
            BlockingSettlement {
                before,
                entered: entered.clone(),
                release: release.clone(),
            },
        )
        .await;
        let mut events = session.subscribe().events;
        let command = CommandContextParts::new(session.plugin_context(PluginContextScope::Command));
        let running = session.clone();
        let run = tokio::spawn(async move { running.prompt("start").await });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        assert!(!session.is_idle());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), session.wait_for_idle())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), command.session.wait_for_idle())
                .await
                .is_err()
        );
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event.event, AgentSessionEvent::AgentSettled))
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        session.wait_for_idle().await;
        command.session.wait_for_idle().await.unwrap();
        assert!(session.is_idle());
        assert_eq!(
            std::iter::from_fn(|| events.try_recv().ok())
                .filter(|event| matches!(event.event, AgentSessionEvent::AgentSettled))
                .count(),
            1
        );
    }
}

struct BoundaryDelivery {
    calls: AtomicUsize,
    settled: Arc<AtomicUsize>,
}

#[pi_plugin::plugin]
impl Plugin for BoundaryDelivery {
    fn id(&self) -> PluginId {
        PluginId::new("boundary-delivery")
    }

    async fn agent_before_settle(
        &self,
        context: AgentPluginContext,
        _: AgentBeforeSettleEvent,
    ) -> Result<BoundaryResult, PluginError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let handle = context.plugin_context_handle();
            let access = handle.access_for_adapter()?;
            access.send_message(
                CustomMessageInput {
                    custom_type: "follow-up".into(),
                    content: CustomMessageContent::Text("custom continuation".into()),
                    display: false,
                    details: None,
                },
                SendMessageOptions {
                    trigger_turn: Some(true),
                    deliver_as: Some(MessageDelivery::FollowUp),
                },
            )?;
            access.send_user_message(
                CustomMessageContent::Text("user continuation".into()),
                SendUserMessageOptions {
                    deliver_as: Some(MessageDelivery::Steer),
                    expand_prompt_templates: false,
                },
            )?;
        }
        Ok(BoundaryResult::default())
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

#[tokio::test]
async fn plugin_messages_during_before_settle_continue_the_current_activity() {
    let directory = tempfile::tempdir().unwrap();
    let provider = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("initial".into()),
        ScriptedTurn::Text("steered".into()),
        ScriptedTurn::Text("followed up".into()),
    ]);
    let requests = provider.provider();
    let settled = Arc::new(AtomicUsize::new(0));
    let (_manager, session) = session(
        &directory.path().join("session.jsonl"),
        provider,
        BoundaryDelivery {
            calls: AtomicUsize::new(0),
            settled: settled.clone(),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), session.prompt("start"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    let requests = requests.requests();
    assert_eq!(requests.len(), 3);
    let second = serde_json::to_string(&requests[1].messages).unwrap();
    let third = serde_json::to_string(&requests[2].messages).unwrap();
    assert!(second.contains("user continuation"));
    assert!(!second.contains("custom continuation"));
    assert!(third.contains("custom continuation"));
    assert!(session.is_idle());
    assert!(!session.has_pending_messages());
}

#[tokio::test]
async fn custom_delivery_waits_for_activity_and_busy_user_delivery_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]);
    let requests = provider.provider();
    let (manager, session) = session(
        &directory.path().join("delivery.jsonl"),
        provider,
        BlockingSettlement {
            before: true,
            entered: entered.clone(),
            release: release.clone(),
        },
    )
    .await;
    let context = session.plugin_context(PluginContextScope::Command);
    let access = context.access_for_adapter().unwrap();
    let run = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("start").await }
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        access
            .send_user_message_and_wait(
                PluginContextScope::Command,
                CustomMessageContent::Text("unadmitted".into()),
                SendUserMessageOptions::default(),
            )
            .await,
        Err(pi_plugin::PluginContextError::Invalid(_))
    ));
    let delivery = access.send_message_and_wait(
        PluginContextScope::Command,
        CustomMessageInput {
            custom_type: "after-activity".into(),
            content: CustomMessageContent::Text("saved context".into()),
            display: true,
            details: None,
        },
        SendMessageOptions {
            trigger_turn: Some(false),
            deliver_as: None,
        },
    );
    tokio::pin!(delivery);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut delivery)
            .await
            .is_err()
    );
    assert!(!session.history().load().unwrap().entries.iter().any(|record|
        matches!(&record.entry, pi_session::SessionEntry::CustomMessage(message) if message.custom_type == "after-activity")
    ));
    release.notify_one();
    run.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(2), delivery)
        .await
        .unwrap()
        .unwrap();
    let history = session.history().load().unwrap();
    assert!(matches!(&history.entries.last().unwrap().entry,
        pi_session::SessionEntry::CustomMessage(message) if message.custom_type == "after-activity"
    ));
    assert_eq!(requests.requests().len(), 1);
    assert!(session.is_idle());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn background_delivery_reports_failure_when_its_generation_closes() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(Notify::new());
    let (manager, session) = session(
        &directory.path().join("retired-delivery.jsonl"),
        ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]),
        CancelBoundary(entered.clone()),
    )
    .await;
    let mut events = session.subscribe().events;
    let context = session.plugin_context(PluginContextScope::Command);
    let access = context.access_for_adapter().unwrap();
    let run = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("start").await }
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    access
        .send_message(
            CustomMessageInput {
                custom_type: "retired-write".into(),
                content: CustomMessageContent::Text("must not append".into()),
                display: false,
                details: None,
            },
            SendMessageOptions {
                trigger_turn: Some(false),
                deliver_as: None,
            },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), manager.shutdown())
        .await
        .unwrap()
        .unwrap();
    run.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(
                events.recv().await.unwrap().event,
                AgentSessionEvent::PluginNotice {
                    level: pi_session::NoticeLevel::Error,
                    ..
                }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(!session.history().load().unwrap().entries.iter().any(|record|
        matches!(&record.entry, pi_session::SessionEntry::CustomMessage(message) if message.custom_type == "retired-write")
    ));
    assert!(context.access_for_adapter().is_err());
}

struct ImageInputProbe(Arc<std::sync::Mutex<Vec<pi_core::ImageContent>>>);

#[pi_plugin::plugin]
impl Plugin for ImageInputProbe {
    fn id(&self) -> PluginId {
        PluginId::new("image-input-probe")
    }

    async fn input(&self, _: InputContext, event: InputEvent) -> Result<InputPatch, PluginError> {
        *self.0.lock().unwrap() = event.images.unwrap_or_default();
        Ok(InputPatch::Continue)
    }
}

#[tokio::test]
async fn preprocessed_plugin_user_delivery_preserves_images_through_history_and_provider() {
    let directory = tempfile::tempdir().unwrap();
    let images = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("seen".into())]);
    let requests = provider.provider();
    let (manager, session) = session(
        &directory.path().join("images.jsonl"),
        provider,
        ImageInputProbe(images.clone()),
    )
    .await;
    let image = pi_core::ImageContent {
        data: "aW1hZ2U=".into(),
        mime_type: "image/png".into(),
    };
    session
        .plugin_context(PluginContextScope::Command)
        .access_for_adapter()
        .unwrap()
        .send_user_message_and_wait(
            PluginContextScope::Command,
            CustomMessageContent::Blocks(vec![
                pi_core::ContentBlock::Text(pi_core::TextContent::new("inspect")),
                pi_core::ContentBlock::Image(image.clone()),
            ]),
            SendUserMessageOptions {
                deliver_as: None,
                expand_prompt_templates: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(*images.lock().unwrap(), vec![image.clone()]);
    assert!(
        matches!(&requests.requests()[0].messages[0], Message::User(user)
        if user.content.contains(&pi_core::ContentBlock::Image(image.clone())))
    );
    let document = session.history().load().unwrap();
    assert!(
        matches!(document.messages()[0].as_standard(), Some(Message::User(user))
        if user.content.contains(&pi_core::ContentBlock::Image(image)))
    );
    manager.shutdown().await.unwrap();
}

struct CountSettlement(Arc<AtomicUsize>);

#[pi_plugin::plugin]
impl Plugin for CountSettlement {
    fn id(&self) -> PluginId {
        PluginId::new("count-settlement")
    }
    async fn input(&self, _: InputContext, event: InputEvent) -> Result<InputPatch, PluginError> {
        match event.text.as_str() {
            "handled" => Ok(InputPatch::Handled),
            "error" => Err(PluginError::Failure("input failed".into())),
            _ => Ok(InputPatch::Continue),
        }
    }
    async fn agent_settled(
        &self,
        _: AgentPluginContext,
        _: AgentSettledEvent,
    ) -> Result<(), PluginError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(PluginError::Failure("settled diagnostic".into()))
    }
}

#[tokio::test]
async fn prompt_and_structured_messages_share_completion_and_release_failed_admission() {
    let directory = tempfile::tempdir().unwrap();
    let settled = Arc::new(AtomicUsize::new(0));
    let (_manager, session) = session(
        &directory.path().join("session.jsonl"),
        ScriptedProviderPlugin::scripted((0..3).map(|_| ScriptedTurn::Text("done".into()))),
        CountSettlement(settled.clone()),
    )
    .await;
    assert!(matches!(
        session.prompt("handled").await.unwrap(),
        PromptOutcome::Handled
    ));
    assert!(session.is_idle());
    assert_eq!(settled.load(Ordering::SeqCst), 0);
    assert!(
        session
            .history()
            .shared_document()
            .unwrap()
            .records
            .is_empty()
    );
    // Input hook failures are diagnostic and leave input runnable, matching Pi.
    session.prompt("error").await.unwrap();
    assert!(session.run_messages(Vec::new()).await.is_err());
    assert!(session.is_idle());
    session.prompt("prompt").await.unwrap();
    // The same text would be intercepted by the input hook through `prompt`.
    session
        .run_messages(vec![Message::User(UserMessage::text("handled", 1))])
        .await
        .unwrap();
    assert_eq!(settled.load(Ordering::SeqCst), 3);
    assert!(session.is_idle());
    let document = session.history().shared_document().unwrap();
    assert_eq!(
        document
            .records
            .iter()
            .filter(|record| matches!(record.record, LaneRecordEntry::OperationStarted { .. }))
            .count(),
        3
    );
    assert_eq!(
        document
            .records
            .iter()
            .filter(|record| matches!(record.record, LaneRecordEntry::OperationFinished { .. }))
            .count(),
        3
    );
}

struct CancelBoundary(Arc<Notify>);

#[pi_plugin::plugin]
impl Plugin for CancelBoundary {
    fn id(&self) -> PluginId {
        PluginId::new("cancel-boundary")
    }

    async fn agent_before_settle(
        &self,
        context: AgentPluginContext,
        _: AgentBeforeSettleEvent,
    ) -> Result<BoundaryResult, PluginError> {
        self.0.notify_one();
        context.signal().wait().await;
        Ok(BoundaryResult {
            continue_run: Some(true),
            ..BoundaryResult::default()
        })
    }
}

#[tokio::test]
async fn reload_cancels_the_product_activity_after_the_agent_has_finished() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(Notify::new());
    let provider = Arc::new(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
        "done".into(),
    )]));
    let requests = provider.provider();
    let factory_entered = entered.clone();
    let manager = MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let entered = factory_entered.clone();
        let provider = provider.clone();
        async move {
            Ok(PreparedSessionGeneration::new(
                PiRuntime::builder()
                    .workspace(request.workspace)
                    .provider_plugin_arc(provider)
                    .plugin(CancelBoundary(entered))
                    .build()
                    .unwrap(),
                AgentSessionOptions::default(),
            ))
        }
    });
    let managed = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("session.jsonl"),
        ))
        .await
        .unwrap();
    let old = managed.current();
    let running = old.clone();
    let run = tokio::spawn(async move { running.prompt("start").await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(!old.is_idle());
    tokio::time::timeout(Duration::from_secs(2), managed.reload())
        .await
        .unwrap()
        .unwrap();
    let pi_session::PromptOutcome::Agent(outcome) = run.await.unwrap().unwrap() else {
        panic!("expected an agent run");
    };
    assert_eq!(outcome.stop, pi_agent::AgentLoopStop::Aborted);
    assert!(old.is_closed());
    assert!(managed.current().is_idle());
    assert_eq!(requests.requests().len(), 1);
    manager.shutdown().await.unwrap();
}

struct BlockingInput {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[pi_plugin::plugin]
impl Plugin for BlockingInput {
    fn id(&self) -> PluginId {
        PluginId::new("blocking-input")
    }

    async fn input(&self, context: InputContext, _: InputEvent) -> Result<InputPatch, PluginError> {
        assert!(!context.session.is_idle()?);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(InputPatch::Continue)
    }
}

#[tokio::test]
async fn abort_during_preparation_does_not_start_a_provider_run() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = ScriptedProviderPlugin::scripted([]);
    let requests = provider.provider();
    let (_manager, session) = session(
        &directory.path().join("session.jsonl"),
        provider,
        BlockingInput {
            entered: entered.clone(),
            release: release.clone(),
        },
    )
    .await;
    let running = session.clone();
    let run = tokio::spawn(async move { running.prompt("start").await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(!session.is_idle());
    session.abort();
    release.notify_one();
    assert!(matches!(
        run.await.unwrap(),
        Err(pi_session::SessionError::Cancelled("session input"))
    ));
    assert!(requests.requests().is_empty());
    assert!(session.is_idle());
    assert!(!session.history().is_materialized());
    assert!(
        session
            .history()
            .shared_document()
            .unwrap()
            .records
            .is_empty()
    );
}
