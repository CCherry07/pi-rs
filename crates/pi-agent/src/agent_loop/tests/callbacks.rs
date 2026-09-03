//! Regressions for the Rust callback and runtime-generation handoff.

use pi_core::{AbortHandle, ProviderError, RegistriesBuilder, ResponseMetadata};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

use super::*;
use crate::{Agent, AgentConfigurationPatch, AgentOptions, AgentRuntime};

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
    let mut expected = vec![user("old prompt"), assistant("old response"), user("Hello")];
    assert_eq!(requests[0].messages, expected);
    expected.extend_from_slice(&outcome.new_messages[2..4]);
    assert_eq!(
        roles(&expected),
        ["user", "assistant", "user", "assistant", "toolResult"]
    );
    assert_eq!(requests[1].messages, expected);
    assert_eq!(&outcome.final_context.messages[..history.len()], history);
    assert_eq!(outcome.new_messages[0], notification);
    assert_eq!(agent.state().messages, outcome.final_context.messages);
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
    assert_eq!(outcome.new_messages, [assistant("fallback")]);
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
    assert_eq!(roles(&inputs[0]), ["user", "custom", "custom"]);
    assert_eq!(
        roles(&inputs[1]),
        ["user", "assistant", "toolResult", "custom", "custom"]
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
        ["user", "assistant", "toolResult", "assistant"]
    );
    assert_eq!(agent.state().messages, outcome.final_context.messages);
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
