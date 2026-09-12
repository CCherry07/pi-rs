use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pi_core::{AgentEvent, ContentBlock, Message, StreamEvent, ToolResult};
use pi_session::{AgentSessionEvent, RevisionedAgentSessionEvent, SessionEntry};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, Mutex};

use crate::backend::events::PiEvent;

use super::session_store::{content_text, LiveSession, SessionStore};

pub(crate) type ForwarderRegistry = Arc<Mutex<HashSet<String>>>;

struct EventProjectionContext<'a> {
    app: &'a AppHandle,
    workspace_id: &'a str,
    thread_id: &'a str,
    live: &'a LiveSession,
}

#[derive(Default)]
struct ProjectionState {
    turn_id: Option<String>,
    messages: MessageProjection,
    tool_names: HashMap<String, String>,
    tool_args: HashMap<String, Value>,
    compaction_id: Option<String>,
}

#[derive(Default)]
struct MessageProjection {
    next_index: usize,
    last_index: Option<usize>,
}

impl MessageProjection {
    fn from_snapshot(snapshot: &pi_session::AgentSessionSnapshot) -> Self {
        let next_index = snapshot.agent.messages.len();
        Self {
            next_index,
            last_index: next_index.checked_sub(1),
        }
    }

    fn start(&self, message: &Message) -> Vec<Value> {
        // Assistant content is emitted per block by update/end, not twice at start.
        if message.is_assistant() {
            return Vec::new();
        }
        super::message_items(message, self.next_index, None, None)
    }

    fn update(
        &self,
        update: &StreamEvent,
        stream: &pi_core::AssistantStream,
    ) -> Option<(&'static str, Value)> {
        let (method, kind, content_index) = match update {
            StreamEvent::TextDelta { content_index, .. } => {
                ("item/agentMessage/delta", "agent", *content_index)
            }
            StreamEvent::ThinkingDelta { content_index, .. } => {
                ("item/reasoning/textDelta", "reasoning", *content_index)
            }
            _ => return None,
        };
        // A readThread snapshot may be ahead of queued events. Cumulative blocks
        // make those updates idempotent with snapshot hydration at the same ID.
        let message = stream.snapshot()?;
        let text = match message.content.get(content_index)? {
            ContentBlock::Text(text) => &text.text,
            ContentBlock::Thinking(thinking) => &thinking.thinking,
            _ => return None,
        };
        Some((
            method,
            json!({
                "itemId": super::message_item_id(kind, self.next_index, content_index),
                "delta": text,
            }),
        ))
    }

    fn end(&mut self, message: &Message) -> Vec<Value> {
        let index = self.next_index;
        self.next_index += 1;
        self.last_index = Some(index);
        super::message_items(message, index, None, None)
    }

    fn entry(&self, entry: &pi_session::SessionRecord) -> Vec<Value> {
        let Some(index) = self.last_index else {
            return Vec::new();
        };
        let SessionEntry::Message(message_entry) = &entry.entry else {
            return Vec::new();
        };
        let Some(message) = message_entry.message.as_standard() else {
            return Vec::new();
        };
        // Persistence follows MessageEnd, including when the end was in the initial
        // snapshot but EntryAppended is still pending in the subscription.
        super::message_items(
            message,
            index,
            Some(&entry.id),
            message_entry.message.display_text(),
        )
    }
}

impl ProjectionState {
    fn from_snapshot(snapshot: &pi_session::AgentSessionSnapshot, thread_id: &str) -> Self {
        let mut state = Self {
            messages: MessageProjection::from_snapshot(snapshot),
            turn_id: snapshot
                .agent
                .is_running
                .then(|| format!("pi-turn-{thread_id}")),
            ..Self::default()
        };
        // Covered ToolExecutionStart events are not replayed. Retain pending
        // arguments so their later update/end still projects the original call.
        for message in &snapshot.agent.messages {
            match message {
                Message::Assistant(message) => {
                    for call in message.tool_calls() {
                        state.tool_names.insert(call.id.to_string(), call.name);
                        state.tool_args.insert(call.id.to_string(), call.arguments);
                    }
                }
                Message::ToolResult(result) => {
                    state.tool_names.remove(result.tool_call_id.as_str());
                    state.tool_args.remove(result.tool_call_id.as_str());
                }
                _ => {}
            }
        }
        state
    }
}

#[derive(Default)]
struct ContextRefresh {
    after_context_change: bool,
}

impl ContextRefresh {
    fn before_event(&mut self, event: &AgentSessionEvent) -> bool {
        // Idle mutations may predate the subscription, and end hooks may remove
        // streamed blocks. Both prompt boundaries publish authoritative Agent state.
        if matches!(event, AgentSessionEvent::Agent(event) if matches!(event.as_ref(), AgentEvent::AgentStart))
            || matches!(event, AgentSessionEvent::AgentSettled)
        {
            self.after_context_change = false;
            return true;
        }
        if let AgentSessionEvent::EntryAppended { entry } = event {
            let changes_context = match &entry.entry {
                SessionEntry::CustomMessage(_) | SessionEntry::BranchSummary(_) => true,
                SessionEntry::Message(message) => message.message.as_standard().is_none(),
                _ => false,
            };
            if changes_context {
                self.after_context_change = true;
            }
            return false;
        }
        if matches!(
            event,
            AgentSessionEvent::CompactionEnd {
                aborted: false,
                error_message: None,
                result: Some(_),
                ..
            }
        ) {
            self.after_context_change = true;
            return false;
        }
        // CompactionEnd does not publish the new Agent state. The next Agent
        // event is the first reliable snapshot boundary for immediate activation.
        if self.after_context_change
            && matches!(
                event,
                AgentSessionEvent::Agent(_)
                    | AgentSessionEvent::AgentEnd { .. }
                    | AgentSessionEvent::AgentSettled
            )
        {
            self.after_context_change = false;
            return true;
        }
        false
    }
}

fn snapshot_covers_event(event: &AgentSessionEvent) -> bool {
    match event {
        AgentSessionEvent::EntryAppended { .. } => true,
        AgentSessionEvent::Agent(event) => matches!(
            event.as_ref(),
            AgentEvent::MessageStart { .. }
                | AgentEvent::MessageUpdate { .. }
                | AgentEvent::MessageEnd { .. }
                | AgentEvent::ToolExecutionStart { .. }
                | AgentEvent::ToolExecutionUpdate { .. }
                | AgentEvent::ToolExecutionEnd { .. }
        ),
        _ => false,
    }
}

fn turn_notification(thread_id: &str, started: bool) -> (&'static str, Value) {
    let method = if started {
        "turn/started"
    } else {
        "turn/completed"
    };
    (
        method,
        json!({
            "threadId": thread_id,
            "turn": { "id": format!("pi-turn-{thread_id}"), "status": if started { "inProgress" } else { "completed" } }
        }),
    )
}

pub(crate) fn spawn_session_forwarder(
    app: AppHandle,
    workspace_id: String,
    thread_id: String,
    key: String,
    live: LiveSession,
    store: SessionStore,
    forwarders: ForwarderRegistry,
) {
    tauri::async_runtime::spawn(async move {
        forward_session_events(app, workspace_id, thread_id, key, live, store, forwarders).await;
    });
}

async fn forward_session_events(
    app: AppHandle,
    workspace_id: String,
    thread_id: String,
    key: String,
    mut live: LiveSession,
    store: SessionStore,
    forwarders: ForwarderRegistry,
) {
    let mut state = ProjectionState::from_snapshot(&live.presentation_snapshot(), &thread_id);
    let mut thread_id = thread_id;
    let mut changes = live.changes.take();
    let mut context_refresh = ContextRefresh::default();
    let mut initial_revision = live.subscription.snapshot.revision;
    loop {
        let event = tokio::select! {
            biased;
            changed = async {
                match &mut changes {
                    Some(receiver) => receiver.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() { break; }
                let current = Arc::clone(&changes.as_mut().unwrap().borrow_and_update());
                let previous_id = thread_id;
                thread_id = current.log().header().id;
                // The watch may coalesce replacements. Rehydrate the latest session
                // at its new subscription boundary instead of replaying old streams.
                live.replace(current);
                initial_revision = live.subscription.snapshot.revision;
                context_refresh = ContextRefresh::default();
                state = ProjectionState::from_snapshot(&live.presentation_snapshot(), &thread_id);
                emit(&app, &workspace_id, "thread/replaced", json!({
                    "previousThreadId": previous_id,
                    "generationChanged": true,
                    "thread": super::thread_from_subscription(&live),
                }));
                continue;
            }
            event = live.subscription.events.recv() => event,
        };
        match event {
            Ok(event) if event.revision <= initial_revision => continue,
            Ok(event)
                if event.revision <= live.subscription.snapshot.revision
                    && snapshot_covers_event(&event.event) =>
            {
                continue;
            }
            Ok(event) => {
                let should_refresh = context_refresh.before_event(&event.event);
                project_event(
                    EventProjectionContext {
                        app: &app,
                        workspace_id: &workspace_id,
                        thread_id: &thread_id,
                        live: &live,
                    },
                    &mut state,
                    event,
                )
                .await;
                if should_refresh {
                    if let Some(thread) =
                        refresh_thread_snapshot(&mut live, &store, &thread_id).await
                    {
                        state = ProjectionState::from_snapshot(
                            &live.presentation_snapshot(),
                            &thread_id,
                        );
                        emit(
                            &app,
                            &workspace_id,
                            "thread/replaced",
                            json!({
                                "previousThreadId": thread_id, "thread": thread,
                            }),
                        );
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                if let Some(thread) = refresh_thread_snapshot(&mut live, &store, &thread_id).await {
                    state =
                        ProjectionState::from_snapshot(&live.presentation_snapshot(), &thread_id);
                    emit(
                        &app,
                        &workspace_id,
                        "thread/replaced",
                        json!({
                            "previousThreadId": thread_id, "thread": thread,
                        }),
                    );
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    forwarders.lock().await.remove(&key);
}

async fn project_event(
    context: EventProjectionContext<'_>,
    state: &mut ProjectionState,
    event: RevisionedAgentSessionEvent,
) {
    let EventProjectionContext {
        app,
        workspace_id,
        thread_id,
        live,
    } = context;
    match event.event {
        AgentSessionEvent::Agent(agent_event) => match *agent_event {
            AgentEvent::AgentStart => {
                let turn_id = format!("pi-turn-{thread_id}");
                state.turn_id = Some(turn_id.clone());
                let (method, params) = turn_notification(thread_id, true);
                emit(app, workspace_id, method, params);
                emit_status(app, workspace_id, thread_id, "active");
            }
            AgentEvent::MessageStart { message } => {
                for item in state.messages.start(&message) {
                    emit(
                        app,
                        workspace_id,
                        "item/completed",
                        json!({
                            "threadId": thread_id, "turnId": state.turn_id, "item": item,
                        }),
                    );
                }
            }
            AgentEvent::MessageUpdate { update, stream } => {
                if let Some((method, mut params)) = state.messages.update(&update, &stream) {
                    params["threadId"] = json!(thread_id);
                    params["turnId"] = json!(state.turn_id);
                    emit(app, workspace_id, method, params);
                }
            }
            AgentEvent::MessageEnd { message } => {
                for item in state.messages.end(&message) {
                    emit(
                        app,
                        workspace_id,
                        "item/completed",
                        json!({
                            "threadId": thread_id, "turnId": state.turn_id, "item": item,
                        }),
                    );
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                let item_id = tool_call_id.to_string();
                state.tool_names.insert(item_id.clone(), tool_name.clone());
                state.tool_args.insert(item_id.clone(), args.clone());
                let item = tool_item(
                    &item_id,
                    &tool_name,
                    args,
                    "inProgress",
                    None,
                    live.cwd().to_string_lossy().as_ref(),
                );
                emit(
                    app,
                    workspace_id,
                    "item/started",
                    json!({
                        "threadId": thread_id,
                        "turnId": state.turn_id,
                        "item": item
                    }),
                );
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                partial_result,
                ..
            } => {
                let item_id = tool_call_id.to_string();
                let args = state.tool_args.get(&item_id).cloned().unwrap_or_default();
                let item = tool_result_item(
                    &item_id,
                    &tool_name,
                    args,
                    "inProgress",
                    &partial_result,
                    live.cwd().to_string_lossy().as_ref(),
                );
                emit(
                    app,
                    workspace_id,
                    "item/started",
                    json!({ "threadId": thread_id, "turnId": state.turn_id, "item": item }),
                );
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let item_id = tool_call_id.to_string();
                let args = state.tool_args.remove(&item_id).unwrap_or_default();
                let item = tool_result_item(
                    &item_id,
                    &tool_name,
                    args,
                    if is_error { "failed" } else { "completed" },
                    &result,
                    live.cwd().to_string_lossy().as_ref(),
                );
                emit(
                    app,
                    workspace_id,
                    "item/completed",
                    json!({
                        "threadId": thread_id,
                        "turnId": state.turn_id,
                        "item": item
                    }),
                );
                state.tool_names.remove(&item_id);
            }
            AgentEvent::AgentEnd { .. } | AgentEvent::TurnStart | AgentEvent::TurnEnd { .. } => {}
        },
        AgentSessionEvent::AgentEnd {
            messages,
            will_retry,
        } => {
            if let Some(Message::Assistant(message)) = messages.last() {
                emit(
                    app,
                    workspace_id,
                    "thread/tokenUsage/updated",
                    json!({
                        "threadId": thread_id,
                        "tokenUsage": live.token_usage()
                    }),
                );
                if let Some(error) = &message.error_message {
                    emit(
                        app,
                        workspace_id,
                        "error",
                        json!({
                            "threadId": thread_id,
                            "turnId": state.turn_id,
                            "error": { "message": error },
                            "willRetry": will_retry
                        }),
                    );
                }
            }
        }
        AgentSessionEvent::AgentSettled => {
            state.turn_id = None;
            let (method, params) = turn_notification(thread_id, false);
            emit(app, workspace_id, method, params);
            emit_status(app, workspace_id, thread_id, "idle");
        }
        AgentSessionEvent::UsageRecorded { .. } => emit(
            app,
            workspace_id,
            "thread/tokenUsage/updated",
            json!({
                "threadId": thread_id,
                "tokenUsage": live.token_usage()
            }),
        ),
        AgentSessionEvent::EntryAppended { entry } => {
            if let Some((key, value)) = super::desktop_views::widget_update(&entry) {
                emit(
                    app,
                    workspace_id,
                    "thread/widgetUpdated",
                    json!({
                        "threadId": thread_id, "key": key, "value": value,
                        "version": entry.seq,
                    }),
                );
            }
            for item in state.messages.entry(&entry) {
                emit(
                    app,
                    workspace_id,
                    "item/completed",
                    json!({
                        "threadId": thread_id, "turnId": state.turn_id, "item": item,
                    }),
                );
            }
        }
        AgentSessionEvent::PluginNotice { message, level } => emit(
            app,
            workspace_id,
            "thread/notice",
            notice_params(thread_id, &message, level),
        ),
        AgentSessionEvent::SessionInfoChanged { name } => emit(
            app,
            workspace_id,
            "thread/name/updated",
            json!({ "threadId": thread_id, "threadName": name }),
        ),
        AgentSessionEvent::AutoRetryStart { error_message, .. } => emit(
            app,
            workspace_id,
            "error",
            json!({
                "threadId": thread_id,
                "turnId": state.turn_id,
                "error": { "message": error_message },
                "willRetry": true
            }),
        ),
        AgentSessionEvent::CompactionStart { .. } => {
            let item_id = format!("compaction-{}", event.revision);
            state.compaction_id = Some(item_id.clone());
            emit(
                app,
                workspace_id,
                "item/started",
                json!({
                    "threadId": thread_id,
                    "turnId": state.turn_id,
                    "item": {
                        "id": item_id,
                        "type": "contextCompaction",
                        "status": "inProgress"
                    }
                }),
            );
        }
        AgentSessionEvent::CompactionEnd {
            aborted,
            error_message,
            ..
        } => {
            let item_id = state
                .compaction_id
                .take()
                .unwrap_or_else(|| format!("compaction-{}", event.revision));
            emit(
                app,
                workspace_id,
                "item/completed",
                json!({
                    "threadId": thread_id,
                    "turnId": state.turn_id,
                    "item": {
                        "id": item_id,
                        "type": "contextCompaction",
                        "status": if aborted || error_message.is_some() { "failed" } else { "completed" }
                    }
                }),
            );
        }
        _ => {}
    }
}

fn tool_item(
    id: &str,
    name: &str,
    arguments: Value,
    status: &str,
    output: Option<String>,
    cwd: &str,
) -> Value {
    if name == "bash" {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or(name);
        json!({
            "id": id,
            "type": "commandExecution",
            "toolName": name,
            "arguments": arguments,
            "command": command,
            "cwd": cwd,
            "status": status,
            "aggregatedOutput": output.unwrap_or_default()
        })
    } else {
        json!({
            "id": id,
            "type": "mcpToolCall",
            "toolName": name,
            "server": "pi",
            "tool": name,
            "arguments": arguments,
            "status": status,
            "result": output.unwrap_or_default()
        })
    }
}

fn tool_result_item(
    id: &str,
    name: &str,
    arguments: Value,
    status: &str,
    result: &ToolResult,
    cwd: &str,
) -> Value {
    let mut item = tool_item(
        id,
        name,
        arguments,
        status,
        Some(tool_result_text(result)),
        cwd,
    );
    item["details"] = json!(result.details);
    item
}

fn tool_result_text(result: &ToolResult) -> String {
    content_text(&result.content)
}

fn notice_params(thread_id: &str, message: &str, level: pi_core::NoticeLevel) -> Value {
    json!({ "threadId": thread_id, "message": message, "level": level })
}

async fn refresh_thread_snapshot(
    live: &mut LiveSession,
    store: &SessionStore,
    thread_id: &str,
) -> Option<Value> {
    live.refresh_subscription();
    Some(if live.primary().is_some() {
        super::thread_from_subscription(live)
    } else {
        let observed = store.observed_isolated(thread_id)?;
        super::thread_from_isolated_snapshot(
            &live.subscription.snapshot,
            &observed.observation,
            &observed.parent_thread_id,
            &observed.agent,
            observed.nickname.as_deref(),
        )
    })
}

fn emit_status(app: &AppHandle, workspace_id: &str, thread_id: &str, status: &str) {
    emit(
        app,
        workspace_id,
        "thread/status/changed",
        json!({ "threadId": thread_id, "status": { "type": status } }),
    );
}

fn emit(app: &AppHandle, workspace_id: &str, method: &str, params: Value) {
    let _ = app.emit(
        "pi-event",
        PiEvent {
            workspace_id: workspace_id.to_string(),
            message: json!({ "method": method, "params": params }),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(content: Vec<ContentBlock>, timestamp_ms: i64) -> Message {
        Message::assistant(pi_core::AssistantMessage {
            content,
            api: "scripted".into(),
            provider: pi_core::ProviderId::new("scripted"),
            model: pi_core::ModelId::new("test"),
            response_model: None,
            response_id: None,
            usage: Default::default(),
            stop_reason: pi_core::StopReason::Stop,
            diagnostics: None,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms,
        })
    }

    fn snapshot(messages: Vec<Message>) -> pi_session::AgentSessionSnapshot {
        pi_session::AgentSessionSnapshot {
            revision: 0,
            agent: pi_agent::AgentStateSnapshot {
                system_prompt: String::new(),
                provider_id: pi_core::ProviderId::new("scripted"),
                model_id: pi_core::ModelId::new("test"),
                thinking_level: pi_core::ThinkingLevel::Off,
                active_tools: Vec::new(),
                messages,
                is_running: false,
                streaming_message: None,
                pending_tool_calls: Default::default(),
                error_message: None,
            },
            queue: Default::default(),
            compaction: None,
            auto_retry: None,
            bash: None,
            name: None,
        }
    }

    fn snapshot_items(snapshot: &pi_session::AgentSessionSnapshot) -> Vec<Value> {
        let thread = super::super::thread_from_snapshot(
            snapshot,
            "child",
            std::path::Path::new("/project"),
            Default::default(),
            json!("pi-rs"),
            None,
            Default::default(),
        );
        thread["turns"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|turn| turn["items"].as_array().unwrap().clone())
            .collect()
    }

    fn block_text(value: &str) -> ContentBlock {
        ContentBlock::Text(pi_core::TextContent::new(value))
    }

    fn block_thinking(value: &str) -> ContentBlock {
        ContentBlock::Thinking(pi_core::ThinkingContent {
            thinking: value.into(),
            thinking_signature: None,
            redacted: None,
        })
    }

    #[test]
    fn message_projection_uses_all_message_slots_and_keeps_text_blocks_separate() {
        let user = Message::User(pi_core::UserMessage::text("task", 42));
        let hidden = Message::custom(pi_core::CustomMessage {
            custom_type: "context".into(),
            content: pi_core::CustomMessageContent::Text("hidden".into()),
            display: false,
            details: None,
            timestamp_ms: 42,
        });
        let result = Message::tool_result(pi_core::ToolResultMessage {
            tool_call_id: pi_core::ToolCallId::new("call"),
            tool_name: "read".into(),
            content: vec![block_text("tool output")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 42,
        });
        let answer = assistant(
            vec![
                block_text("same"),
                block_thinking("reason"),
                block_text("same"),
            ],
            42,
        );
        let messages = vec![user, hidden, result, answer.clone(), answer];
        let mut projection = MessageProjection::default();
        let mut live_items = Vec::new();
        for message in &messages {
            live_items.extend(projection.end(message));
        }
        let historical = snapshot_items(&snapshot(messages));
        let historical_messages = historical
            .into_iter()
            .filter(|item| item["type"] != "mcpToolCall")
            .collect::<Vec<_>>();
        assert_eq!(live_items, historical_messages);
        assert_eq!(
            live_items
                .iter()
                .map(|item| item["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "user-0-0",
                "agent-3-0",
                "reasoning-3-1",
                "agent-3-2",
                "agent-4-0",
                "reasoning-4-1",
                "agent-4-2"
            ]
        );
        let mut projection = MessageProjection {
            next_index: 3,
            last_index: Some(2),
        };
        let Message::Assistant(message) = assistant(
            vec![
                block_text("same"),
                block_thinking("reason"),
                block_text("same"),
            ],
            42,
        ) else {
            unreachable!()
        };
        let stream = pi_core::AssistantStream::new(
            pi_core::AssistantStreamId::new("blocks"),
            Arc::new(MutableStream(std::sync::Mutex::new((*message).clone()))),
        );
        for index in [0, 2] {
            let (_, delta) = projection
                .update(
                    &StreamEvent::TextDelta {
                        content_index: index,
                        delta: "same".into(),
                    },
                    &stream,
                )
                .unwrap();
            assert_eq!(delta["itemId"], format!("agent-3-{index}"));
        }
        let (_, delta) = projection
            .update(
                &StreamEvent::ThinkingDelta {
                    content_index: 1,
                    delta: "reason".into(),
                },
                &stream,
            )
            .unwrap();
        assert_eq!(delta["itemId"], "reasoning-3-1");
        // The provider/end hook may revise timestamp metadata; identity does not change.
        assert_eq!(
            projection.end(&assistant(vec![block_text("same")], 99_999))[0]["id"],
            "agent-3-0"
        );
    }

    struct MutableStream(std::sync::Mutex<pi_core::AssistantMessage>);
    impl pi_core::AssistantStreamView for MutableStream {
        fn snapshot(&self) -> Option<pi_core::AssistantMessage> {
            Some(self.0.lock().unwrap().clone())
        }
    }

    #[test]
    fn midstream_snapshot_and_pending_end_share_ids_without_double_counting_stream() {
        let Message::Assistant(partial) = assistant(
            vec![block_thinking("prefix thought"), block_text("prefix text")],
            7,
        ) else {
            unreachable!()
        };
        let view = Arc::new(MutableStream(std::sync::Mutex::new((*partial).clone())));
        let mut initial = snapshot(vec![Message::User(pi_core::UserMessage::text("task", 1))]);
        initial.agent.is_running = true;
        initial.agent.streaming_message = Some(pi_core::AssistantStream::new(
            pi_core::AssistantStreamId::new("stream"),
            view.clone(),
        ));
        let mut projection = MessageProjection::from_snapshot(&initial);
        let items = snapshot_items(&initial);
        assert_eq!(items[1]["id"], "reasoning-1-0");
        assert_eq!(items[1]["content"][0], "prefix thought");
        assert_eq!(items[2]["id"], "agent-1-1");
        assert_eq!(items[2]["text"], "prefix text");
        let (_, delta) = projection
            .update(
                &StreamEvent::TextDelta {
                    content_index: 1,
                    delta: " suffix".into(),
                },
                initial.agent.streaming_message.as_ref().unwrap(),
            )
            .unwrap();
        assert_eq!(delta["itemId"], items[2]["id"]);
        // A retained stream handle can already contain the finished response even
        // though this snapshot's message vector and queued events precede the end.
        let Message::Assistant(final_message) = assistant(
            vec![
                block_thinking("prefix thought"),
                block_text("prefix text suffix"),
            ],
            999,
        ) else {
            unreachable!()
        };
        *view.0.lock().unwrap() = (*final_message).clone();
        let ahead = snapshot_items(&initial);
        assert_eq!(ahead.len(), 3);
        assert_eq!(ahead[2]["text"], "prefix text suffix");
        let end_items = projection.end(&Message::Assistant(final_message.clone()));
        assert_eq!(end_items, ahead[1..]);
        let final_snapshot = snapshot(vec![
            initial.agent.messages[0].clone(),
            Message::Assistant(final_message),
        ]);
        assert_eq!(snapshot_items(&final_snapshot), ahead);
        assert_eq!(projection.next_index, final_snapshot.agent.messages.len());
    }

    #[tokio::test]
    async fn real_session_snapshot_matches_queued_live_blocks_and_persisted_entry_ids() {
        use pi_core::{ModelId, ProviderId, ResponseMetadata, StopReason, Usage};
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
        let directory = tempfile::tempdir().unwrap();
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Events(
                vec![
                    StreamEvent::Start {
                        metadata: ResponseMetadata::new(
                            ProviderId::new("scripted"),
                            ModelId::new("test"),
                            "scripted",
                            13,
                        ),
                    },
                    StreamEvent::TextStart { content_index: 0 },
                    StreamEvent::TextDelta {
                        content_index: 0,
                        delta: "before".into(),
                    },
                    StreamEvent::TextEnd {
                        content_index: 0,
                        text_signature: None,
                    },
                    StreamEvent::ThinkingStart { content_index: 1 },
                    StreamEvent::ThinkingDelta {
                        content_index: 1,
                        delta: "thought".into(),
                    },
                    StreamEvent::ThinkingEnd {
                        content_index: 1,
                        thinking_signature: None,
                    },
                    StreamEvent::TextStart { content_index: 2 },
                    StreamEvent::TextDelta {
                        content_index: 2,
                        delta: "after".into(),
                    },
                    StreamEvent::TextEnd {
                        content_index: 2,
                        text_signature: None,
                    },
                    StreamEvent::Done {
                        reason: StopReason::Stop,
                        usage: Usage::default(),
                    },
                ],
            )]))
            .build()
            .unwrap();
        let session =
            pi_session::AgentSession::create(runtime, directory.path().join("session.jsonl"))
                .await
                .unwrap();
        let mut subscription = session.subscribe();
        let mut projection = MessageProjection::from_snapshot(&subscription.snapshot);
        session.submit("task").await.unwrap();
        let mut emitted = HashMap::new();
        let mut deltas = Vec::new();
        while let Ok(event) = subscription.events.try_recv() {
            let items = match event.event {
                AgentSessionEvent::Agent(event) => match *event {
                    AgentEvent::MessageStart { message } => projection.start(&message),
                    AgentEvent::MessageUpdate { update, stream } => {
                        if let Some((_, delta)) = projection.update(&update, &stream) {
                            deltas.push(delta["itemId"].clone());
                        }
                        Vec::new()
                    }
                    AgentEvent::MessageEnd { message } => projection.end(&message),
                    _ => Vec::new(),
                },
                AgentSessionEvent::EntryAppended { entry } => {
                    let items = projection.entry(&entry);
                    // Re-subscribing between MessageEnd and its EntryAppended does
                    // not assign the persisted blocks to the next message slot.
                    if let SessionEntry::Message(message) = &entry.entry {
                        let mut at_end = snapshot(
                            session.snapshot().agent.messages[..projection.next_index].to_vec(),
                        );
                        if message.message.as_standard().is_some() {
                            at_end.revision = event.revision - 1;
                            assert_eq!(
                                MessageProjection::from_snapshot(&at_end).entry(&entry),
                                items
                            );
                        }
                    }
                    items
                }
                _ => Vec::new(),
            };
            for item in items {
                emitted.insert(item["id"].as_str().unwrap().to_string(), item);
            }
        }
        let historical = super::super::thread_from_session(&session);
        let items = historical["turns"][0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(
            deltas,
            [
                json!("agent-1-0"),
                json!("reasoning-1-1"),
                json!("agent-1-2")
            ]
        );
        assert_eq!(emitted.len(), items.len());
        for item in items {
            assert_eq!(emitted[item["id"].as_str().unwrap()], *item);
        }
        session.shutdown().await;
    }

    #[tokio::test]
    async fn actual_midstream_subscription_reads_prefix_and_replays_cumulative_blocks() {
        use pi_core::{ModelId, ProviderId, ResponseMetadata, StopReason, Usage};
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
        let directory = tempfile::tempdir().unwrap();
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::Events(vec![
                    StreamEvent::Start {
                        metadata: ResponseMetadata::new(
                            ProviderId::new("scripted"),
                            ModelId::new("test"),
                            "scripted",
                            17,
                        ),
                    },
                    StreamEvent::TextStart { content_index: 0 },
                    StreamEvent::TextDelta {
                        content_index: 0,
                        delta: "prefix".into(),
                    },
                    StreamEvent::TextDelta {
                        content_index: 0,
                        delta: " suffix".into(),
                    },
                    StreamEvent::TextEnd {
                        content_index: 0,
                        text_signature: None,
                    },
                    StreamEvent::ThinkingStart { content_index: 1 },
                    StreamEvent::ThinkingDelta {
                        content_index: 1,
                        delta: "thought".into(),
                    },
                    StreamEvent::ThinkingDelta {
                        content_index: 1,
                        delta: " continues".into(),
                    },
                    StreamEvent::ThinkingEnd {
                        content_index: 1,
                        thinking_signature: None,
                    },
                    StreamEvent::Done {
                        reason: StopReason::Stop,
                        usage: Usage::default(),
                    },
                ]),
                ScriptedTurn::Text("followup answer".into()),
            ]))
            .build()
            .unwrap();
        let session =
            pi_session::AgentSession::create(runtime, directory.path().join("session.jsonl"))
                .await
                .unwrap();
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        session.runtime().agent().subscribe(Arc::new({
            let reached = reached.clone();
            let release = release.clone();
            move |event: AgentEvent, _: pi_core::AbortSignal| {
                let reached = reached.clone();
                let release = release.clone();
                async move {
                    if matches!(event, AgentEvent::MessageUpdate { update, .. }
                        if matches!(update.as_ref(), StreamEvent::TextDelta { delta, .. } if delta == "prefix")) {
                        reached.notify_one();
                        release.notified().await;
                    }
                    Ok(())
                }
            }
        }));
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.submit("task").await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), reached.notified())
            .await
            .unwrap();
        let mut subscription = session.subscribe();
        // AgentLoop's private context has a partial assistant, but public messages
        // contains only completed messages; the live accumulator is separate.
        assert_eq!(subscription.snapshot.agent.messages.len(), 1);
        assert!(subscription.snapshot.agent.streaming_message.is_some());
        let initial_items = snapshot_items(&subscription.snapshot);
        assert_eq!(initial_items.len(), 2);
        assert_eq!(initial_items[1]["text"], "prefix");
        let mut projection = MessageProjection::from_snapshot(&subscription.snapshot);
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let final_items = snapshot_items(&session.snapshot());
        let mut by_id = initial_items
            .into_iter()
            .map(|item| (item["id"].as_str().unwrap().to_string(), item))
            .collect::<HashMap<_, _>>();
        let mut cumulative = Vec::new();
        while let Ok(event) = subscription.events.try_recv() {
            if event.revision <= subscription.snapshot.revision {
                continue;
            }
            if let AgentSessionEvent::Agent(event) = event.event {
                match *event {
                    AgentEvent::MessageUpdate { update, stream } => {
                        if let Some((_, params)) = projection.update(&update, &stream) {
                            cumulative.push(params["delta"].clone());
                            assert!(matches!(
                                params["itemId"].as_str(),
                                Some("agent-1-0" | "reasoning-1-1")
                            ));
                        }
                    }
                    AgentEvent::MessageEnd { message } => {
                        for item in projection.end(&message) {
                            by_id.insert(item["id"].as_str().unwrap().to_string(), item);
                        }
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(
            cumulative,
            [
                json!("prefix suffix"),
                json!("thought continues"),
                json!("thought continues")
            ]
        );
        assert_eq!(by_id.len(), final_items.len());
        for item in final_items {
            assert_eq!(by_id[item["id"].as_str().unwrap()], item);
        }
        session.submit("followup").await.unwrap();
        while let Ok(event) = subscription.events.try_recv() {
            if let AgentSessionEvent::Agent(event) = event.event {
                if let AgentEvent::MessageEnd { message } = *event {
                    for item in projection.end(&message) {
                        by_id.insert(item["id"].as_str().unwrap().to_string(), item);
                    }
                }
            }
        }
        let followup = snapshot_items(&session.snapshot());
        assert_eq!(followup.last().unwrap()["id"], "agent-3-0");
        assert_eq!(by_id.len(), followup.len());
        session.shutdown().await;
    }

    #[tokio::test]
    async fn compaction_replaces_old_ordinal_items_before_the_next_response() {
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
        let directory = tempfile::tempdir().unwrap();
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::Text("old response".into()),
                ScriptedTurn::Text("new response".into()),
                ScriptedTurn::Text("third response".into()),
            ]))
            .build()
            .unwrap();
        let session =
            pi_session::AgentSession::create(runtime, directory.path().join("session.jsonl"))
                .await
                .unwrap();
        session.submit("old task").await.unwrap();
        let mut subscription = session.subscribe();
        let mut items = snapshot_items(&subscription.snapshot);
        let mut projection = MessageProjection::from_snapshot(&subscription.snapshot);
        let mut refresh = ContextRefresh::default();
        session
            .append_compaction(pi_session::CompactionEntry {
                summary: "compact context".into(),
                retained_tail: Vec::new(),
                tokens_before: 100,
                details: None,
                usage: None,
            })
            .await
            .unwrap();
        while let Ok(event) = subscription.events.try_recv() {
            // CompactionEnd has not refreshed the hub's Agent context yet.
            assert!(!refresh.before_event(&event.event));
        }
        assert!(refresh.after_context_change);
        session.submit("new task").await.unwrap();
        let mut replaced = false;
        while let Ok(event) = subscription.events.try_recv() {
            if refresh.before_event(&event.event) {
                subscription = session.subscribe();
                projection = MessageProjection::from_snapshot(&subscription.snapshot);
                // Same behavior as thread/replaced: replace, never merge the old
                // ordinal namespace with a different compacted context.
                items = snapshot_items(&subscription.snapshot);
                replaced = true;
                break;
            }
        }
        assert!(replaced);
        assert!(!json!(items).to_string().contains("old response"));
        assert!(json!(items).to_string().contains("new response"));
        session.submit("third task").await.unwrap();
        while let Ok(event) = subscription.events.try_recv() {
            if let AgentSessionEvent::Agent(event) = event.event {
                if let AgentEvent::MessageEnd { message } = *event {
                    items.extend(projection.end(&message));
                }
            }
        }
        let expected = snapshot_items(&session.snapshot());
        assert_eq!(items, expected);
        let ids = items
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), items.len());
        session.shutdown().await;
    }

    #[tokio::test]
    async fn custom_and_shell_context_writes_reset_before_a_later_prompt_even_after_resubscribe() {
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
        for subscribe_after_write in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let runtime = pi_runtime::PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([
                    ScriptedTurn::Text("first answer".into()),
                    ScriptedTurn::Text("second answer".into()),
                ]))
                .build()
                .unwrap();
            let session =
                pi_session::AgentSession::create(runtime, directory.path().join("session.jsonl"))
                    .await
                    .unwrap();
            session.submit("first task").await.unwrap();
            let mut subscription = session.subscribe();
            let before_count = subscription.snapshot.agent.messages.len();
            session
                .append_custom_message(pi_core::CustomMessage {
                    custom_type: "notice".into(),
                    content: pi_core::CustomMessageContent::Text("custom context".into()),
                    display: true,
                    details: None,
                    timestamp_ms: 27,
                })
                .unwrap();
            session
                .execute_shell("printf shell-context", Default::default())
                .await
                .unwrap();
            let mut refresh = ContextRefresh::default();
            if subscribe_after_write {
                subscription = session.subscribe();
            } else {
                while let Ok(event) = subscription.events.try_recv() {
                    assert!(!refresh.before_event(&event.event));
                }
                assert!(refresh.after_context_change);
            }
            // The event hub still has the pre-write Agent vector; counting only
            // MessageEnd here would put the next user in the custom message slot.
            assert_eq!(subscription.snapshot.agent.messages.len(), before_count);
            session.submit("second task").await.unwrap();
            let event = subscription.events.recv().await.unwrap();
            assert!(refresh.before_event(&event.event));
            subscription = session.subscribe();
            let mut projection = MessageProjection::from_snapshot(&subscription.snapshot);
            let items = snapshot_items(&subscription.snapshot);
            assert!(json!(items).to_string().contains("custom context"));
            assert!(json!(items).to_string().contains("second answer"));
            let ids = items
                .iter()
                .map(|item| item["id"].as_str().unwrap())
                .collect::<HashSet<_>>();
            assert_eq!(ids.len(), items.len());
            assert!(projection.next_index > before_count + 2);
            assert_eq!(items, snapshot_items(&session.snapshot()));
            // A later response cannot reuse any prior message identity.
            let later = projection.end(&assistant(vec![block_text("later")], 27));
            assert!(!ids.contains(later[0]["id"].as_str().unwrap()));
            session.shutdown().await;
        }
    }

    #[test]
    fn refreshed_tool_snapshot_preserves_plugin_details_without_decoding_business_fields() {
        let details =
            json!({ "session": { "opaque": "control", "saved": null }, "pluginStatus": "queued" });
        let result = Message::tool_result(pi_core::ToolResultMessage {
            tool_call_id: pi_core::ToolCallId::new("plugin-call"),
            tool_name: "custom_task".into(),
            content: vec![block_text("started")],
            details: Some(details.clone()),
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 1,
        });
        let thread = super::super::thread_from_snapshot(
            &snapshot(vec![result]),
            "parent",
            std::path::Path::new("/project"),
            Default::default(),
            json!("pi-rs"),
            None,
            Default::default(),
        );
        let item = &thread["turns"][0]["items"][0];
        assert_eq!(item["type"], "mcpToolCall");
        assert_eq!(item["toolName"], "custom_task");
        assert_eq!(item["details"], details);
        assert_eq!(item["result"], "started");
        assert!(item.get("newThreadId").is_none());
    }

    #[tokio::test]
    async fn snapshot_rebase_keeps_turn_notifications_and_fast_provider_errors() {
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
        let directory = tempfile::tempdir().unwrap();
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Error(
                "provider unavailable".into(),
            )]))
            .build()
            .unwrap();
        let session =
            pi_session::AgentSession::create(runtime, directory.path().join("session.jsonl"))
                .await
                .unwrap();
        let mut subscription = session.subscribe();
        let initial_revision = subscription.snapshot.revision;
        session.submit("fail quickly").await.unwrap();
        // Refresh the data boundary without dropping queued semantic events.
        subscription.snapshot = session.subscribe().snapshot;
        let mut notifications = Vec::new();
        let mut errors = Vec::new();
        while let Ok(event) = subscription.events.try_recv() {
            if event.revision <= initial_revision
                || (event.revision <= subscription.snapshot.revision
                    && snapshot_covers_event(&event.event))
            {
                continue;
            }
            match event.event {
                AgentSessionEvent::Agent(event)
                    if matches!(event.as_ref(), AgentEvent::AgentStart) =>
                {
                    notifications.push(turn_notification("child", true))
                }
                AgentSessionEvent::AgentSettled => {
                    notifications.push(turn_notification("child", false))
                }
                AgentSessionEvent::AgentEnd { messages, .. } => {
                    for message in messages {
                        if let Message::Assistant(message) = message {
                            if let Some(error) = &message.error_message {
                                errors.push(error.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        assert_eq!(
            notifications
                .iter()
                .map(|(method, _)| *method)
                .collect::<Vec<_>>(),
            ["turn/started", "turn/completed"]
        );
        assert_eq!(
            notifications[0].1["turn"]["id"],
            notifications[1].1["turn"]["id"]
        );
        let mut active = subscription.snapshot.clone();
        active.agent.is_running = true;
        let active_thread = super::super::thread_from_snapshot(
            &active,
            "child",
            std::path::Path::new("/project"),
            Default::default(),
            json!("pi-rs"),
            None,
            Default::default(),
        );
        assert_eq!(
            active_thread["activeTurnId"],
            notifications[0].1["turn"]["id"]
        );
        assert!(errors
            .iter()
            .any(|error| error.contains("provider unavailable")));
        let items = snapshot_items(&subscription.snapshot);
        let notices = items
            .iter()
            .filter(|item| item["type"] == "notice")
            .collect::<Vec<_>>();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0]["id"], "error-1-0");
        assert_eq!(notices[0]["status"], "failed");
        assert!(notices[0]["detail"]
            .as_str()
            .unwrap()
            .contains("provider unavailable"));
        session.shutdown().await;
    }

    #[test]
    fn snapshot_rehydrates_arguments_for_tool_starts_covered_by_refresh() {
        let call: pi_core::ToolCall = serde_json::from_value(json!({
            "id": "pending", "name": "bash", "arguments": { "command": "printf retained" }
        }))
        .unwrap();
        let completed: pi_core::ToolCall = serde_json::from_value(json!({
            "id": "finished", "name": "read", "arguments": { "path": "file" }
        }))
        .unwrap();
        let result = Message::tool_result(pi_core::ToolResultMessage {
            tool_call_id: completed.id.clone(),
            tool_name: completed.name.clone(),
            content: Vec::new(),
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 1,
        });
        let snapshot = snapshot(vec![
            assistant(
                vec![
                    ContentBlock::ToolCall(completed),
                    ContentBlock::ToolCall(call.clone()),
                ],
                1,
            ),
            result,
        ]);
        let mut state = ProjectionState::from_snapshot(&snapshot, "child");
        assert_eq!(state.tool_names.len(), 1);
        assert_eq!(state.tool_names["pending"], "bash");
        assert!(!state.tool_args.contains_key("finished"));
        let args = state.tool_args.remove("pending").unwrap();
        assert_eq!(args, call.arguments);
        let projected = tool_item(
            "pending",
            "bash",
            args,
            "completed",
            Some("retained".into()),
            "/project",
        );
        assert_eq!(projected["command"], "printf retained");
    }

    #[test]
    fn plugin_notice_projection_preserves_severity_and_text_without_a_fake_turn() {
        let params = notice_params(
            "thread",
            "Native command output",
            pi_core::NoticeLevel::Warning,
        );
        assert_eq!(params["threadId"], "thread");
        assert_eq!(params["level"], "warning");
        assert_eq!(params["message"], "Native command output");
        assert!(params.get("turnId").is_none());
    }

    #[test]
    fn persisted_user_item_prefers_display_text_and_keeps_images() {
        let content = vec![
            ContentBlock::Text(pi_core::TextContent::new("expanded skill body")),
            ContentBlock::Image(pi_core::ImageContent {
                data: "abc".to_string(),
                mime_type: "image/png".to_string(),
            }),
        ];

        let projected = super::super::user_content_with_display(&content, Some("/skill:review"));

        assert_eq!(projected[0]["text"], "/skill:review");
        assert_eq!(projected[1]["type"], "image");
        assert!(!json!(projected).to_string().contains("expanded skill body"));
    }

    #[test]
    fn partial_and_final_results_preserve_plugin_data_including_errors() {
        for name in ["custom_task", "bash"] {
            for status in ["inProgress", "completed", "failed"] {
                let mut result = ToolResult::text("latest cumulative output");
                result.details = Some(json!({ "custom": [1, "state"], "reference": null }));
                let item = tool_result_item(
                    "call",
                    name,
                    json!({ "command": "pwd" }),
                    status,
                    &result,
                    "/project",
                );
                assert_eq!(item["details"], result.details.unwrap());
                assert_eq!(item["toolName"], name);
                assert_eq!(item["arguments"], json!({ "command": "pwd" }));
                assert_eq!(item["status"], status);
                assert_eq!(
                    item[if name == "bash" {
                        "aggregatedOutput"
                    } else {
                        "result"
                    }],
                    "latest cumulative output"
                );
            }
        }
    }

    #[test]
    fn all_tool_presentations_preserve_canonical_identity_and_arguments() {
        for name in ["bash", "custom_task", "spawn_agent"] {
            let arguments = json!({ "command": "pwd", "data": { "value": 2 } });
            let item = tool_item(
                "call-1",
                name,
                arguments.clone(),
                "inProgress",
                None,
                "/project",
            );
            assert_eq!(item["toolName"], name);
            assert_eq!(item["arguments"], arguments);
            assert_eq!(item["status"], "inProgress");
            assert!(item.get("newThreadId").is_none());
        }
    }
}
