//! One test per case, in source order, from
//! `legacy/pi/packages/agent/test/agent-loop.test.ts` (23 cases).
//!
//! The loop receives the same done-only responses as Pi. Callback inputs, terminal
//! results and lifecycle ordering are asserted directly. Rust's default stream
//! is scoped to the runtime generation; typed tool patches express Pi's mutations.

#[path = "tests/fixtures.rs"]
mod fixtures;

#[path = "tests/callbacks.rs"]
mod callback_regressions;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    Tool, ToolCall, ToolCallBlock, ToolCallId, ToolCallPatch, ToolContext, ToolError,
    ToolResultPatch, ToolSpec, ToolUpdateSink, UsageCost,
};
use serde_json::{Value, json};

use super::*;
use fixtures::*;

mod default_stream_function_compatibility {
    use super::*;

    // Pi: uses the configured default when a legacy caller omits streamFn.
    #[tokio::test]
    async fn uses_the_configured_default_when_a_legacy_caller_omits_stream_fn() {
        let mut harness = TestLoop::new([text_response("fallback")], vec![], vec![]);
        harness.services.default_stream_fn = harness.config.stream_fn.take();
        assert!(
            harness
                .services
                .registries
                .provider(&harness.config.provider_id)
                .is_none()
        );
        let outcome = harness.run("Hello").await;

        assert_eq!(harness.provider.requests().len(), 1);
        assert_eq!(outcome.stop, AgentLoopStop::Completed);
        let Message::Assistant(message) = &outcome.new_messages[1] else {
            panic!("expected fallback assistant response");
        };
        assert_eq!(text(&message.content), "fallback");
    }
}

mod agent_loop_with_agent_message {
    use super::*;

    #[tokio::test]
    async fn should_emit_events_with_agent_message_types() {
        let harness = TestLoop::new([text_response("Hi there!")], vec![], vec![]);
        let outcome = harness.run("Hello").await;

        assert_eq!(roles(&outcome.new_messages), ["user", "assistant"]);
        assert_eq!(outcome.new_messages[0], user("Hello"));
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

    #[tokio::test]
    async fn should_handle_custom_message_types_via_convert_to_llm() {
        let notification = custom("notification", "This is a notification");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let converted = Arc::new(Mutex::new(Vec::new()));
        let mut harness = TestLoop::new([done()], vec![], vec![]);
        harness.config.convert_to_llm = ConvertToLlm::new({
            let seen = seen.clone();
            let converted = converted.clone();
            move |messages| {
                let seen = seen.clone();
                let converted = converted.clone();
                async move {
                    *seen.lock().unwrap() = messages.clone();
                    let messages: Vec<_> = messages
                        .into_iter()
                        .filter(|message| !matches!(message, Message::Custom(_)))
                        .collect();
                    *converted.lock().unwrap() = messages.clone();
                    messages
                }
            }
        });
        harness.context.messages.push(notification.clone());
        let outcome = harness.run("Hello").await;

        assert_eq!(*seen.lock().unwrap(), [notification.clone(), user("Hello")]);
        assert_eq!(*converted.lock().unwrap(), [user("Hello")]);
        assert_eq!(harness.provider.requests()[0].messages, [user("Hello")]);
        assert_eq!(outcome.final_context.messages[0], notification);
        assert_eq!(roles(&outcome.new_messages), ["user", "assistant"]);
    }

    #[tokio::test]
    async fn should_apply_transform_context_before_convert_to_llm() {
        let transformed = Arc::new(Mutex::new(Vec::new()));
        let converted = Arc::new(Mutex::new(Vec::new()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut harness = TestLoop::new([done()], vec![], vec![]);
        harness.config.transform_context = Some(TransformContext::new({
            let transformed = transformed.clone();
            let order = order.clone();
            move |messages, _signal| {
                let transformed = transformed.clone();
                let order = order.clone();
                async move {
                    order.lock().unwrap().push("transform");
                    let tail = messages[messages.len() - 2..].to_vec();
                    *transformed.lock().unwrap() = tail.clone();
                    tail
                }
            }
        }));
        harness.config.convert_to_llm = ConvertToLlm::new({
            let converted = converted.clone();
            let order = order.clone();
            move |messages| {
                let converted = converted.clone();
                let order = order.clone();
                async move {
                    order.lock().unwrap().push("convert");
                    *converted.lock().unwrap() = messages.clone();
                    messages
                }
            }
        });
        harness.context.messages = vec![
            user("old message 1"),
            assistant("old response 1"),
            user("old message 2"),
            assistant("old response 2"),
        ];
        let outcome = harness.run("new message").await;

        let expected = vec![assistant("old response 2"), user("new message")];
        assert_eq!(*order.lock().unwrap(), ["transform", "convert"]);
        assert_eq!(*transformed.lock().unwrap(), expected);
        assert_eq!(*converted.lock().unwrap(), expected);
        assert_eq!(harness.provider.requests()[0].messages, expected);
        assert_eq!(outcome.final_context.messages.len(), 6);
    }

    #[tokio::test]
    async fn should_handle_tool_calls_and_results() {
        let usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            total_tokens: 10,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
            },
            ..Usage::default()
        };
        let patched_usage = Usage {
            input: 5,
            output: 6,
            cache_read: 7,
            cache_write: 8,
            total_tokens: 26,
            cost: UsageCost {
                input: 0.5,
                output: 0.6,
                cache_read: 0.7,
                cache_write: 0.8,
                total: 2.6,
            },
            ..Usage::default()
        };
        let observed = Arc::new(Mutex::new(None));
        let hooks = Hooks {
            after: Some(Box::new({
                let observed = observed.clone();
                let patched_usage = patched_usage.clone();
                move |event| {
                    *observed.lock().unwrap() = event.result.usage;
                    ToolResultPatch {
                        usage: Some(patched_usage.clone()),
                        ..ToolResultPatch::default()
                    }
                }
            })),
            ..Hooks::default()
        };
        let probe = Arc::new(ToolProbe::default());
        let tool = EchoTool {
            usage: Some(usage.clone()),
            ..EchoTool::new(&probe)
        };
        let harness = TestLoop::new(
            [calls(&["hello"]), done()],
            vec![Arc::new(tool)],
            vec![Arc::new(hooks)],
        );
        let outcome = harness.run("echo something").await;

        assert_eq!(
            *probe.executions.lock().unwrap(),
            [json!({"value": "hello"})]
        );
        assert_eq!(*observed.lock().unwrap(), Some(usage));
        let events = harness.events.snapshot();
        assert!(events.iter().any(|event| matches!(event,
            AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args }
                if tool_call_id.as_str() == "tool-1" && tool_name == "echo" && args == &json!({"value": "hello"})
        )));
        assert!(events.iter().any(|event| matches!(event,
            AgentEvent::ToolExecutionEnd { is_error: false, result, .. }
                if result.usage.as_ref() == Some(&patched_usage)
        )));
        let results = tool_results(&outcome.new_messages);
        assert_eq!(results.len(), 1);
        assert_eq!(text(&results[0].content), "echoed: hello");
        assert_eq!(results[0].details, Some(json!({"value": "hello"})));
        assert_eq!(results[0].usage, Some(patched_usage));
        assert!(!results[0].is_error);
        assert_eq!(
            tool_results(&harness.provider.requests()[1].messages),
            results
        );
    }

    #[tokio::test]
    async fn should_not_execute_tool_calls_from_a_length_truncated_assistant_message() {
        let probe = Arc::new(ToolProbe::default());
        let harness = TestLoop::new(
            [truncated_call(), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![],
        );
        let outcome = harness.run("echo something").await;

        assert!(probe.executions.lock().unwrap().is_empty());
        assert!(harness.events.snapshot().iter().any(|event| matches!(event,
            AgentEvent::ToolExecutionEnd { is_error: true, result, .. }
                if text(&result.content).contains("output token limit")
        )));
        assert_eq!(harness.provider.requests().len(), 2);
        assert!(matches!(
            outcome.new_messages.last(),
            Some(Message::Assistant(_))
        ));
        let results = tool_results(&outcome.new_messages);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert_eq!(results[0].tool_call_id.as_str(), "tool-1");
        assert_eq!(
            tool_results(&harness.provider.requests()[1].messages),
            results
        );
    }

    #[tokio::test]
    async fn should_execute_mutated_before_tool_call_args_without_revalidation() {
        let probe = Arc::new(ToolProbe::default());
        let hooks = Hooks {
            before: Some(Box::new(|event| {
                assert_eq!(event.validated_args, json!({"value": "hello"}));
                ToolCallPatch {
                    arguments: Some(json!({"value": 123})),
                    block: None,
                }
            })),
            ..Hooks::default()
        };
        let harness = TestLoop::new(
            [calls(&["hello"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![Arc::new(hooks)],
        );
        let outcome = harness.run("echo something").await;

        assert_eq!(*probe.executions.lock().unwrap(), [json!({"value": 123})]);
        assert_eq!(
            *probe.validations.lock().unwrap(),
            [json!({"value": "hello"})]
        );
        assert_eq!(
            text(&tool_results(&outcome.new_messages)[0].content),
            "echoed: 123"
        );
    }

    #[tokio::test]
    async fn should_prepare_tool_arguments_for_validation() {
        struct EditTool(Arc<ToolProbe>);
        #[async_trait]
        impl Tool for EditTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "edit".to_string(),
                    parameters: json!({"type": "object", "properties": {"edits": {"type": "array", "items": {"type": "object", "properties": {"oldText": {"type": "string"}, "newText": {"type": "string"}}, "required": ["oldText", "newText"]}}}, "required": ["edits"]}),
                    ..EchoTool::new(&self.0).spec()
                }
            }

            async fn prepare_arguments(
                &self,
                _context: &ToolContext,
                mut args: Value,
            ) -> Result<Value, ToolError> {
                if args["oldText"].is_string() && args["newText"].is_string() {
                    let mut edits = args["edits"].as_array().cloned().unwrap_or_default();
                    edits.push(json!({"oldText": args["oldText"], "newText": args["newText"]}));
                    args = json!({"edits": edits});
                }
                Ok(args)
            }

            fn validate_arguments(&self, args: &Value) -> Result<(), ToolError> {
                self.0.validations.lock().unwrap().push(args.clone());
                if args["edits"].as_array().is_some_and(|edits| {
                    edits
                        .iter()
                        .all(|edit| edit["oldText"].is_string() && edit["newText"].is_string())
                }) {
                    Ok(())
                } else {
                    Err(ToolError::InvalidArguments(
                        "expected edits array".to_string(),
                    ))
                }
            }

            async fn execute(
                &self,
                _context: ToolContext,
                _id: ToolCallId,
                args: Value,
                _updates: ToolUpdateSink,
            ) -> Result<ToolResult, ToolError> {
                self.0
                    .executions
                    .lock()
                    .unwrap()
                    .push(args["edits"].clone());
                Ok(ToolResult::text(format!(
                    "edited {}",
                    args["edits"].as_array().unwrap().len()
                )))
            }
        }
        let probe = Arc::new(ToolProbe::default());
        let harness = TestLoop::new(
            [
                tool_response(vec![ToolCall::new(
                    "tool-1",
                    "edit",
                    json!({"oldText": "before", "newText": "after"}),
                )]),
                done(),
            ],
            vec![Arc::new(EditTool(probe.clone()))],
            vec![],
        );
        harness.run("edit something").await;

        assert_eq!(
            *probe.executions.lock().unwrap(),
            [json!([{"oldText": "before", "newText": "after"}])]
        );
        assert_eq!(
            *probe.validations.lock().unwrap(),
            [json!({"edits": [{"oldText": "before", "newText": "after"}]})]
        );
    }

    #[tokio::test]
    async fn should_emit_tool_execution_end_in_completion_order_but_persist_tool_results_in_source_order()
     {
        let probe = Arc::new(ToolProbe::default());
        let tool = EchoTool {
            gated: true,
            ..EchoTool::new(&probe)
        };
        let mut harness = TestLoop::new(
            [calls(&["first", "second"]), done()],
            vec![Arc::new(tool)],
            vec![],
        );
        harness.config.tool_execution = ToolExecutionMode::Parallel;
        let run = harness.run("echo both");
        tokio::pin!(run);

        // Poll to the gate: second must finish while first is still blocked.
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(
            *probe.trace.lock().unwrap(),
            ["start:echo:first", "start:echo:second", "end:echo:second"]
        );
        probe.release_first.notify_one();
        let outcome = run.await;

        let events = harness.events.snapshot();
        let end_ids: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        let message_ids: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageEnd {
                    message: Message::ToolResult(result),
                } => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        let turn_ids: Vec<_> = events
            .iter()
            .flat_map(|event| match event {
                AgentEvent::TurnEnd { tool_results, .. } => tool_results.as_slice(),
                _ => &[],
            })
            .map(|result| result.tool_call_id.as_str())
            .collect();
        assert_eq!(end_ids, ["tool-2", "tool-1"]);
        assert_eq!(message_ids, ["tool-1", "tool-2"]);
        assert_eq!(turn_ids, ["tool-1", "tool-2"]);
        let results = tool_results(&outcome.new_messages);
        assert_eq!(
            results
                .iter()
                .map(|result| result.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            ["tool-1", "tool-2"]
        );
        assert_eq!(
            tool_results(&harness.provider.requests()[1].messages),
            results
        );
    }

    #[tokio::test]
    async fn should_inject_queued_messages_after_all_tool_calls_complete() {
        struct SteeringQueue {
            probe: Arc<ToolProbe>,
            delivered: Mutex<bool>,
        }
        impl AgentMessageQueues for SteeringQueue {
            fn drain_steering(&self) -> Vec<Message> {
                let mut delivered = self.delivered.lock().unwrap();
                if !*delivered && !self.probe.executions.lock().unwrap().is_empty() {
                    *delivered = true;
                    vec![user("interrupt")]
                } else {
                    vec![]
                }
            }
            fn drain_follow_up(&self) -> Vec<Message> {
                vec![]
            }
        }
        let probe = Arc::new(ToolProbe::default());
        let mut harness = TestLoop::new(
            [calls(&["first", "second"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![],
        );
        harness.config.tool_execution = ToolExecutionMode::Sequential;
        harness.services.queues = Arc::new(SteeringQueue {
            probe: probe.clone(),
            delivered: Mutex::new(false),
        });
        harness.run("start").await;

        assert_eq!(
            *probe.executions.lock().unwrap(),
            [json!({"value": "first"}), json!({"value": "second"})]
        );
        let events = harness.events.snapshot();
        let tool_errors: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .collect();
        assert_eq!(tool_errors, [false, false]);
        let sequence: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageStart {
                    message: Message::ToolResult(result),
                } => Some(format!("tool:{}", result.tool_call_id)),
                AgentEvent::MessageStart {
                    message: Message::User(message),
                } => Some(text(&message.content).to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sequence,
            ["start", "tool:tool-1", "tool:tool-2", "interrupt"]
        );
        let requests = harness.provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.last(), Some(&user("interrupt")));
    }

    #[tokio::test]
    async fn should_force_sequential_execution_when_a_tool_has_execution_mode_sequential_even_with_default_parallel_config()
     {
        let probe = Arc::new(ToolProbe::default());
        let tool = EchoTool {
            name: "slow",
            mode: ToolExecutionMode::Sequential,
            gated: true,
            ..EchoTool::new(&probe)
        };
        let harness = TestLoop::new(
            [
                tool_response(vec![
                    ToolCall::new("tool-1", "slow", json!({"value": "first"})),
                    ToolCall::new("tool-2", "slow", json!({"value": "second"})),
                ]),
                done(),
            ],
            vec![Arc::new(tool)],
            vec![],
        );
        let run = harness.run("run both");
        tokio::pin!(run);

        assert_eq!(harness.config.tool_execution, ToolExecutionMode::Parallel);
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(*probe.trace.lock().unwrap(), ["start:slow:first"]);
        probe.release_first.notify_one();
        run.await;

        assert_eq!(
            *probe.trace.lock().unwrap(),
            [
                "start:slow:first",
                "end:slow:first",
                "start:slow:second",
                "end:slow:second"
            ]
        );
        let events = harness.events.snapshot();
        let ids: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageEnd {
                    message: Message::ToolResult(result),
                } => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["tool-1", "tool-2"]);
    }

    #[tokio::test]
    async fn should_force_sequential_execution_when_one_of_multiple_tools_has_execution_mode_sequential()
     {
        let probe = Arc::new(ToolProbe::default());
        let slow = EchoTool {
            name: "slow",
            mode: ToolExecutionMode::Sequential,
            gated: true,
            ..EchoTool::new(&probe)
        };
        let fast = EchoTool {
            name: "fast",
            ..EchoTool::new(&probe)
        };
        let harness = TestLoop::new(
            [
                tool_response(vec![
                    ToolCall::new("tool-1", "slow", json!({"value": "a"})),
                    ToolCall::new("tool-2", "fast", json!({"value": "b"})),
                ]),
                done(),
            ],
            vec![Arc::new(slow), Arc::new(fast)],
            vec![],
        );
        let run = harness.run("run both");
        tokio::pin!(run);

        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(*probe.trace.lock().unwrap(), ["start:slow:a"]);
        probe.release_first.notify_one();
        run.await;
        assert_eq!(
            *probe.trace.lock().unwrap(),
            ["start:slow:a", "end:slow:a", "start:fast:b", "end:fast:b"]
        );
    }

    #[tokio::test]
    async fn should_allow_parallel_execution_when_all_tools_have_execution_mode_parallel() {
        let probe = Arc::new(ToolProbe::default());
        let tool = EchoTool {
            mode: ToolExecutionMode::Parallel,
            gated: true,
            ..EchoTool::new(&probe)
        };
        let harness = TestLoop::new(
            [calls(&["first", "second"]), done()],
            vec![Arc::new(tool)],
            vec![],
        );
        let run = harness.run("echo both");
        tokio::pin!(run);

        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(
            *probe.trace.lock().unwrap(),
            ["start:echo:first", "start:echo:second", "end:echo:second"]
        );
        probe.release_first.notify_one();
        run.await;
    }

    #[tokio::test]
    async fn should_use_prepare_next_turn_snapshot_before_continuing() {
        let probe = Arc::new(ToolProbe::default());
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let mut harness = TestLoop::new(
            [calls(&["hello"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![],
        );
        harness.context.system_prompt = "first prompt".to_string();
        harness.services.turn_control = Arc::new(FnTurnControl::new().with_prepare_next_turn({
            let prepare_calls = prepare_calls.clone();
            move |snapshot, _signal| {
                let prepare_calls = prepare_calls.clone();
                async move {
                    if prepare_calls.fetch_add(1, Ordering::SeqCst) > 0 {
                        return Ok(None);
                    }
                    assert_eq!(
                        roles(&snapshot.context.messages),
                        ["user", "assistant", "toolResult"]
                    );
                    let mut next = snapshot.context.as_ref().clone();
                    next.system_prompt = "second prompt".to_string();
                    Ok(Some(AgentLoopTurnUpdate {
                        context: Some(next),
                        ..AgentLoopTurnUpdate::default()
                    }))
                }
            }
        }));
        harness.run("echo something").await;

        let requests = harness.provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
        assert_eq!(requests[1].system_prompt, "second prompt");
        assert_eq!(requests[0].system_prompt, "first prompt");
    }

    #[tokio::test]
    async fn should_stop_after_the_current_turn_when_should_stop_after_turn_returns_true() {
        #[derive(Default)]
        struct CountingQueues {
            steering: AtomicUsize,
            follow_up: AtomicUsize,
        }
        impl AgentMessageQueues for CountingQueues {
            fn drain_steering(&self) -> Vec<Message> {
                self.steering.fetch_add(1, Ordering::SeqCst);
                vec![]
            }
            fn drain_follow_up(&self) -> Vec<Message> {
                self.follow_up.fetch_add(1, Ordering::SeqCst);
                vec![user("follow up should stay queued")]
            }
        }
        let probe = Arc::new(ToolProbe::default());
        let queues = Arc::new(CountingQueues::default());
        let snapshot = Arc::new(Mutex::new(None));
        let mut harness = TestLoop::new(
            [calls(&["hello"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![],
        );
        harness.services.queues = queues.clone();
        harness.services.turn_control = Arc::new(
            FnTurnControl::new()
                .with_prepare_next_turn(|_, _| async {
                    Err(AgentTurnControlError(
                        "a stopped turn must not prepare a continuation".to_string(),
                    ))
                })
                .with_should_stop_after_turn({
                    let snapshot = snapshot.clone();
                    move |context, _signal| {
                        let snapshot = snapshot.clone();
                        async move {
                            *snapshot.lock().unwrap() = Some(context);
                            Ok(true)
                        }
                    }
                }),
        );
        let outcome = harness.run("echo something").await;

        assert_eq!(harness.provider.requests().len(), 1);
        assert_eq!(
            *probe.executions.lock().unwrap(),
            [json!({"value": "hello"})]
        );
        assert_eq!(queues.steering.load(Ordering::SeqCst), 1);
        assert_eq!(queues.follow_up.load(Ordering::SeqCst), 0);
        let snapshot = snapshot
            .lock()
            .unwrap()
            .take()
            .expect("stop callback must run");
        assert_eq!(snapshot.message.stop_reason, StopReason::ToolUse);
        assert_eq!(
            snapshot
                .tool_results
                .iter()
                .map(|result| result.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            ["tool-1"]
        );
        assert_eq!(
            roles(&snapshot.context.messages),
            ["user", "assistant", "toolResult"]
        );
        assert_eq!(
            roles(&outcome.new_messages),
            ["user", "assistant", "toolResult"]
        );
        assert_eq!(
            event_types(&harness.events.snapshot()),
            [
                "agent_start",
                "turn_start",
                "message_start",
                "message_end",
                "message_start",
                "message_end",
                "tool_execution_start",
                "tool_execution_end",
                "message_start",
                "message_end",
                "turn_end",
                "agent_end",
            ]
        );
    }

    #[tokio::test]
    async fn should_stop_after_a_tool_batch_when_every_tool_result_sets_terminate_true() {
        let probe = Arc::new(ToolProbe::default());
        let tool = EchoTool {
            terminate_values: vec!["hello"],
            ..EchoTool::new(&probe)
        };
        let harness = TestLoop::new([calls(&["hello"]), done()], vec![Arc::new(tool)], vec![]);
        let outcome = harness.run("echo something").await;

        assert_eq!(harness.provider.requests().len(), 1);
        assert_eq!(
            roles(&outcome.new_messages),
            ["user", "assistant", "toolResult"]
        );
        assert_eq!(outcome.stop, AgentLoopStop::TerminatedByTools);
        assert_eq!(
            harness
                .events
                .snapshot()
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnEnd { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn should_stop_after_a_blocked_tool_call_when_before_tool_call_sets_terminate_true() {
        let probe = Arc::new(ToolProbe::default());
        let hooks = Hooks {
            before: Some(Box::new(|_| ToolCallPatch {
                arguments: None,
                block: Some(ToolCallBlock {
                    reason: "Blocked by policy".to_string(),
                    terminate: true,
                }),
            })),
            ..Hooks::default()
        };
        let harness = TestLoop::new(
            [calls(&["hello"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![Arc::new(hooks)],
        );
        let outcome = harness.run("echo something").await;

        assert!(probe.executions.lock().unwrap().is_empty());
        assert_eq!(harness.provider.requests().len(), 1);
        let results = tool_results(&outcome.new_messages);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert_eq!(text(&results[0].content), "Blocked by policy");
        assert_eq!(outcome.stop, AgentLoopStop::TerminatedByTools);
    }

    #[tokio::test]
    async fn should_continue_after_a_mixed_batch_with_one_terminating_blocked_call() {
        let probe = Arc::new(ToolProbe::default());
        let hooks = Hooks {
            before: Some(Box::new(|event| ToolCallPatch {
                arguments: None,
                block: (event.validated_args["value"] == "first").then(|| ToolCallBlock {
                    reason: "Blocked first".to_string(),
                    terminate: true,
                }),
            })),
            ..Hooks::default()
        };
        let mut harness = TestLoop::new(
            [calls(&["first", "second"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![Arc::new(hooks)],
        );
        harness.config.tool_execution = ToolExecutionMode::Parallel;
        let outcome = harness.run("echo both").await;

        assert_eq!(
            *probe.executions.lock().unwrap(),
            [json!({"value": "second"})]
        );
        assert_eq!(harness.provider.requests().len(), 2);
        assert_eq!(outcome.stop, AgentLoopStop::Completed);
    }

    #[tokio::test]
    async fn should_continue_after_parallel_tool_calls_when_not_all_tool_results_terminate() {
        let probe = Arc::new(ToolProbe::default());
        let tool = EchoTool {
            terminate_values: vec!["first"],
            ..EchoTool::new(&probe)
        };
        let mut harness = TestLoop::new(
            [calls(&["first", "second"]), done()],
            vec![Arc::new(tool)],
            vec![],
        );
        harness.config.tool_execution = ToolExecutionMode::Parallel;
        let outcome = harness.run("echo both").await;

        assert_eq!(harness.provider.requests().len(), 2);
        assert_eq!(
            roles(&outcome.new_messages),
            ["user", "assistant", "toolResult", "toolResult", "assistant"]
        );
        assert_eq!(outcome.stop, AgentLoopStop::Completed);
    }

    #[tokio::test]
    async fn should_allow_after_tool_call_to_mark_a_tool_batch_as_terminating() {
        let probe = Arc::new(ToolProbe::default());
        let hooks = Hooks {
            after: Some(Box::new(|event| {
                assert!(!event.result.terminate);
                ToolResultPatch {
                    terminate: Some(true),
                    ..ToolResultPatch::default()
                }
            })),
            ..Hooks::default()
        };
        let harness = TestLoop::new(
            [calls(&["hello"]), done()],
            vec![Arc::new(EchoTool::new(&probe))],
            vec![Arc::new(hooks)],
        );
        let outcome = harness.run("echo something").await;

        assert_eq!(harness.provider.requests().len(), 1);
        assert_eq!(outcome.stop, AgentLoopStop::TerminatedByTools);
        assert_eq!(
            *probe.executions.lock().unwrap(),
            [json!({"value": "hello"})]
        );
    }
}

mod agent_loop_continue_with_agent_message {
    use super::*;

    #[tokio::test]
    async fn should_throw_when_context_has_no_messages() {
        let harness = TestLoop::new([], vec![], vec![]);
        let error = harness.continue_run().await.unwrap_err();

        assert!(matches!(error, AgentLoopError::EmptyContext));
        assert_eq!(error.to_string(), "Cannot continue: no messages in context");
        assert!(harness.provider.requests().is_empty());
        assert!(harness.events.snapshot().is_empty());
    }

    #[tokio::test]
    async fn should_continue_from_existing_context_without_emitting_user_message_events() {
        let mut harness = TestLoop::new([text_response("Response")], vec![], vec![]);
        harness.context.messages = vec![user("Hello")];
        let outcome = harness.continue_run().await.unwrap();

        assert_eq!(roles(&outcome.new_messages), ["assistant"]);
        let events = harness.events.snapshot();
        let ended: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageEnd { message } => Some(message.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(roles(&ended), ["assistant"]);
        assert!(!events.iter().any(|event| matches!(
            event,
            AgentEvent::MessageStart {
                message: Message::User(_)
            } | AgentEvent::MessageEnd {
                message: Message::User(_)
            }
        )));
        assert_eq!(harness.provider.requests()[0].messages, [user("Hello")]);
        assert_eq!(outcome.final_context.messages[0], user("Hello"));
    }

    #[tokio::test]
    async fn should_allow_custom_message_types_as_last_message_caller_responsibility() {
        let original = custom("custom", "Hook content");
        let mut harness = TestLoop::new(
            [text_response("Response to custom message")],
            vec![],
            vec![],
        );
        harness.context.messages = vec![original.clone()];
        harness.config.convert_to_llm = ConvertToLlm::new(|messages| async move {
            messages
                .into_iter()
                .map(Message::into_provider_message)
                .collect()
        });
        let outcome = harness.continue_run().await.unwrap();

        assert_eq!(roles(&outcome.new_messages), ["assistant"]);
        assert_eq!(
            harness.provider.requests()[0].messages,
            [user("Hook content")]
        );
        assert_eq!(outcome.final_context.messages[0], original);
    }
}
