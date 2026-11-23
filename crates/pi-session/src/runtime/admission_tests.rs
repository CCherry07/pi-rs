use super::*;
use crate::PromptDisposition;

struct BlockingFirstResponse {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    seen: AtomicUsize,
}

#[pi_plugin::plugin]
impl Plugin for BlockingFirstResponse {
    fn id(&self) -> PluginId {
        PluginId::new("blocking-first-response")
    }

    async fn message_end(
        &self,
        _context: AgentPluginContext,
        event: MessageEndEvent,
    ) -> Result<MessageEndPatch, PluginError> {
        if matches!(event.message, Message::Assistant(_))
            && self.seen.fetch_add(1, Ordering::SeqCst) == 0
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(MessageEndPatch::default())
    }
}

#[tokio::test]
async fn live_model_and_thinking_changes_reach_the_next_request_and_preserve_streamed_history() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let scripted = ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("first answer".into()),
        ScriptedTurn::Events(vec![
            StreamEvent::Start {
                metadata: ResponseMetadata::new(
                    "scripted".into(),
                    "next".into(),
                    "scripted",
                    now_ms(),
                ),
            },
            StreamEvent::TextStart { content_index: 0 },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: "second answer".into(),
            },
            StreamEvent::TextEnd {
                content_index: 0,
                text_signature: None,
            },
            StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            },
        ]),
    ]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .plugin(BlockingFirstResponse {
            entered: entered.clone(),
            release: release.clone(),
            seen: AtomicUsize::new(0),
        })
        .provider_plugin(scripted)
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let generation = session.runtime().generation();
    let running = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("first prompt").await })
    };
    entered.notified().await;
    assert!(session.runtime().agent().state().is_running);
    assert!(session.set_model("missing".into(), "model".into()).is_err());
    assert_eq!(session.runtime().agent().state().model_id.as_str(), "test");
    let cancelled = session
        .runtime()
        .prepare_model_configuration(
            Some(pi_core::ModelSelection {
                provider: "scripted".into(),
                model_id: "cancelled".into(),
            }),
            Some(ThinkingLevel::Low),
        )
        .unwrap();
    drop(cancelled);
    assert_eq!(session.runtime().agent().state().model_id.as_str(), "test");
    session.set_model("scripted".into(), "next".into()).unwrap();
    session.set_thinking_level(ThinkingLevel::High).unwrap();
    session
        .queue_user_input("second prompt", pi_plugin::InputStreamingBehavior::FollowUp)
        .await
        .unwrap();
    release.notify_one();
    running.await.unwrap().unwrap();
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].model.as_str(), "test");
    assert_eq!(requests[0].thinking_level, ThinkingLevel::Off);
    assert_eq!(requests[1].model.as_str(), "next");
    assert_eq!(requests[1].thinking_level, ThinkingLevel::High);
    assert_eq!(session.runtime().generation(), generation);
    let state = session.runtime().agent().state();
    assert_eq!(state.messages.len(), 4);
    assert_eq!(state.model_id.as_str(), "next");
    assert_eq!(state.thinking_level, ThinkingLevel::High);
    let document = session.store().load().unwrap();
    let context = document.context().unwrap();
    assert_eq!(context.messages.len(), 4);
    assert_eq!(context.model.unwrap().model_id.as_str(), "next");
    assert_eq!(context.thinking_level, "high");
    session.shutdown().await;
}

struct InputOrderPlugin(Arc<Mutex<Vec<String>>>);

#[pi_plugin::plugin]
impl Plugin for InputOrderPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("input-order")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        for source in [
            pi_plugin::CommandSource::Extension,
            pi_plugin::CommandSource::Skill,
            pi_plugin::CommandSource::Prompt,
        ] {
            context.register_command(Arc::new(InputOrderCommand(source)))?;
        }
        Ok(())
    }

    async fn input(
        &self,
        _context: pi_plugin::InputContext,
        event: pi_plugin::InputEvent,
    ) -> Result<pi_plugin::InputPatch, PluginError> {
        self.0.lock().unwrap().push(event.text.clone());
        Ok(match event.text.as_str() {
            "rewrite" => pi_plugin::InputPatch::Transform {
                text: "/skill:order rewritten".into(),
                images: None,
            },
            "extension-from-hook" => pi_plugin::InputPatch::Transform {
                text: "/order-extension untouched".into(),
                images: None,
            },
            "/order-prompt intercepted" => pi_plugin::InputPatch::Handled,
            _ => pi_plugin::InputPatch::Continue,
        })
    }
}

struct InputOrderCommand(pi_plugin::CommandSource);

#[async_trait]
impl Command for InputOrderCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: match self.0 {
                pi_plugin::CommandSource::Extension => "order-extension",
                pi_plugin::CommandSource::Skill => "skill:order",
                pi_plugin::CommandSource::Prompt => "order-prompt",
            }
            .into(),
            description: "Checks command and input ordering".into(),
            argument_hint: None,
        }
    }

    fn metadata(&self) -> Option<pi_plugin::CommandMetadata> {
        Some(pi_plugin::CommandMetadata {
            source: self.0,
            source_info: pi_plugin::CommandSourceInfo {
                path: "input-order".into(),
                source: "test".into(),
                scope: pi_plugin::CommandSourceScope::Temporary,
                origin: pi_plugin::CommandSourceOrigin::TopLevel,
                base_dir: None,
                changelog_path: None,
            },
        })
    }

    async fn execute(
        &self,
        _context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        Ok(CommandOutcome::TransformInput(match self.0 {
            pi_plugin::CommandSource::Extension => format!("/skill:order {arguments}"),
            pi_plugin::CommandSource::Skill => format!("/order-prompt {arguments}"),
            pi_plugin::CommandSource::Prompt => format!("expanded {arguments}"),
        }))
    }
}

#[tokio::test]
async fn resource_commands_expand_after_input_hooks_across_submission_paths() {
    use pi_plugin::InputStreamingBehavior;
    let directory = tempfile::tempdir().unwrap();
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let runtime = PiRuntime::builder()
        .plugin(InputOrderPlugin(inputs.clone()))
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("done".into()),
            ScriptedTurn::Text("done".into()),
            ScriptedTurn::WaitForAbort,
        ]))
        .build()
        .unwrap();
    for (input, expected) in [
        ("/order-extension direct", "expanded direct"),
        ("/skill:order direct", "expanded direct"),
        ("rewrite", "expanded rewritten"),
        ("extension-from-hook", "/order-extension untouched"),
    ] {
        let pi_runtime::PreparedTextSubmission::Agent(prepared) =
            runtime.prepare_text_submission(input).await.unwrap()
        else {
            panic!("expected a prepared prompt");
        };
        assert_eq!(prepared.display_text(), input);
        assert_eq!(prepared.text(), expected);
        drop(prepared);
        let pi_runtime::QueuedTextOutcome::Message {
            display_text, text, ..
        } = runtime
            .process_queued_input(input, None, InputStreamingBehavior::Steer)
            .await
            .unwrap()
        else {
            panic!("expected queued input");
        };
        assert_eq!(display_text, input);
        assert_eq!(text, expected);
    }
    assert_eq!(
        *inputs.lock().unwrap(),
        [
            "/skill:order direct",
            "/skill:order direct",
            "/skill:order direct",
            "/skill:order direct",
            "rewrite",
            "rewrite",
            "extension-from-hook",
            "extension-from-hook",
        ]
    );
    inputs.lock().unwrap().clear();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    session.prompt("/order-extension session").await.unwrap();
    session.prompt("/skill:order session").await.unwrap();
    assert!(matches!(
        session.prompt("/order-prompt intercepted").await.unwrap(),
        PromptOutcome::Handled
    ));
    session
        .queue_user_input("rewrite", InputStreamingBehavior::FollowUp)
        .await
        .unwrap();
    assert_eq!(session.snapshot().queue.follow_up, ["expanded rewritten"]);
    session.clear_queue().unwrap();
    let (sender, started) = tokio::sync::oneshot::channel();
    let running = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .prompt_with_receipt("start", |_| {
                    sender.send(()).unwrap();
                })
                .await
        })
    };
    started.await.unwrap();
    session.prompt("/skill:order live").await.unwrap();
    assert_eq!(session.snapshot().queue.steering, ["/skill:order live"]);
    session
        .queue_user_input("/order-prompt live", InputStreamingBehavior::FollowUp)
        .await
        .unwrap();
    assert_eq!(session.snapshot().queue.follow_up, ["expanded live"]);
    assert_eq!(
        *inputs.lock().unwrap(),
        [
            "/skill:order session",
            "/skill:order session",
            "/order-prompt intercepted",
            "rewrite",
            "start",
            "/skill:order live",
            "/order-prompt live"
        ]
    );
    session.abort_and_wait().await;
    running.await.unwrap().unwrap();
    session.shutdown().await;
}

struct RpcInputProbe(Arc<Mutex<Vec<pi_plugin::InputEvent>>>);

#[pi_plugin::plugin]
impl Plugin for RpcInputProbe {
    fn id(&self) -> PluginId {
        PluginId::new("rpc-input-probe")
    }

    async fn input(
        &self,
        _context: pi_plugin::InputContext,
        event: pi_plugin::InputEvent,
    ) -> Result<pi_plugin::InputPatch, PluginError> {
        let handled = event.text == "handled";
        let rewrite = event.text == "rewrite";
        self.0.lock().unwrap().push(event);
        Ok(if handled {
            pi_plugin::InputPatch::Handled
        } else if rewrite {
            pi_plugin::InputPatch::Transform {
                text: "rewritten".into(),
                images: None,
            }
        } else {
            pi_plugin::InputPatch::Continue
        })
    }
}

#[tokio::test]
async fn explicit_streaming_policy_runs_commands_and_rpc_input_hooks_before_rejection() {
    use pi_plugin::{InputSource, InputStreamingBehavior};
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let runtime = PiRuntime::builder()
        .plugin(ExplicitCommandPlugin(calls.clone()))
        .plugin(RpcInputProbe(inputs.clone()))
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::WaitForAbort,
        ]))
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe();
    let (sender, started) = tokio::sync::oneshot::channel();
    let running = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .prompt_with_streaming_behavior_and_receipt(
                    SessionInput::new("start").with_source(InputSource::Rpc),
                    None,
                    |receipt| {
                        sender.send(receipt).unwrap();
                    },
                )
                .await
        })
    };
    assert_eq!(started.await.unwrap(), PromptDisposition::Started);
    for text in ["/ui-action", "handled"] {
        let result = session
            .prompt_with_streaming_behavior_and_receipt(
                SessionInput::new(text).with_source(InputSource::Rpc),
                None,
                |receipt| assert_eq!(receipt, PromptDisposition::Handled),
            )
            .await
            .unwrap();
        assert!(matches!(result, PromptOutcome::Handled));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let error = session
        .prompt_with_streaming_behavior_and_receipt(
            SessionInput::new("requires delivery").with_source(InputSource::Rpc),
            None,
            |_| panic!("rejected input cannot receive a success receipt"),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Specify streamingBehavior"));
    assert!(
        session
            .queue_user_input("/ui-action", InputStreamingBehavior::Steer)
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot be queued")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    session
        .queue_user_input(
            SessionInput::new("queued").with_source(InputSource::Rpc),
            InputStreamingBehavior::FollowUp,
        )
        .await
        .unwrap();
    session
        .queue_user_input(
            SessionInput::new("rewrite").with_source(InputSource::Rpc),
            InputStreamingBehavior::Steer,
        )
        .await
        .unwrap();
    assert_eq!(session.snapshot().queue.steering, ["rewritten"]);
    let inputs = inputs.lock().unwrap().clone();
    assert_eq!(
        inputs
            .iter()
            .map(|input| input.text.as_str())
            .collect::<Vec<_>>(),
        ["start", "handled", "requires delivery", "queued", "rewrite"]
    );
    assert!(inputs.iter().all(|input| input.source == InputSource::Rpc));
    assert!(
        inputs[..3]
            .iter()
            .all(|input| input.streaming_behavior.is_none())
    );
    assert_eq!(
        inputs[3].streaming_behavior,
        Some(InputStreamingBehavior::FollowUp)
    );
    session.abort_and_wait().await;
    running.await.unwrap().unwrap();
    assert!(session.is_idle());
    let mut aborted = None;
    while let Ok(event) = events.events.try_recv() {
        if let AgentSessionEvent::AgentSettled { aborted: value } = event.event {
            aborted = Some(value);
        }
    }
    assert_eq!(aborted, Some(true));
    session.shutdown().await;
}

#[tokio::test]
async fn abort_waits_for_settled_callbacks_before_acknowledging_idle() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let runtime = PiRuntime::builder()
        .plugin(BlockingAgentSettledPlugin {
            entered: entered.clone(),
            release: release.clone(),
        })
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::WaitForAbort,
        ]))
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let (sender, started) = tokio::sync::oneshot::channel();
    let running = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .prompt_with_receipt("start", |_| {
                    sender.send(()).unwrap();
                })
                .await
        })
    };
    started.await.unwrap();
    let aborting = {
        let session = session.clone();
        tokio::spawn(async move { session.abort_and_wait().await })
    };
    entered.notified().await;
    assert!(!aborting.is_finished());
    release.notify_one();
    aborting.await.unwrap();
    running.await.unwrap().unwrap();
    assert!(session.is_idle());
    session.shutdown().await;
}

#[tokio::test]
async fn summarization_retries_publish_progress_and_use_the_current_session_policy() {
    for branch in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = scripted_runtime([
            ScriptedTurn::Text("first answer".into()),
            ScriptedTurn::Text("abandoned answer".into()),
            ScriptedTurn::Error("Connection closed.".into()),
            ScriptedTurn::Text("summary recovered".into()),
            ScriptedTurn::Text("retained turn summary".into()),
        ]);
        let session = AgentSession::create(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default()
                .compaction(CompactionSettings {
                    keep_recent_tokens: 1,
                    ..Default::default()
                })
                .retry(AutoRetrySettings {
                    enabled: true,
                    max_retries: 2,
                    base_delay_ms: 0,
                }),
        )
        .await
        .unwrap();
        session.prompt("first prompt").await.unwrap();
        let target = session
            .history()
            .load()
            .unwrap()
            .entries
            .last()
            .unwrap()
            .id
            .clone();
        session.prompt("second prompt").await.unwrap();
        let mut subscription = session.subscribe();
        if branch {
            session
                .summarize_branch_and_checkout(&target, None, false, None)
                .await
                .unwrap();
        } else {
            session.compact(None).await.unwrap();
        }
        let mut retry_events = Vec::new();
        while let Ok(event) = subscription.events.try_recv() {
            match event.event {
                AgentSessionEvent::SummarizationRetryScheduled {
                    attempt,
                    max_attempts,
                    delay_ms,
                    error_message,
                } => {
                    assert_eq!((attempt, max_attempts, delay_ms), (1, 2, 0));
                    assert!(error_message.contains("Connection closed"));
                    retry_events.push("scheduled");
                }
                AgentSessionEvent::SummarizationRetryAttemptStart { source } => {
                    assert_eq!(
                        source,
                        if branch {
                            crate::SummarizationSource::BranchSummary
                        } else {
                            crate::SummarizationSource::Compaction {
                                reason: crate::CompactionReason::Manual,
                            }
                        }
                    );
                    retry_events.push("attempt");
                }
                AgentSessionEvent::SummarizationRetryFinished => retry_events.push("finished"),
                _ => {}
            }
        }
        assert_eq!(retry_events, ["scheduled", "attempt", "finished"]);
        session.shutdown().await;
    }
}

#[tokio::test]
async fn aborting_summarization_backoff_finishes_retry_progress_without_another_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let session = AgentSession::create(
        scripted_runtime([
            ScriptedTurn::Text("answer".into()),
            ScriptedTurn::Error("Connection closed.".into()),
        ]),
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default()
            .compaction(CompactionSettings {
                keep_recent_tokens: 1,
                ..Default::default()
            })
            .retry(AutoRetrySettings {
                enabled: true,
                max_retries: 2,
                base_delay_ms: 60_000,
            }),
    )
    .await
    .unwrap();
    session.prompt("first").await.unwrap();
    let mut subscription = session.subscribe();
    let compacting = {
        let session = session.clone();
        tokio::spawn(async move { session.compact(None).await })
    };
    while let Ok(event) = subscription.events.recv().await {
        if matches!(
            event.event,
            AgentSessionEvent::SummarizationRetryScheduled { .. }
        ) {
            break;
        }
    }
    session.abort_compaction();
    assert!(matches!(
        compacting.await.unwrap(),
        Err(SessionError::Cancelled(_))
    ));
    let mut finished = 0;
    let mut compaction_ended = false;
    while let Ok(event) = subscription.events.try_recv() {
        assert!(!matches!(
            event.event,
            AgentSessionEvent::SummarizationRetryAttemptStart { .. }
        ));
        if matches!(event.event, AgentSessionEvent::SummarizationRetryFinished) {
            finished += 1;
        }
        if let AgentSessionEvent::CompactionEnd {
            aborted,
            will_retry,
            error_message,
            ..
        } = event.event
        {
            assert!(aborted);
            assert!(!will_retry);
            assert!(error_message.is_none());
            compaction_ended = true;
        }
    }
    assert_eq!(finished, 1);
    assert!(compaction_ended);
    session.shutdown().await;
}

#[tokio::test]
async fn manual_compaction_aborts_the_active_turn_before_summarizing() {
    let directory = tempfile::tempdir().unwrap();
    let session = AgentSession::create(
        scripted_runtime([
            ScriptedTurn::Text("saved answer".into()),
            ScriptedTurn::WaitForAbort,
            ScriptedTurn::Text("summary".into()),
            ScriptedTurn::Text("turn summary".into()),
        ]),
        directory.path().join("session.jsonl"),
        AgentSessionOptions::default().compaction(CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    session.prompt("saved prompt").await.unwrap();
    let (sender, started) = tokio::sync::oneshot::channel();
    let running = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .prompt_with_receipt("interrupt me", |_| {
                    sender.send(()).unwrap();
                })
                .await
        })
    };
    started.await.unwrap();
    let mut events = session.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(2), session.compact(None))
        .await
        .expect("manual compaction must interrupt provider work instead of waiting for it")
        .unwrap();
    let PromptOutcome::Agent(outcome) = running.await.unwrap().unwrap() else {
        panic!("expected started run")
    };
    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    let mut ordering = Vec::new();
    while let Ok(event) = events.events.try_recv() {
        match event.event {
            AgentSessionEvent::AgentSettled { aborted } => {
                assert!(aborted);
                ordering.push("settled");
            }
            AgentSessionEvent::CompactionStart {
                reason: crate::CompactionReason::Manual,
            } => ordering.push("compaction"),
            _ => {}
        }
    }
    assert_eq!(ordering, ["settled", "compaction"]);
    session.shutdown().await;
}

#[tokio::test]
async fn manual_compaction_reports_empty_and_already_compacted_with_terminal_events() {
    let directory = tempfile::tempdir().unwrap();
    let session = AgentSession::create(
        scripted_runtime([]),
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    for (already, expected) in [
        (false, "Nothing to compact (session too small)"),
        (true, "Already compacted"),
    ] {
        if already {
            session
                .append_compaction(CompactionEntry {
                    from_hook: None,
                    system_message: None,
                    retained_entry_ids: Vec::new(),
                    summary: "existing summary".into(),
                    retained_tail: Vec::new(),
                    tokens_before: 1,
                    details: None,
                    usage: None,
                })
                .await
                .unwrap();
        }
        let mut events = session.subscribe();
        assert_eq!(
            session.compact(None).await.unwrap_err().to_string(),
            expected
        );
        let mut ordering = Vec::new();
        while let Ok(event) = events.events.try_recv() {
            match event.event {
                AgentSessionEvent::CompactionStart { .. } => ordering.push("start"),
                AgentSessionEvent::CompactionEnd {
                    result,
                    aborted,
                    will_retry,
                    error_message,
                    ..
                } => {
                    assert!(result.is_none());
                    assert!(!aborted);
                    assert!(!will_retry);
                    assert_eq!(
                        error_message,
                        Some(format!("Compaction failed: {expected}"))
                    );
                    ordering.push("end");
                }
                _ => {}
            }
        }
        assert_eq!(ordering, ["start", "end"]);
    }
    session.shutdown().await;
}

#[tokio::test]
async fn prompt_receipt_follows_admission_and_precedes_provider_execution() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::WaitForAbort]);
    let provider = plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(plugin)
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
        Default::default(),
    )
    .await
    .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .prompt_with_receipt("working", |disposition| {
                    assert!(provider.requests().is_empty());
                    assert!(session.accepts_live_messages());
                    sender.send(disposition).unwrap();
                })
                .await
        })
    };
    let disposition = tokio::time::timeout(std::time::Duration::from_secs(2), receiver)
        .await
        .expect("the receipt must not wait for provider completion")
        .unwrap();
    assert_eq!(disposition, PromptDisposition::Started);
    assert!(!running.is_finished());
    let queued = Arc::new(Mutex::new(None));
    session
        .prompt_with_receipt("next", |disposition| {
            *queued.lock().unwrap() = Some(disposition);
        })
        .await
        .unwrap();
    assert_eq!(*queued.lock().unwrap(), Some(PromptDisposition::Queued));
    session.abort();
    running.await.unwrap().unwrap();
    session.shutdown().await;
}

#[tokio::test]
async fn prompt_receipt_reports_handled_input_and_omits_failed_preparation() {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let session = AgentSession::create(
        explicit_command_runtime(Arc::clone(&calls), ScriptedTurn::WaitForAbort),
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    session
        .prompt_with_receipt("/ui-action", |disposition| {
            assert_eq!(disposition, PromptDisposition::Handled);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(!session.accepts_live_messages());
        })
        .await
        .unwrap();
    session.shutdown().await;
    let error = session
        .prompt_with_receipt("closed", |_| {
            panic!("failed preparation cannot be acknowledged")
        })
        .await
        .unwrap_err();
    assert!(matches!(error, SessionError::Closed));
}
