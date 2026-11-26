//! Stateful HTTP recovery through ProductSessionFactory and the real Coding loop.
//! Oracle: MCP runtime.ts::withClient retries once only after typed session expiry
//! (pre-execution404); ambiguous tool failures are not replayed. Frozen descriptor
//! fencing and complete next-idle publication are deliberate Rust adaptations.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_core::{AgentEvent, ContentBlock, Message, ToolSpec};
use pi_session::{
    AgentSessionEvent, CreateSessionOptions, MultiSessionManager, SessionEntry, SessionStore,
};
use serde_json::{Value, json};

mod fixture;
use fixture::{Fixture, descriptor};

const TIMEOUT: Duration = Duration::from_secs(30);
const TOOL: &str = "mcp__fixture__echo";
const CALL_ID: &str = "original-recovery-call";
const OLD_GUIDANCE: &str = "Recovery fixture initial instructions.";
const NEW_GUIDANCE: &str = "Recovery fixture replacement instructions.";

fn tool_spec(session: &pi_session::AgentSession) -> ToolSpec {
    session
        .catalog()
        .tool_specs()
        .into_iter()
        .find(|tool| tool.name == TOOL)
        .expect("direct MCP tool")
}

fn assert_spec(spec: &ToolSpec, changed: bool) {
    let remote = descriptor(changed);
    assert_eq!(spec.parameters, remote["inputSchema"]);
    let output = spec.output_schema.as_ref().expect("MCP result envelope");
    assert_eq!(
        output["properties"]["structuredContent"],
        remote["outputSchema"]
    );
    assert_eq!(output["required"], json!(["content"]));
    assert_eq!(spec.description, remote["description"].as_str().unwrap());
}

fn assert_guidance(request: &Value, replaced: bool) {
    let prompt = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| matches!(message["role"].as_str(), Some("system" | "developer")))
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let (expected, absent) = if replaced {
        (NEW_GUIDANCE, OLD_GUIDANCE)
    } else {
        (OLD_GUIDANCE, NEW_GUIDANCE)
    };
    assert_eq!(prompt.matches(expected).count(), 1, "{prompt}");
    assert!(!prompt.contains(absent), "{prompt}");
}

fn assert_single_pair(messages: &[Message], stale: bool) {
    let calls = messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(message) => Some(message.content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    let results = messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1, "one original logical call");
    assert_eq!(
        results.len(),
        1,
        "one matching result, never a dangling call"
    );
    assert_eq!(calls[0].id.as_str(), CALL_ID);
    assert_eq!(calls[0].name, TOOL);
    assert_eq!(calls[0].arguments, json!({"value":"original"}));
    assert_eq!(results[0].tool_call_id, calls[0].id);
    assert_eq!(results[0].tool_name, TOOL);
    assert_eq!(results[0].is_error, stale);
    let text = results[0]
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(content) => Some(content.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if stale {
        assert!(
            text.contains("MCP callable inventory changed; retry after the next idle boundary"),
            "{text}"
        );
        assert!(text.len() <= 512, "stale outcome must remain bounded");
    } else {
        assert!(text.contains("executed once on replacement"), "{text}");
    }
}

fn assert_disk_pair(fixture: &Fixture, stale: bool) {
    // Reopen the actual JSONL, rather than inspecting only the runtime's memory.
    let document = SessionStore::open_handle(fixture.session.path())
        .unwrap()
        .load()
        .unwrap();
    let messages = document
        .entries
        .into_iter()
        .filter_map(|record| match record.entry {
            SessionEntry::Message(entry) => entry.message.as_standard().cloned(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_single_pair(&messages, stale);
}

async fn recovery_case(changed: bool) {
    let fixture = Fixture::new(changed).await;
    let active = fixture.session.current();
    let specs = active.catalog().tool_specs();
    let original = tool_spec(&active);
    assert_spec(&original, false);
    let mut events = active.subscribe().events;
    let turn = tokio::spawn({
        let active = active.clone();
        async move { active.prompt("call the fixture exactly once").await }
    });
    fixture.until(|trace| trace.attempts.len() == 1).await;
    assert!(!active.is_idle());
    assert!(Arc::ptr_eq(&active, &fixture.session.current()));
    assert_eq!(fixture.generations.load(Ordering::Acquire), 1);
    assert!(fixture.snapshot().executions.is_empty());
    assert_guidance(&fixture.snapshot().requests[0], false);
    fixture.expire.add_permits(1);

    // The provider continuation can start only after the logical operation has
    // settled and its matching result was persisted/projected. Supervised recovery
    // installs Ready inventory and its revision BEFORE returning either successful
    // execution or typed InventoryChanged. This is client/runtime evidence, not a
    // server response-sent flag or access to ProductSessionFactory's private watch.
    fixture.until(|trace| trace.requests.len() == 2).await;
    let trace = fixture.snapshot();
    assert_eq!(
        trace.initializes, 2,
        "one replacement handshake before outcome"
    );
    assert_eq!(
        trace.lists,
        ["connection-1", "connection-2"],
        "complete replacement discovery before outcome"
    );
    let expected_attempts = if changed {
        vec!["connection-1"]
    } else {
        vec!["connection-1", "connection-2"]
    };
    assert_eq!(
        trace
            .attempts
            .iter()
            .map(|(session, _)| session.as_str())
            .collect::<Vec<_>>(),
        expected_attempts
    );
    assert!(
        trace
            .attempts
            .iter()
            .all(|(_, arguments)| *arguments == json!({"value":"original"}))
    );
    assert_eq!(
        trace.executions,
        if changed {
            vec![]
        } else {
            vec!["connection-2"]
        }
    );
    assert_disk_pair(&fixture, changed);
    let provider_results = trace.requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(provider_results.len(), 1);
    assert_eq!(provider_results[0]["tool_call_id"], CALL_ID);
    assert_eq!(
        trace.requests[0]["tools"], trace.requests[1]["tools"],
        "active provider declaration stays frozen through recovery"
    );
    assert_guidance(&trace.requests[1], false);
    assert!(!active.is_idle());
    assert_eq!(active.catalog().tool_specs(), specs);
    assert!(Arc::ptr_eq(&active, &fixture.session.current()));
    assert_eq!(fixture.generations.load(Ordering::Acquire), 1);
    fixture.finish_turn.add_permits(1);
    tokio::time::timeout(TIMEOUT, turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(active.is_idle());
    assert!(
        Arc::ptr_eq(&active, &fixture.session.current()),
        "turn completion must not replace the active generation"
    );

    let (mut starts, mut ends, mut emitted) = (0, 0, Vec::new());
    loop {
        let event = match events.try_recv() {
            Ok(event) => event.event,
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
            Err(error) => panic!("lost runtime evidence: {error}"),
        };
        if let AgentSessionEvent::Agent(event) = event {
            match *event {
                AgentEvent::ToolExecutionStart {
                    tool_call_id,
                    tool_name,
                    ..
                } => {
                    assert_eq!(tool_call_id.as_str(), CALL_ID);
                    assert_eq!(tool_name, TOOL);
                    starts += 1;
                }
                AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    tool_name,
                    is_error,
                    ..
                } => {
                    assert_eq!(tool_call_id.as_str(), CALL_ID);
                    assert_eq!(tool_name, TOOL);
                    assert_eq!(is_error, changed);
                    ends += 1;
                }
                AgentEvent::MessageEnd { message } => emitted.push(message),
                _ => {}
            }
        }
    }
    assert_eq!(
        (starts, ends),
        (1, 1),
        "wire replay is not a second logical execution"
    );
    assert_single_pair(&emitted, changed);

    tokio::time::timeout(TIMEOUT, active.prompt("next idle: describe only"))
        .await
        .unwrap()
        .unwrap();
    let replacement = fixture.session.current();
    assert!(!Arc::ptr_eq(&active, &replacement));
    assert_eq!(fixture.generations.load(Ordering::Acquire), 2);
    assert_spec(&tool_spec(&replacement), changed);
    assert_eq!(
        active.catalog().tool_specs(),
        specs,
        "retained generation remains frozen"
    );
    assert_guidance(&fixture.snapshot().requests[2], true);
    tokio::time::timeout(TIMEOUT, replacement.prompt("second idle: describe only"))
        .await
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&replacement, &fixture.session.current()));
    assert_eq!(
        fixture.generations.load(Ordering::Acquire),
        2,
        "published recovery must not stale itself"
    );
    let final_trace = fixture.snapshot();
    assert_eq!(final_trace.requests.len(), 4);
    assert_eq!(final_trace.initializes, 2);
    assert_eq!(final_trace.lists.len(), 2);
    assert_eq!(final_trace.attempts.len(), expected_attempts.len());
    assert_eq!(final_trace.executions.len(), usize::from(!changed));
    assert_guidance(&final_trace.requests[3], true);
    let new_tool = final_trace.requests[2]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == TOOL)
        .unwrap();
    assert_eq!(
        new_tool["function"]["parameters"]["properties"]["value"]["type"],
        if changed { "number" } else { "string" }
    );
    assert_eq!(
        final_trace.requests[2]["tools"],
        final_trace.requests[3]["tools"]
    );
    assert_disk_pair(&fixture, changed);
    fixture.manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_session_reopens_once_and_preserves_one_logical_call_until_next_idle() {
    recovery_case(false).await;
}

#[tokio::test]
async fn expired_session_changed_descriptor_defers_dispatch_and_publishes_at_next_idle() {
    recovery_case(true).await;
}
