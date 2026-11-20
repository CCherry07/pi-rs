//! Regressions for the Rust callback and runtime-generation handoff.

use pi_core::{AbortHandle, ResponseMetadata, ResponseMetadataPatch};
use pi_plugin::{ProviderError, RegistriesBuilder};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

use super::*;
use crate::{Agent, AgentConfigurationPatch, AgentOptions, AgentRuntime};

#[tokio::test]
async fn queued_done_and_error_follow_each_content_delta_once() {
    use futures::StreamExt;

    struct Snapshots {
        recording: Arc<RecordingEvents>,
        content: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl AgentEventSink for Snapshots {
        async fn emit(
            &self,
            event: AgentEvent,
            signal: AbortSignal,
        ) -> Result<AgentEvent, crate::EventError> {
            if let AgentEvent::MessageUpdate { stream, .. } = &event {
                let message = stream.snapshot().unwrap();
                assert_eq!(message.stop_reason, StopReason::Pending);
                self.content
                    .lock()
                    .unwrap()
                    .push(text(&message.content).to_string());
            }
            self.recording.emit(event, signal).await
        }
    }
    for failure in [false, true] {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut harness = TestLoop::new([], vec![], vec![]);
        let snapshots = Arc::new(Snapshots {
            recording: harness.events.clone(),
            content: Mutex::new(Vec::new()),
        });
        harness.services.events = snapshots.clone();
        harness.config.stream_fn = Some(StreamFn::new({
            let reads = reads.clone();
            move |_, _, _| {
                let reads = reads.clone();
                async move {
                    let mut events = vec![
                        Ok(StreamEvent::Start {
                            metadata: ResponseMetadata::new(
                                "scripted".into(),
                                "test".into(),
                                "scripted",
                                1,
                            ),
                        }),
                        Ok(StreamEvent::TextStart { content_index: 0 }),
                        Ok(StreamEvent::TextDelta {
                            content_index: 0,
                            delta: "one".into(),
                        }),
                        Ok(StreamEvent::TextDelta {
                            content_index: 0,
                            delta: "two".into(),
                        }),
                        Ok(StreamEvent::TextEnd {
                            content_index: 0,
                            text_signature: None,
                        }),
                    ];
                    if failure {
                        events.push(Err(ProviderError::Failure("stream failed".into())));
                        events.push(Err(ProviderError::Failure(
                            "must not poll after failure".into(),
                        )));
                    } else {
                        events.push(Ok(StreamEvent::Done {
                            reason: StopReason::Stop,
                            usage: Usage::default(),
                        }));
                    }
                    Ok::<_, ProviderError>(AssistantResponse::Stream(Box::pin(
                        futures::stream::iter(events).inspect(move |_| {
                            reads.fetch_add(1, Ordering::SeqCst);
                        }),
                    )))
                }
            }
        }));
        let outcome = run_agent_loop(
            RunId::new("queued-terminal"),
            vec![user("hello")],
            harness.context,
            harness.config,
            harness.services,
            AbortHandle::new().1,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome.stop,
            if failure {
                AgentLoopStop::ProviderError
            } else {
                AgentLoopStop::Completed
            }
        );
        assert_eq!(
            *snapshots.content.lock().unwrap(),
            ["", "one", "onetwo", "onetwo"]
        );
        assert_eq!(reads.load(Ordering::SeqCst), 6);
        let Message::Assistant(message) = &outcome.new_messages[1] else {
            panic!("expected assistant")
        };
        assert_eq!(text(&message.content), "onetwo");
        assert_eq!(
            message.stop_reason,
            if failure {
                StopReason::Error
            } else {
                StopReason::Stop
            }
        );
        assert_eq!(
            harness
                .events
                .snapshot()
                .iter()
                .filter(|event| matches!(
                    event,
                    AgentEvent::MessageEnd {
                        message: Message::Assistant(_)
                    }
                ))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn paused_delivery_bounds_provider_reads_and_retires_the_source_on_abort_or_drop() {
    use std::sync::atomic::AtomicBool;

    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    struct UntilAbort(Arc<RecordingEvents>);
    #[async_trait]
    impl AgentEventSink for UntilAbort {
        async fn emit(
            &self,
            event: AgentEvent,
            signal: AbortSignal,
        ) -> Result<AgentEvent, crate::EventError> {
            if matches!(
                event,
                AgentEvent::MessageStart {
                    message: Message::Assistant(_)
                }
            ) {
                signal.wait().await;
            }
            if let AgentEvent::MessageUpdate { stream, update } = &event
                && matches!(update.as_ref(), StreamEvent::TextStart { .. })
            {
                assert_eq!(
                    text(&stream.snapshot().unwrap().content),
                    "",
                    "abort drains each delta before assembling the next"
                );
            }
            self.0.emit(event, signal).await
        }
    }

    for cancel in [false, true] {
        let read = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut harness = TestLoop::new([], vec![], vec![]);
        harness.services.events = Arc::new(UntilAbort(harness.events.clone()));
        harness.config.stream_fn = Some(StreamFn::new({
            let read = read.clone();
            let dropped = dropped.clone();
            move |_, _, _| {
                let read = read.clone();
                let dropped = DropFlag(dropped.clone());
                async move {
                    Ok::<_, ProviderError>(AssistantResponse::Stream(Box::pin(
                        futures::stream::unfold((0, dropped), move |(index, dropped)| {
                            read.fetch_add(1, Ordering::SeqCst);
                            async move {
                                let event = match index {
                                    0 => StreamEvent::Start {
                                        metadata: ResponseMetadata::new(
                                            "scripted".into(),
                                            "test".into(),
                                            "scripted",
                                            1,
                                        ),
                                    },
                                    1 => StreamEvent::TextStart { content_index: 0 },
                                    _ => StreamEvent::TextDelta {
                                        content_index: 0,
                                        delta: "x".into(),
                                    },
                                };
                                Some((Ok(event), (index + 1, dropped)))
                            }
                        }),
                    )))
                }
            }
        }));
        let (abort, signal) = AbortHandle::new();
        let mut run = Box::pin(run_agent_loop(
            RunId::new("bounded-source"),
            vec![user("hello")],
            harness.context,
            harness.config,
            harness.services,
            signal,
        ));
        assert!(futures::poll!(&mut run).is_pending());
        let before = read.load(Ordering::SeqCst);
        assert!(
            before > 2,
            "source must advance past the first content event"
        );
        assert_eq!(
            before, 33,
            "32 pending deltas plus the in-flight start bound source progress"
        );
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(read.load(Ordering::SeqCst), before);
        if cancel {
            abort.abort();
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), run)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(outcome.stop, AgentLoopStop::Aborted);
            let events = harness.events.snapshot();
            let updates = events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::MessageUpdate { update, .. } => Some(update.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(matches!(
                updates.first(),
                Some(StreamEvent::TextStart { .. })
            ));
            assert!(matches!(updates.last(), Some(StreamEvent::TextEnd { .. })));
            assert_eq!(updates.len(), before);
            assert!(
                updates[1..updates.len() - 1]
                    .iter()
                    .all(|update| matches!(update, StreamEvent::TextDelta { .. }))
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        AgentEvent::MessageEnd {
                            message: Message::Assistant(_)
                        }
                    ))
                    .count(),
                1
            );
        } else {
            drop(run);
        }
        assert_eq!(read.load(Ordering::SeqCst), before);
        assert!(
            dropped.load(Ordering::SeqCst),
            "retired turns must drop their provider source"
        );
    }
}

#[tokio::test]
async fn provider_progress_during_event_delivery_exposes_only_observed_usage() {
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Notify;

    struct PausedStart {
        release_start: Arc<Notify>,
        allow_usage: Arc<Notify>,
        usage: Mutex<Vec<u64>>,
        recording: Arc<RecordingEvents>,
    }
    #[async_trait]
    impl AgentEventSink for PausedStart {
        async fn emit(
            &self,
            event: AgentEvent,
            signal: AbortSignal,
        ) -> Result<AgentEvent, crate::EventError> {
            match &event {
                AgentEvent::MessageStart {
                    message: Message::Assistant(message),
                } => {
                    assert_eq!(message.usage.input, 0);
                    self.release_start.notified().await;
                }
                AgentEvent::MessageUpdate { stream, update }
                    if matches!(update.as_ref(), StreamEvent::TextStart { .. }) =>
                {
                    let snapshot = stream.snapshot().unwrap();
                    assert_eq!(
                        text(&snapshot.content),
                        "",
                        "first stream snapshot must seed incremental consumers without later content"
                    );
                    assert_eq!(
                        snapshot.stop_reason,
                        StopReason::Pending,
                        "queued Done does not terminate earlier content updates"
                    );
                    self.usage.lock().unwrap().push(snapshot.usage.input);
                    self.allow_usage.notify_one();
                }
                _ => {}
            }
            self.recording.emit(event, signal).await
        }
    }

    for usage_available in [true, false] {
        let parsed_content = Arc::new(AtomicBool::new(false));
        let parsed_usage = Arc::new(AtomicBool::new(false));
        let release_start = Arc::new(Notify::new());
        let allow_usage = Arc::new(Notify::new());
        let mut harness = TestLoop::new([], vec![], vec![]);
        let sink = Arc::new(PausedStart {
            release_start: release_start.clone(),
            allow_usage: allow_usage.clone(),
            usage: Mutex::new(Vec::new()),
            recording: harness.events.clone(),
        });
        harness.services.events = sink.clone();
        harness.config.stream_fn = Some(StreamFn::new({
            let parsed_content = parsed_content.clone();
            let parsed_usage = parsed_usage.clone();
            let allow_usage = allow_usage.clone();
            move |_, _, _| {
                let parsed_content = parsed_content.clone();
                let parsed_usage = parsed_usage.clone();
                let allow_usage = allow_usage.clone();
                async move {
                    let usage = Usage {
                        input: 10,
                        output: 4,
                        total_tokens: 14,
                        ..Usage::default()
                    };
                    let events = vec![
                        StreamEvent::Start {
                            metadata: ResponseMetadata::new(
                                "scripted".into(),
                                "test".into(),
                                "scripted",
                                1,
                            ),
                        },
                        StreamEvent::TextStart { content_index: 0 },
                        StreamEvent::TextDelta {
                            content_index: 0,
                            delta: "observed".into(),
                        },
                        StreamEvent::Metadata {
                            patch: ResponseMetadataPatch {
                                usage: Some(usage.clone()),
                                ..Default::default()
                            },
                        },
                        StreamEvent::TextEnd {
                            content_index: 0,
                            text_signature: None,
                        },
                        StreamEvent::Done {
                            reason: StopReason::Stop,
                            usage,
                        },
                    ];
                    Ok::<_, ProviderError>(AssistantResponse::Stream(Box::pin(
                        futures::stream::unfold(
                            (
                                events.into_iter().enumerate(),
                                parsed_content,
                                parsed_usage,
                                allow_usage,
                            ),
                            |(mut events, parsed_content, parsed_usage, allow_usage)| async move {
                                let (index, event) = events.next()?;
                                if index == 3 {
                                    parsed_content.store(true, Ordering::SeqCst);
                                    allow_usage.notified().await;
                                }
                                if index == 4 {
                                    parsed_usage.store(true, Ordering::SeqCst);
                                }
                                Some((
                                    Ok(event),
                                    (events, parsed_content, parsed_usage, allow_usage),
                                ))
                            },
                        ),
                    )))
                }
            }
        }));
        let run = harness.run("hello");
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        assert!(
            parsed_content.load(Ordering::SeqCst),
            "provider must progress while assistant start delivery waits"
        );
        assert!(!parsed_usage.load(Ordering::SeqCst));
        if usage_available {
            allow_usage.notify_one();
            assert!(futures::poll!(&mut run).is_pending());
            assert!(parsed_usage.load(Ordering::SeqCst));
        }
        release_start.notify_one();
        let outcome = run.await;
        assert_eq!(
            *sink.usage.lock().unwrap(),
            [if usage_available { 10 } else { 0 }]
        );
        let Message::Assistant(message) = &outcome.new_messages[1] else {
            panic!("expected assistant")
        };
        assert_eq!(message.usage.total_tokens, 14);
    }
}

#[tokio::test]
async fn default_converter_filters_custom_messages_without_changing_agent_history() {
    let probe = Arc::new(ToolProbe::default());
    let harness = TestLoop::new(
        [calls(&["hello"]), done()],
        vec![Arc::new(EchoTool::new(&probe))],
        vec![],
    );
    let history = vec![
        user("old prompt"),
        assistant("old response"),
        custom("notification", "history notification"),
    ];
    let notification = custom("notification", "prompt notification");
    let agent = Agent::with_runtime(
        AgentOptions {
            stream_fn: harness.config.stream_fn.clone(),
            messages: history.clone(),
            active_tools: vec!["echo".to_string()],
            ..AgentOptions::default()
        },
        Arc::new(AgentRuntime::new(
            1,
            "system",
            harness.services.registries.clone(),
            harness.services.plugins.clone(),
            harness.services.provider_plugins.clone(),
        )),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    agent.subscribe(Arc::new({
        let events = events.clone();
        move |event: AgentEvent, _signal: AbortSignal| {
            let events = events.clone();
            async move {
                events.lock().unwrap().push(event);
                Ok(())
            }
        }
    }));
    let outcome = agent
        .prompt(vec![notification.clone(), user("Hello")])
        .await
        .unwrap();

    let requests = harness.provider.requests();
    let declaration = outcome.new_messages[0].clone();
    assert!(
        matches!(&declaration, Message::System(system) if system.tools_added.as_ref().unwrap()[0].name == "echo")
    );
    let mut expected = vec![
        user("old prompt"),
        assistant("old response"),
        declaration,
        user("Hello"),
    ];
    assert_eq!(requests[0].messages, expected);
    expected.extend_from_slice(&outcome.new_messages[3..5]);
    assert_eq!(
        roles(&expected),
        [
            "user",
            "assistant",
            "system",
            "user",
            "assistant",
            "toolResult"
        ]
    );
    assert_eq!(requests[1].messages, expected);
    assert_eq!(&outcome.final_context.messages[..history.len()], history);
    assert_eq!(outcome.new_messages[1], notification);
    assert_eq!(
        agent.state().messages.as_ref(),
        &outcome.final_context.messages
    );
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event, AgentEvent::MessageEnd { message } if message == &notification
    )));
}

#[tokio::test]
async fn explicit_stream_wins_and_continue_uses_the_configured_default() {
    let mut harness = TestLoop::new([text_response("explicit")], vec![], vec![]);
    let fallback = MockStream::new([text_response("fallback")]);
    harness.services.default_stream_fn = Some(fallback.stream_fn());

    harness.run("Hello").await;
    assert_eq!(harness.provider.requests().len(), 1);
    assert!(fallback.requests().is_empty());

    harness.config.stream_fn = None;
    harness.context.messages = vec![user("Continue")];
    let outcome = harness.continue_run().await.unwrap();
    let mut expected = text_response("fallback");
    expected.thinking_level = Some(ThinkingLevel::Off);
    assert_eq!(outcome.new_messages, [Message::assistant(expected)]);
    assert_eq!(harness.provider.requests().len(), 1);
    assert_eq!(fallback.requests()[0].messages, [user("Continue")]);
}

#[tokio::test]
async fn missing_stream_and_invalid_continuation_return_typed_errors() {
    let mut harness = TestLoop::new([], vec![], vec![]);
    harness.config.stream_fn = None;
    harness.context.messages = vec![user("Hello")];
    assert!(matches!(
        harness.continue_run().await.unwrap_err(),
        AgentLoopError::NoDefaultStreamFunction
    ));

    harness.context.messages = vec![assistant("Hello")];
    let error = harness.continue_run().await.unwrap_err();
    assert!(matches!(error, AgentLoopError::CannotContinueFromAssistant));
    assert_eq!(
        error.to_string(),
        "Cannot continue from message role: assistant"
    );
    assert!(harness.provider.requests().is_empty());
}

#[tokio::test]
async fn agent_options_compose_plugin_transform_and_conversion_on_every_turn() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let hooks = Hooks {
        context: Some(Box::new({
            let order = order.clone();
            move |mut messages| {
                order.lock().unwrap().push("plugin");
                messages.push(custom("notification", "plugin context"));
                messages
            }
        })),
        ..Hooks::default()
    };
    let probe = Arc::new(ToolProbe::default());
    let harness = TestLoop::new(
        [calls(&["hello"]), done()],
        vec![Arc::new(EchoTool::new(&probe))],
        vec![Arc::new(hooks)],
    );
    let fallback = MockStream::new([]);
    let runtime = AgentRuntime::new(
        1,
        "system",
        harness.services.registries.clone(),
        harness.services.plugins.clone(),
        harness.services.provider_plugins.clone(),
    )
    .with_default_stream_fn(fallback.stream_fn());
    let converted_inputs = Arc::new(Mutex::new(Vec::new()));
    let options = AgentOptions {
        stream_fn: harness.config.stream_fn.clone(),
        transform_context: Some(TransformContext::new({
            let order = order.clone();
            move |mut messages, signal| {
                let order = order.clone();
                async move {
                    assert!(!signal.is_aborted());
                    tokio::task::yield_now().await;
                    order.lock().unwrap().push("transform");
                    assert_eq!(
                        messages.last(),
                        Some(&custom("notification", "plugin context"))
                    );
                    messages.push(custom("notification", "transformed context"));
                    messages
                }
            }
        })),
        convert_to_llm: ConvertToLlm::new({
            let order = order.clone();
            let inputs = converted_inputs.clone();
            move |messages| {
                let order = order.clone();
                let inputs = inputs.clone();
                async move {
                    tokio::task::yield_now().await;
                    order.lock().unwrap().push("convert");
                    inputs.lock().unwrap().push(messages.clone());
                    messages
                        .into_iter()
                        .map(Message::into_provider_message)
                        .collect()
                }
            }
        }),
        active_tools: vec!["echo".to_string()],
        ..AgentOptions::default()
    };
    let agent = Agent::with_runtime(options, Arc::new(runtime));
    let outcome = agent.prompt(vec![user("Hello")]).await.unwrap();

    assert_eq!(
        *order.lock().unwrap(),
        [
            "plugin",
            "transform",
            "convert",
            "plugin",
            "transform",
            "convert"
        ]
    );
    let inputs = converted_inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(roles(&inputs[0]), ["system", "user", "custom", "custom"]);
    assert_eq!(
        roles(&inputs[1]),
        [
            "system",
            "user",
            "assistant",
            "toolResult",
            "custom",
            "custom"
        ]
    );
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(
            &request.messages[request.messages.len() - 2..],
            [user("plugin context"), user("transformed context")]
        );
    }
    assert!(fallback.requests().is_empty());
    assert_eq!(
        roles(&outcome.final_context.messages),
        ["system", "user", "assistant", "toolResult", "assistant"]
    );
    assert_eq!(
        agent.state().messages.as_ref(),
        &outcome.final_context.messages
    );
}

#[tokio::test]
async fn runtime_default_moves_with_generation_and_survives_prompt_configuration() {
    let provider_plugin = Arc::new(ScriptedProviderPlugin::scripted([]));
    let provider = provider_plugin.provider();
    let (plugins, provider_plugins, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![provider_plugin])
        .unwrap();
    let registries = Arc::new(registries);
    let plugins = Arc::new(plugins);
    let provider_plugins = Arc::new(provider_plugins);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let runtime = |generation| {
        let seen = seen.clone();
        Arc::new(
            AgentRuntime::new(
                generation,
                "system",
                registries.clone(),
                plugins.clone(),
                provider_plugins.clone(),
            )
            .with_default_stream_fn(StreamFn::new(move |request, context, _signal| {
                let seen = seen.clone();
                async move {
                    assert_eq!(context.generation(), generation);
                    seen.lock()
                        .unwrap()
                        .push((generation, request.system_prompt));
                    Ok(done().into())
                }
            })),
        )
    };
    let agent = Agent::with_runtime(AgentOptions::default(), runtime(1));
    agent
        .configure(AgentConfigurationPatch {
            system_prompt: Some("changed".to_string()),
            ..AgentConfigurationPatch::default()
        })
        .unwrap();
    agent.prompt("first").await.unwrap();
    agent.replace_runtime(runtime(2)).await.unwrap();
    agent.prompt("second").await.unwrap();

    assert_eq!(
        *seen.lock().unwrap(),
        [(1, "changed".to_string()), (2, "system".to_string())]
    );
    assert!(provider.requests().is_empty());
}

#[tokio::test]
async fn registry_stream_preserves_content_updates_and_final_metadata() {
    let mut metadata = ResponseMetadata::new("scripted".into(), "test".into(), "scripted", 42);
    metadata.response_id = Some("response-id".to_string());
    let usage = Usage {
        input: 10,
        output: 5,
        total_tokens: 15,
        ..Usage::default()
    };
    let provider_plugin = Arc::new(ScriptedProviderPlugin::scripted([ScriptedTurn::Events(
        vec![
            StreamEvent::Start { metadata },
            StreamEvent::TextStart { content_index: 0 },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: "streamed".to_string(),
            },
            StreamEvent::TextEnd {
                content_index: 0,
                text_signature: None,
            },
            StreamEvent::Done {
                reason: StopReason::Stop,
                usage: usage.clone(),
            },
        ],
    )]));
    let (plugins, provider_plugins, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![provider_plugin])
        .unwrap();
    let mut harness = TestLoop::new([], vec![], vec![]);
    harness.services.registries = Arc::new(registries);
    harness.services.plugins = Arc::new(plugins);
    harness.services.provider_plugins = Arc::new(provider_plugins);
    harness.services.default_stream_fn = Some(StreamFn::from_registries(
        harness.services.registries.clone(),
    ));
    harness.config.stream_fn = None;
    let outcome = harness.run("Hello").await;

    let events = harness.events.snapshot();
    let updates: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageUpdate { update, .. } => Some(update.as_ref()),
            _ => None,
        })
        .collect();
    assert!(matches!(
        &updates[..],
        [
            StreamEvent::TextStart { .. },
            StreamEvent::TextDelta { .. },
            StreamEvent::TextEnd { .. }
        ]
    ));
    let Message::Assistant(message) = &outcome.new_messages[1] else {
        panic!("expected assistant")
    };
    assert_eq!(text(&message.content), "streamed");
    assert_eq!(message.timestamp_ms, 42);
    assert_eq!(message.response_id.as_deref(), Some("response-id"));
    assert_eq!(message.usage, usage);
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert!(events.iter().any(|event| matches!(event, AgentEvent::MessageEnd { message: ended } if ended == &outcome.new_messages[1])));
}

#[tokio::test]
async fn pending_stream_callback_is_cancelled_and_closes_the_lifecycle() {
    let mut harness = TestLoop::new([], vec![], vec![]);
    let received_signal = Arc::new(Mutex::new(None));
    harness.config.stream_fn = Some(StreamFn::new({
        let received_signal = received_signal.clone();
        move |_request, _context, signal| {
            *received_signal.lock().unwrap() = Some(signal);
            std::future::pending::<Result<AssistantResponse, ProviderError>>()
        }
    }));
    let (abort, signal) = AbortHandle::new();
    let run = run_agent_loop(
        RunId::new("cancel-stream"),
        vec![user("Hello")],
        harness.context.clone(),
        harness.config.clone(),
        harness.services.clone(),
        signal,
    );
    tokio::pin!(run);
    assert!(futures::poll!(&mut run).is_pending());
    abort.abort();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap();
    assert!(
        received_signal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_aborted()
    );
    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    assert_eq!(roles(&outcome.new_messages), ["user", "assistant"]);
    assert_eq!(
        event_types(&harness.events.snapshot()),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}
