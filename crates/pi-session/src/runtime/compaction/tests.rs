//! Regression tests across the compaction computation and adapter seams.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_runtime::PiRuntime;
use serde_json::{Value, json};

use pi_core::{
    AbortSignal, AssistantMessage, ContentBlock, Message, ModelId, ProviderId, StopReason,
    TextContent, ThinkingLevel, ToolCall, ToolCallId, ToolResultMessage, Usage, UserMessage,
};

use super::*;
use crate::{
    AgentMessage, CompactionSettings, MessageEntry, SessionContextBuildOptions, SessionEntry,
    SessionRecord, build_session_context,
};

#[test]
fn token_estimates_match_executed_legacy_oracle() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/rpc-compaction-oracle.json"
    ))
    .unwrap();
    for case in fixture["tokens"].as_array().unwrap() {
        let message: AgentMessage = serde_json::from_value(case["message"].clone()).unwrap();
        assert_eq!(
            estimate_tokens(&message),
            case["expected"].as_u64().unwrap(),
            "message: {}",
            case["message"]
        );
    }
}

fn usage(total_tokens: u64) -> Usage {
    Usage {
        input: total_tokens,
        total_tokens,
        ..Usage::default()
    }
}

fn user(text: &str) -> AgentMessage {
    Message::User(UserMessage::text(text, 0)).into()
}

fn assistant(text: &str, total_tokens: u64) -> AgentMessage {
    assistant_at(text, total_tokens, 0)
}

fn assistant_at(text: &str, total_tokens: u64, timestamp_ms: i64) -> AgentMessage {
    Message::assistant(AssistantMessage {
        duration_ms: None,
        thinking_level: None,
        content: vec![ContentBlock::Text(TextContent::new(text))],
        api: "scripted".to_string(),
        provider: ProviderId::new("scripted"),
        model: ModelId::new("test"),
        provider_thinking_level: None,
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: usage(total_tokens),
        stop_reason: StopReason::Stop,
        error_message: None,
        deferred: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp_ms,
    })
    .into()
}

fn record(index: usize, entry: SessionEntry) -> SessionRecord {
    SessionRecord {
        id: format!("entry-{index}"),
        seq: index as u64,
        parent_id: index.checked_sub(1).map(|value| format!("entry-{value}")),
        timestamp_ms: index as i64,
        entry,
    }
}

fn message_record(index: usize, message: AgentMessage) -> SessionRecord {
    record(
        index,
        SessionEntry::Message(MessageEntry {
            message,
            terminate: false,
        }),
    )
}

#[test]
fn preparation_projects_once_before_enriching_the_computed_plan() {
    struct RecordingPolicy {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SessionCompactionPolicy for RecordingPolicy {
        fn prepare(&self, entries: &[SessionRecord], preparation: &mut CompactionPreparation) {
            let mut events = self.events.lock().unwrap();
            assert_eq!(*events, ["transform", "project"]);
            assert!(matches!(entries[0].entry, SessionEntry::Custom(_)));
            assert_eq!(preparation.first_kept_entry_id.as_deref(), Some("entry-1"));
            assert_eq!(preparation.tokens_before, 42);
            assert_eq!(preparation.turn_prefix_messages, [user("projected input")]);
            events.push("prepare");
            preparation.file_ops.read.insert("policy metadata".into());
        }
    }

    let events = Arc::new(Mutex::new(Vec::new()));
    let mut options = SessionContextBuildOptions::default();
    let transform_events = events.clone();
    options.entry_transforms.push(Arc::new(move |entries| {
        transform_events.lock().unwrap().push("transform");
        entries.to_vec()
    }));
    let projector_events = events.clone();
    options.entry_projectors.insert(
        "domain-input".into(),
        Arc::new(move |_, _, _| {
            let mut events = projector_events.lock().unwrap();
            assert_eq!(*events, ["transform"]);
            events.push("project");
            vec![user("projected input")]
        }),
    );
    let retained = assistant("retained answer", 42);
    let entries = [
        record(
            0,
            SessionEntry::Custom(crate::CustomEntry {
                custom_type: "domain-input".into(),
                data: None,
            }),
        ),
        message_record(1, retained.clone()),
    ];
    let preparation = prepare_compaction(
        &entries,
        CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        },
        &options,
        &RecordingPolicy {
            events: events.clone(),
        },
    )
    .unwrap();

    assert_eq!(*events.lock().unwrap(), ["transform", "project", "prepare"]);
    assert!(preparation.is_split_turn);
    assert!(preparation.messages_to_summarize.is_empty());
    assert_eq!(preparation.retained_tail, [retained]);
    assert!(preparation.file_ops.read.contains("policy metadata"));
}

#[test]
fn summary_normalization_preserves_extension_roles_and_unpaired_tool_calls() {
    let mut calling = serde_json::to_value(assistant("", 0)).unwrap();
    calling["content"] = json!([
        {"type":"toolCall", "id":"pending", "name":"lookup", "arguments":{"key":"42"}}
    ]);
    let messages = [
        AgentMessage::custom(json!({
            "role":"custom", "content":"domain note", "timestamp":0,
        }))
        .unwrap(),
        AgentMessage::custom(json!({
            "role":"branchSummary", "summary":"branch checkpoint", "timestamp":0,
        }))
        .unwrap(),
        AgentMessage::custom(json!({
            "role":"compactionSummary", "summary":"history checkpoint", "timestamp":0,
        }))
        .unwrap(),
        AgentMessage::custom(json!({
            "role":"bashExecution", "command":"pwd", "output":"/workspace",
            "exitCode":0, "timestamp":0,
        }))
        .unwrap(),
        AgentMessage::custom(json!({
            "role":"bashExecution", "command":"hidden", "output":"excluded output",
            "excludeFromContext":true, "timestamp":0,
        }))
        .unwrap(),
        AgentMessage::custom(json!({
            "role":"vendor-status", "content":"opaque metadata", "timestamp":0,
        }))
        .unwrap(),
        AgentMessage::custom(calling).unwrap(),
        user("continue"),
    ];

    assert_eq!(
        serialize_conversation(&messages),
        concat!(
            "[User]: domain note\n\n",
            "[User]: The following is a summary of a branch that this conversation came back from:\n\n",
            "<summary>\nbranch checkpoint</summary>\n\n",
            "[User]: The conversation history before this point was compacted into the following summary:\n\n",
            "<summary>\nhistory checkpoint\n</summary>\n\n",
            "[User]: Ran `pwd`\n```\n/workspace\n```\n\n",
            "[Assistant tool calls]: lookup(key=\"42\")\n\n",
            "[User]: continue",
        )
    );
}

#[test]
fn estimate_uses_last_valid_usage_plus_trailing_messages() {
    let mut failed = assistant("failed", 9_999);
    if let Some(Message::Assistant(message)) = failed.as_standard() {
        let mut value = (**message).clone();
        value.stop_reason = StopReason::Error;
        failed = Message::assistant(value).into();
    }
    let messages = vec![assistant("done", 100), user("tail"), failed];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 100);
    assert_eq!(estimate.last_usage_index, Some(0));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 100 + estimate.trailing_tokens);
}

#[test]
fn raw_estimate_uses_usage_regardless_of_a_newer_prefix_timestamp() {
    let messages = vec![
        AgentMessage::custom(json!({
            "role": "compactionSummary",
            "summary": "short summary",
            "tokensBefore": 9_500,
            "timestamp": 200,
        }))
        .unwrap(),
        assistant_at("retained answer", 9_500, 100),
        Message::User(UserMessage::text("x".repeat(4_000), 300)).into(),
    ];

    let estimate = estimate_context_tokens(&messages);

    // Raw messages have no provenance. Session projection rejects a retained
    // pre-compaction checkpoint by entry identity, never by wall-clock order.
    assert_eq!(estimate.last_usage_index, Some(1));
    assert_eq!(estimate.usage_tokens, 9_500);
    assert_eq!(estimate.tokens, 10_500);
}

#[test]
fn token_totals_and_thresholds_match_pi_boundaries() {
    assert_eq!(calculate_context_tokens(&usage(42)), 42);
    assert_eq!(
        calculate_context_tokens(&Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            total_tokens: 0,
            ..Usage::default()
        }),
        100
    );
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 200,
        keep_recent_tokens: 50,
    };
    assert!(!should_compact(800, 1_000, settings));
    assert!(should_compact(801, 1_000, settings));
    assert!(!should_compact(
        u64::MAX,
        1_000,
        CompactionSettings {
            enabled: false,
            ..settings
        }
    ));
    for tokens in [0, 1] {
        assert!(should_compact(
            tokens,
            100,
            CompactionSettings {
                reserve_tokens: 200,
                ..settings
            }
        ));
    }
}

#[test]
fn cut_point_never_starts_at_a_tool_result() {
    let entries = [
        message_record(0, user("request")),
        message_record(1, assistant("call", 10)),
        message_record(
            2,
            Message::tool_result(ToolResultMessage {
                duration_ms: None,
                tool_call_id: ToolCallId::new("call-1"),
                tool_name: "read".to_string(),
                content: vec![ContentBlock::Text(TextContent::new("result"))],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp_ms: 0,
            })
            .into(),
        ),
        message_record(3, assistant("finish", 20)),
    ];
    let projected = entries
        .iter()
        .map(|record| {
            let SessionEntry::Message(entry) = &record.entry else {
                unreachable!("message fixture");
            };
            pi_core::session::ProjectedSessionEntry {
                source_entry: record.clone(),
                messages: vec![entry.message.clone()],
            }
        })
        .collect();
    let preparation = algorithm::prepare_projected_compaction(
        projected,
        CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        },
        20,
    )
    .unwrap();
    assert_eq!(preparation.first_kept_entry_id.as_deref(), Some("entry-3"));
    assert_eq!(preparation.retained_tail[0].role(), "assistant");
}

#[test]
fn preparation_carries_previous_summary_and_retained_tail() {
    let previous = record(
        0,
        SessionEntry::Compaction(CompactionEntry {
            first_kept_entry_id: None,
            from_hook: None,
            system_message: None,
            retained_entry_ids: Vec::new(),
            summary: "previous".to_string(),
            retained_tail: vec![user("retained user"), assistant("retained assistant", 50)],
            tokens_before: 1_000,
            details: Some(json!({ "readFiles": ["README.md"], "modifiedFiles": ["src/lib.rs"] })),
            usage: None,
        }),
    );
    let entries = vec![
        previous,
        message_record(1, user("new user")),
        message_record(2, assistant("new assistant", 100)),
    ];
    let preparation = prepare_compaction(
        &entries,
        CompactionSettings {
            keep_recent_tokens: 1,
            ..CompactionSettings::default()
        },
        &SessionContextBuildOptions::default(),
        &GenericCompactionPolicy,
    )
    .unwrap();
    assert_eq!(preparation.previous_summary.as_deref(), Some("previous"));
    assert_eq!(preparation.file_ops, FileOperations::default());
    let all = preparation
        .messages_to_summarize
        .iter()
        .chain(&preparation.turn_prefix_messages)
        .chain(&preparation.retained_tail)
        .map(AgentMessage::role)
        .collect::<Vec<_>>();
    assert_eq!(all, vec!["user", "assistant", "user", "assistant"]);
}

#[test]
fn preparation_requires_compactable_history_after_the_latest_compaction() {
    assert!(
        prepare_compaction(
            &[],
            CompactionSettings::default(),
            &SessionContextBuildOptions::default(),
            &GenericCompactionPolicy,
        )
        .is_none()
    );
    let latest = record(
        0,
        SessionEntry::Compaction(CompactionEntry {
            first_kept_entry_id: None,
            from_hook: None,
            system_message: None,
            retained_entry_ids: Vec::new(),
            summary: "done".to_string(),
            retained_tail: Vec::new(),
            tokens_before: 10,
            details: None,
            usage: None,
        }),
    );
    assert!(
        prepare_compaction(
            &[latest],
            CompactionSettings::default(),
            &SessionContextBuildOptions::default(),
            &GenericCompactionPolicy,
        )
        .is_none()
    );
}

#[test]
fn retained_pre_compaction_usage_is_not_treated_as_current_usage() {
    let retained = assistant("small retained answer", 100_000);
    let entries = vec![record(
        0,
        SessionEntry::Compaction(CompactionEntry {
            first_kept_entry_id: None,
            from_hook: None,
            system_message: None,
            retained_entry_ids: Vec::new(),
            summary: "short summary".to_string(),
            retained_tail: vec![retained],
            tokens_before: 100_000,
            details: None,
            usage: None,
        }),
    )];
    let context = build_session_context(&entries, &SessionContextBuildOptions::default());
    let estimate = estimate_session_context_tokens(&entries, &context.messages);
    assert_eq!(estimate.usage_tokens, 0);
    assert!(estimate.tokens < 100_000);
    assert_eq!(
        current_session_context_tokens(&entries, &context.messages),
        None
    );
}

#[test]
fn post_compaction_assistant_usage_restores_current_context_accounting() {
    let entries = vec![
        record(
            0,
            SessionEntry::Compaction(CompactionEntry {
                first_kept_entry_id: None,
                from_hook: None,
                system_message: None,
                retained_entry_ids: Vec::new(),
                summary: "short summary".to_string(),
                retained_tail: vec![assistant_at("retained", 100_000, 1)],
                tokens_before: 100_000,
                details: None,
                usage: None,
            }),
        ),
        message_record(1, Message::User(UserMessage::text("continue", 3)).into()),
        message_record(2, assistant_at("fresh", 250, 4)),
    ];
    let context = build_session_context(&entries, &SessionContextBuildOptions::default());

    let estimate = current_session_context_tokens(&entries, &context.messages).unwrap();

    assert_eq!(estimate.tokens, 250);
    assert_eq!(estimate.usage_tokens, 250);
}

#[test]
fn summary_serialization_matches_legacy_visible_text_and_utf16_limits() {
    let mut assistant = serde_json::to_value(assistant("", 10)).unwrap();
    assistant["content"] = json!([
        {"type":"thinking", "thinking":"omit this reasoning"},
        {"type":"text", "text":"first"},
        {"type":"text", "text":"second"},
        {"type":"toolCall", "id":"call", "name":"lookup", "arguments":{"key":"42"}}
    ]);
    let messages = [
        AgentMessage::custom(json!({"role":"user", "content":[
            {"type":"text", "text":"hello"}, {"type":"text", "text":" world"}
        ], "timestamp":0}))
        .unwrap(),
        AgentMessage::custom(assistant).unwrap(),
        AgentMessage::custom(
            json!({"role":"toolResult", "toolCallId":"call", "toolName":"lookup", "content":[
            {"type":"text", "text":"found"}, {"type":"text", "text":" it"}
        ], "isError":false, "timestamp":0}),
        )
        .unwrap(),
    ];
    assert_eq!(
        serialize_conversation(&messages),
        "[User]: hello world\n\n[Assistant]: first\nsecond\n\n[Assistant tool calls]: lookup(key=\"42\")\n\n[Tool result]: found it"
    );

    let result = AgentMessage::custom(
        json!({"role":"toolResult", "toolCallId":"call", "toolName":"lookup", "content":[
        {"type":"text", "text":format!("{}🦀tail", "x".repeat(1999))}
    ], "isError":false, "timestamp":0}),
    )
    .unwrap();
    assert_eq!(
        serialize_conversation(&[result]),
        format!(
            "[Tool result]: {}\n\n[... 5 more characters truncated]",
            "x".repeat(1999)
        )
    );
}

#[tokio::test]
async fn incomplete_or_tool_call_summaries_are_rejected() {
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
    let truncated = ScriptedTurn::Events(vec![
        pi_core::StreamEvent::Start {
            metadata: pi_core::ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                0,
            ),
        },
        pi_core::StreamEvent::TextStart { content_index: 0 },
        pi_core::StreamEvent::TextDelta {
            content_index: 0,
            delta: "unfinished checkpoint".to_string(),
        },
        pi_core::StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        pi_core::StreamEvent::Done {
            reason: StopReason::Length,
            usage: Usage::default(),
        },
    ]);
    for (turn, expected) in [
        (
            truncated,
            "Summarization failed: generation hit the token cap and the summary is incomplete",
        ),
        (
            ScriptedTurn::ToolCalls(vec![ToolCall::new("call", "lookup", json!({}))]),
            "Summarization attempted to call a tool",
        ),
    ] {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([turn]))
            .agent_options(pi_agent::AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..pi_agent::AgentOptions::default()
            })
            .build()
            .unwrap();
        let preparation = CompactionPreparation {
            first_kept_entry_id: None,
            messages_to_summarize: vec![user("work")],
            turn_prefix_messages: Vec::new(),
            retained_tail: Vec::new(),
            is_split_turn: false,
            tokens_before: 10,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionSettings::default(),
        };
        let (_, signal) = pi_core::AbortHandle::new();
        let error = compact(
            &preparation,
            &runtime,
            &GenericCompactionPolicy,
            None,
            ThinkingLevel::Off,
            signal,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, CompactionError::SummarizationFailed(ref message) if message == expected)
        );
    }
}

#[test]
fn serialize_truncates_tool_results() {
    let message: AgentMessage = Message::tool_result(ToolResultMessage {
        duration_ms: None,
        tool_call_id: ToolCallId::new("call-1"),
        tool_name: "read".to_string(),
        content: vec![ContentBlock::Text(TextContent::new("x".repeat(5_000)))],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp_ms: 0,
    })
    .into();
    let serialized = serialize_conversation(&[message]);
    assert!(serialized.contains("[... 3000 more characters truncated]"));
}

struct FakeCompleter {
    prompts: Mutex<Vec<CompactionCompletionRequest>>,
    capabilities: CompactionModelCapabilities,
}

#[tokio::test]
async fn summary_request_carries_previous_summary_custom_focus_and_reasoning() {
    let preparation = CompactionPreparation {
        first_kept_entry_id: None,
        messages_to_summarize: vec![user("new work")],
        turn_prefix_messages: Vec::new(),
        retained_tail: vec![assistant("tail", 10)],
        is_split_turn: false,
        tokens_before: 500,
        previous_summary: Some("old summary".to_string()),
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            reserve_tokens: 100,
            keep_recent_tokens: 20,
            enabled: true,
        },
    };
    let completer = FakeCompleter {
        prompts: Mutex::new(Vec::new()),
        capabilities: CompactionModelCapabilities::default(),
    };
    let (_, signal) = pi_core::AbortHandle::new();

    let result = compact(
        &preparation,
        &completer,
        &GenericCompactionPolicy,
        Some("preserve exact errors"),
        ThinkingLevel::High,
        signal,
    )
    .await
    .unwrap();
    let requests = completer.prompts.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].thinking_level, ThinkingLevel::High);
    assert_eq!(requests[0].max_output_tokens, None);
    assert_eq!(requests[0].system_prompt, SUMMARIZATION_SYSTEM_PROMPT);
    assert!(
        requests[0]
            .prompt
            .contains("<previous-summary>\nold summary")
    );
    assert!(
        requests[0]
            .prompt
            .contains("Additional focus: preserve exact errors")
    );
    assert_eq!(result.summary, "## Goal\nKeep working");
    assert_eq!(result.retained_tail, preparation.retained_tail);
}

#[tokio::test]
async fn split_turn_requests_wrap_prefix_and_forward_custom_focus() {
    struct CheckpointPolicy;
    impl SessionCompactionPolicy for CheckpointPolicy {
        fn summary_prompt(&self, _updating: bool) -> &str {
            "Summarize history."
        }

        fn turn_prefix_prompt(&self) -> &str {
            "Summarize earlier messages."
        }
    }
    let preparation = CompactionPreparation {
        first_kept_entry_id: None,
        messages_to_summarize: vec![user("prior work")],
        turn_prefix_messages: vec![user("current request"), assistant("early progress", 10)],
        retained_tail: vec![assistant("later work", 10)],
        is_split_turn: true,
        tokens_before: 500,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings::default(),
    };
    for (instructions, suffix) in [
        (None, ""),
        (Some(""), ""),
        (
            Some("preserve exact errors"),
            "\n\nAdditional focus: preserve exact errors",
        ),
    ] {
        let completer = FakeCompleter {
            prompts: Mutex::new(Vec::new()),
            capabilities: CompactionModelCapabilities::default(),
        };
        let (_, signal) = pi_core::AbortHandle::new();
        compact(
            &preparation,
            &completer,
            &CheckpointPolicy,
            instructions,
            ThinkingLevel::Off,
            signal,
        )
        .await
        .unwrap();
        let requests = completer.prompts.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].prompt,
            format!(
                "<conversation>\n[User]: prior work\n</conversation>\n\nSummarize history.{suffix}"
            )
        );
        // The split-turn wrapper differs from the ordinary summary wrapper
        // in legacy compaction.ts's generateTurnPrefixSummary.
        assert_eq!(
            requests[1].prompt,
            format!(
                "# Conversation\n[User]: current request\n\n[Assistant]: early progress\n\n# Instructions\nSummarize earlier messages.{suffix}"
            )
        );
    }
}

#[tokio::test]
async fn split_turn_without_new_history_preserves_previous_checkpoint() {
    for (previous, history) in [
        (None, "No prior history."),
        (Some("previous checkpoint"), "previous checkpoint"),
        (Some(""), ""),
    ] {
        let preparation = CompactionPreparation {
            first_kept_entry_id: None,
            messages_to_summarize: Vec::new(),
            turn_prefix_messages: vec![user("current request")],
            retained_tail: vec![assistant("later work", 10)],
            is_split_turn: true,
            tokens_before: 500,
            previous_summary: previous.map(str::to_string),
            file_ops: FileOperations::default(),
            settings: CompactionSettings::default(),
        };
        let completer = FakeCompleter {
            prompts: Mutex::new(Vec::new()),
            capabilities: CompactionModelCapabilities::default(),
        };
        let (_, signal) = pi_core::AbortHandle::new();
        let result = compact(
            &preparation,
            &completer,
            &GenericCompactionPolicy,
            None,
            ThinkingLevel::Off,
            signal,
        )
        .await
        .unwrap();
        assert_eq!(completer.prompts.lock().unwrap().len(), 1);
        assert_eq!(
            result.summary,
            format!("{history}\n\n---\n\n**Turn Context (split turn):**\n\n## Goal\nKeep working")
        );
        assert_eq!(result.retained_tail, preparation.retained_tail);
        assert_eq!(result.usage, Some(usage(25)));
    }
}

struct FailingCompleter {
    aborted: bool,
}

#[async_trait]
impl CompactionCompleter for FailingCompleter {
    async fn complete_compaction(
        &self,
        _request: CompactionCompletionRequest,
        _signal: AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError> {
        if self.aborted {
            Err(CompactionError::Aborted("cancelled fixture".to_string()))
        } else {
            Err(CompactionError::SummarizationFailed(
                "failed fixture".to_string(),
            ))
        }
    }
}

#[tokio::test]
async fn summary_failures_and_aborts_are_returned_without_partial_entries() {
    let preparation = CompactionPreparation {
        first_kept_entry_id: None,
        messages_to_summarize: vec![user("work")],
        turn_prefix_messages: Vec::new(),
        retained_tail: Vec::new(),
        is_split_turn: false,
        tokens_before: 10,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings::default(),
    };
    for (aborted, expected) in [(false, "failed fixture"), (true, "cancelled fixture")] {
        let (_, signal) = pi_core::AbortHandle::new();
        let error = compact(
            &preparation,
            &FailingCompleter { aborted },
            &GenericCompactionPolicy,
            None,
            ThinkingLevel::Off,
            signal,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(expected));
        assert_eq!(matches!(error, CompactionError::Aborted(_)), aborted);
    }
}

#[async_trait]
impl CompactionCompleter for FakeCompleter {
    fn model_capabilities(&self) -> CompactionModelCapabilities {
        self.capabilities
    }

    async fn complete_compaction(
        &self,
        request: CompactionCompletionRequest,
        _signal: AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError> {
        self.prompts.lock().unwrap().push(request);
        Ok(CompactionCompletion {
            text: "## Goal\nKeep working".to_string(),
            usage: usage(25),
        })
    }
}

#[tokio::test]
async fn summaries_leave_output_limits_to_provider_and_respect_reasoning() {
    let preparation = CompactionPreparation {
        first_kept_entry_id: None,
        messages_to_summarize: vec![user("history")],
        turn_prefix_messages: vec![user("large turn")],
        retained_tail: vec![assistant("tail", 10)],
        is_split_turn: true,
        tokens_before: 600_000,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            reserve_tokens: 500_000,
            keep_recent_tokens: 20_000,
            enabled: true,
        },
    };
    let completer = FakeCompleter {
        prompts: Mutex::new(Vec::new()),
        capabilities: CompactionModelCapabilities {
            reasoning: false,
            max_output_tokens: Some(128_000),
        },
    };
    let (_, signal) = pi_core::AbortHandle::new();

    compact(
        &preparation,
        &completer,
        &GenericCompactionPolicy,
        None,
        ThinkingLevel::High,
        signal,
    )
    .await
    .unwrap();

    let requests = completer.prompts.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.max_output_tokens.is_none())
    );
    assert!(
        requests
            .iter()
            .all(|request| request.thinking_level == ThinkingLevel::Off)
    );
}

#[tokio::test]
async fn generic_split_turn_summarizes_without_interpreting_coding_tools() {
    let mut calling = assistant("", 50);
    if let Some(Message::Assistant(message)) = calling.as_standard() {
        let mut value = (**message).clone();
        value.content = vec![ContentBlock::ToolCall(ToolCall::new(
            "call-1",
            "edit",
            json!({"path":"src/lib.rs"}),
        ))];
        calling = Message::assistant(value).into();
    }
    let preparation = CompactionPreparation {
        first_kept_entry_id: None,
        messages_to_summarize: vec![user("old"), calling],
        turn_prefix_messages: vec![user("large turn prefix")],
        retained_tail: vec![assistant("suffix", 50)],
        is_split_turn: true,
        tokens_before: 500,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings::default(),
    };
    let completer = Arc::new(FakeCompleter {
        prompts: Mutex::new(Vec::new()),
        capabilities: CompactionModelCapabilities::default(),
    });
    let (_, signal) = pi_core::AbortHandle::new();
    let result = compact(
        &preparation,
        completer.as_ref(),
        &GenericCompactionPolicy,
        None,
        ThinkingLevel::Off,
        signal,
    )
    .await
    .unwrap();
    assert_eq!(completer.prompts.lock().unwrap().len(), 2);
    assert!(result.summary.contains("Turn Context (split turn)"));
    assert!(!result.summary.contains("<modified-files>"));
    assert_eq!(result.details, None);
    assert_eq!(result.usage.unwrap().total_tokens, 50);
}
