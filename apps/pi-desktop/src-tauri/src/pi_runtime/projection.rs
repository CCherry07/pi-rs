use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use pi_core::{AgentEvent, ContentBlock, Message, StreamEvent, ToolResult};
use pi_session::{AgentSessionEvent, RevisionedAgentSessionEvent, SessionEntry};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, Mutex};

use crate::backend::events::PiEvent;

use super::session_store::{content_text, LiveSession, SessionStore};

pub(crate) type ForwarderRegistry = Arc<Mutex<HashSet<String>>>;

#[derive(Clone)]
struct SubagentProjection {
    child_thread_id: String,
    agent: String,
    total_tokens: Option<u64>,
}

#[derive(Clone)]
struct WorkflowNodeProjection {
    run_id: String,
    key: String,
    state: String,
    child: SubagentProjection,
}

#[derive(Clone, Default)]
struct WorkflowProjection {
    nodes: Vec<WorkflowNodeProjection>,
}

impl WorkflowProjection {
    fn display_nodes(&self) -> Vec<super::WorkflowDisplayNode> {
        self.nodes
            .iter()
            .map(|node| super::WorkflowDisplayNode {
                key: node.key.clone(),
                agent: node.child.agent.clone(),
                thread_id: node.child.child_thread_id.clone(),
                state: node.state.clone(),
                total_tokens: node.child.total_tokens,
            })
            .collect()
    }
}

struct EventProjectionContext<'a> {
    app: &'a AppHandle,
    workspace_id: &'a str,
    thread_id: &'a str,
    live: &'a LiveSession,
    store: &'a SessionStore,
    forwarders: &'a ForwarderRegistry,
}

#[derive(Default)]
struct ProjectionState {
    turn_id: Option<String>,
    message_id: Option<String>,
    pending_message_items: HashMap<bool, VecDeque<String>>,
    reasoning_ids: HashMap<usize, String>,
    reasoning_text: HashMap<usize, String>,
    tool_names: HashMap<String, String>,
    tool_args: HashMap<String, Value>,
    subagents: HashMap<String, SubagentProjection>,
    workflows: HashMap<String, WorkflowProjection>,
    compaction_id: Option<String>,
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
    let mut state = ProjectionState::default();
    let mut thread_id = thread_id;
    let mut changes = live.changes.take();
    let mut refresh_after_run = live.subscription.snapshot.agent.is_running;
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
                refresh_after_run = live.subscription.snapshot.agent.is_running;
                state = ProjectionState::default();
                emit(&app, &workspace_id, "thread/replaced", json!({
                    "previousThreadId": previous_id,
                    "thread": super::thread_from_subscription(&live),
                }));
                continue;
            }
            event = live.subscription.events.recv() => event,
        };
        match event {
            Ok(event) if event.revision <= live.subscription.snapshot.revision => continue,
            Ok(event) => {
                let settled = matches!(event.event, AgentSessionEvent::AgentSettled);
                project_event(
                    EventProjectionContext {
                        app: &app,
                        workspace_id: &workspace_id,
                        thread_id: &thread_id,
                        live: &live,
                        store: &store,
                        forwarders: &forwarders,
                    },
                    &mut state,
                    event,
                )
                .await;
                // A subscription installed mid-response can miss MessageStart.
                // Once that run settles, recover its complete history from snapshot.
                if settled && refresh_after_run {
                    refresh_after_run = false;
                    if let Some(session) = live.primary().cloned() {
                        live.replace(session);
                        refresh_after_run = live.subscription.snapshot.agent.is_running;
                        state = ProjectionState::default();
                        emit(
                            &app,
                            &workspace_id,
                            "thread/replaced",
                            json!({
                                "previousThreadId": thread_id,
                                "thread": super::thread_from_subscription(&live),
                            }),
                        );
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                emit_snapshot_status(&app, &workspace_id, &thread_id, &live);
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
        store,
        forwarders,
    } = context;
    match event.event {
        AgentSessionEvent::Agent(agent_event) => match *agent_event {
            AgentEvent::AgentStart => {
                let turn_id = format!("pi-turn-{thread_id}");
                state.turn_id = Some(turn_id.clone());
                emit(
                    app,
                    workspace_id,
                    "turn/started",
                    json!({
                        "threadId": thread_id,
                        "turn": { "id": turn_id, "status": "inProgress" }
                    }),
                );
                emit_status(app, workspace_id, thread_id, "active");
            }
            AgentEvent::MessageStart { message } => match message {
                Message::User(message) => {
                    let item_id = format!("user-{}", message.timestamp_ms);
                    state
                        .pending_message_items
                        .entry(true)
                        .or_default()
                        .push_back(item_id.clone());
                    emit(
                        app,
                        workspace_id,
                        "item/completed",
                        json!({
                            "threadId": thread_id,
                            "turnId": state.turn_id,
                            "item": {
                                "id": item_id,
                                "type": "userMessage",
                                "content": user_item_content(&message.content)
                            }
                        }),
                    );
                }
                Message::Assistant(_) => {
                    let id = format!("message-{}", uuid::Uuid::new_v4());
                    state.message_id = Some(id);
                    state.reasoning_ids.clear();
                    state.reasoning_text.clear();
                }
                _ => {}
            },
            AgentEvent::MessageUpdate { update, .. } => match update.as_ref() {
                StreamEvent::TextDelta { delta, .. } => {
                    if let Some(item_id) = &state.message_id {
                        emit(
                            app,
                            workspace_id,
                            "item/agentMessage/delta",
                            json!({
                                "threadId": thread_id,
                                "turnId": state.turn_id,
                                "itemId": item_id,
                                "delta": delta
                            }),
                        );
                    }
                }
                StreamEvent::ThinkingStart { content_index } => {
                    let id = format!("reasoning-{}", uuid::Uuid::new_v4());
                    state.reasoning_ids.insert(*content_index, id.clone());
                    state.reasoning_text.insert(*content_index, String::new());
                    emit(
                        app,
                        workspace_id,
                        "item/started",
                        json!({
                            "threadId": thread_id,
                            "turnId": state.turn_id,
                            "item": {
                                "id": id,
                                "type": "reasoning",
                                "summary": [],
                                "content": []
                            }
                        }),
                    );
                }
                StreamEvent::ThinkingDelta {
                    content_index,
                    delta,
                } => {
                    state
                        .reasoning_text
                        .entry(*content_index)
                        .or_default()
                        .push_str(delta);
                    if let Some(item_id) = state.reasoning_ids.get(content_index) {
                        emit(
                            app,
                            workspace_id,
                            "item/reasoning/textDelta",
                            json!({
                                "threadId": thread_id,
                                "turnId": state.turn_id,
                                "itemId": item_id,
                                "delta": delta
                            }),
                        );
                    }
                }
                _ => {}
            },
            AgentEvent::MessageEnd { message } => {
                if let Message::Assistant(message) = message {
                    for (index, item_id) in state.reasoning_ids.drain() {
                        let content = state.reasoning_text.remove(&index).unwrap_or_default();
                        emit(
                            app,
                            workspace_id,
                            "item/completed",
                            json!({
                                "threadId": thread_id,
                                "turnId": state.turn_id,
                                "item": {
                                    "id": item_id,
                                    "type": "reasoning",
                                    "summary": [],
                                    "content": [content]
                                }
                            }),
                        );
                    }
                    let text = message
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if let Some(item_id) =
                        state.message_id.take().filter(|_| !text.trim().is_empty())
                    {
                        state
                            .pending_message_items
                            .entry(false)
                            .or_default()
                            .push_back(item_id.clone());
                        emit(
                            app,
                            workspace_id,
                            "item/completed",
                            json!({
                                "threadId": thread_id,
                                "turnId": state.turn_id,
                                "item": { "id": item_id, "type": "agentMessage", "text": text }
                            }),
                        );
                    }
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
                let item = if tool_name == "subagent" {
                    subagent_tool_item(&item_id, thread_id, &args, "inProgress", None, None)
                } else if tool_name == "subagent_workflow" {
                    super::workflow_tool_item(&item_id, thread_id, "inProgress", &[], None)
                } else {
                    tool_item(
                        &item_id,
                        &tool_name,
                        args,
                        "inProgress",
                        None,
                        live.cwd().to_string_lossy().as_ref(),
                    )
                };
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
                if tool_name == "subagent" {
                    if let Some(projected) = project_subagent(
                        app,
                        workspace_id,
                        thread_id,
                        store,
                        forwarders,
                        &partial_result,
                    )
                    .await
                    {
                        let item_id = tool_call_id.to_string();
                        state.subagents.insert(item_id.clone(), projected.clone());
                        let args = state.tool_args.get(&item_id).cloned().unwrap_or_default();
                        emit(
                            app,
                            workspace_id,
                            "item/started",
                            json!({
                                "threadId": thread_id,
                                "turnId": state.turn_id,
                                "item": subagent_tool_item(
                                    &item_id,
                                    thread_id,
                                    &args,
                                    "inProgress",
                                    Some(&projected),
                                    Some(tool_result_text(&partial_result)),
                                )
                            }),
                        );
                    }
                } else if tool_name == "subagent_workflow" {
                    let item_id = tool_call_id.to_string();
                    let previous = state.workflows.remove(&item_id).unwrap_or_default();
                    let projected = project_workflow(
                        app,
                        workspace_id,
                        thread_id,
                        store,
                        forwarders,
                        &partial_result,
                        previous,
                    )
                    .await;
                    let nodes = projected.display_nodes();
                    state.workflows.insert(item_id.clone(), projected);
                    emit(
                        app,
                        workspace_id,
                        "item/started",
                        json!({
                            "threadId": thread_id,
                            "turnId": state.turn_id,
                            "item": super::workflow_tool_item(
                                &item_id,
                                thread_id,
                                "inProgress",
                                &nodes,
                                Some(tool_result_text(&partial_result)),
                            )
                        }),
                    );
                } else if tool_name == "bash" {
                    let delta = tool_result_text(&partial_result);
                    if !delta.is_empty() {
                        emit(
                            app,
                            workspace_id,
                            "item/commandExecution/outputDelta",
                            json!({
                                "threadId": thread_id,
                                "turnId": state.turn_id,
                                "itemId": tool_call_id.to_string(),
                                "delta": delta
                            }),
                        );
                    }
                }
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let item_id = tool_call_id.to_string();
                let output = tool_result_text(&result);
                let args = state.tool_args.remove(&item_id).unwrap_or_default();
                if tool_name == "subagent" {
                    let projected =
                        project_subagent(app, workspace_id, thread_id, store, forwarders, &result)
                            .await
                            .or_else(|| state.subagents.get(&item_id).cloned());
                    if let Some(projected) = projected {
                        state.subagents.insert(item_id.clone(), projected);
                    }
                } else if tool_name == "subagent_workflow" {
                    let previous = state.workflows.remove(&item_id).unwrap_or_default();
                    let projected = project_workflow(
                        app,
                        workspace_id,
                        thread_id,
                        store,
                        forwarders,
                        &result,
                        previous,
                    )
                    .await;
                    state.workflows.insert(item_id.clone(), projected);
                }
                let item = if tool_name == "subagent" {
                    subagent_tool_item(
                        &item_id,
                        thread_id,
                        &args,
                        if is_error { "failed" } else { "completed" },
                        state.subagents.get(&item_id),
                        Some(output),
                    )
                } else if tool_name == "subagent_workflow" {
                    let nodes = state
                        .workflows
                        .get(&item_id)
                        .map(WorkflowProjection::display_nodes)
                        .unwrap_or_default();
                    super::workflow_tool_item(
                        &item_id,
                        thread_id,
                        if is_error { "failed" } else { "completed" },
                        &nodes,
                        Some(output),
                    )
                } else {
                    tool_item(
                        &item_id,
                        &tool_name,
                        args,
                        if is_error { "failed" } else { "completed" },
                        Some(output),
                        live.cwd().to_string_lossy().as_ref(),
                    )
                };
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
                state.subagents.remove(&item_id);
                state.workflows.remove(&item_id);
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
            if let Some(turn_id) = state.turn_id.take() {
                emit(
                    app,
                    workspace_id,
                    "turn/completed",
                    json!({
                        "threadId": thread_id,
                        "turn": { "id": turn_id, "status": "completed" }
                    }),
                );
            }
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
            let SessionEntry::Message(message_entry) = &entry.entry else {
                return;
            };
            let Some(message) = message_entry.message.as_standard() else {
                return;
            };
            let Some((is_user, _)) = super::message_key(message) else {
                return;
            };
            let Some(item_id) = state
                .pending_message_items
                .get_mut(&is_user)
                .and_then(VecDeque::pop_front)
            else {
                return;
            };
            let item = match message {
                Message::User(message) => json!({
                    "id": item_id,
                    "type": "userMessage",
                    "entryId": entry.id,
                    "content": user_item_content_with_display(
                        &message.content,
                        message_entry.message.display_text(),
                    )
                }),
                Message::Assistant(message) => json!({
                    "id": item_id,
                    "type": "agentMessage",
                    "entryId": entry.id,
                    "text": message.content.iter().filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("")
                }),
                _ => return,
            };
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

async fn project_subagent(
    app: &AppHandle,
    workspace_id: &str,
    parent_thread_id: &str,
    store: &SessionStore,
    forwarders: &ForwarderRegistry,
    result: &ToolResult,
) -> Option<SubagentProjection> {
    let details = result.details.as_ref()?.as_object()?;
    let isolated_session_id = details.get("isolatedSessionId")?.as_str()?;
    let agent = details
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or("subagent")
        .to_string();
    let total_tokens = details
        .get("usage")
        .and_then(|usage| usage.get("totalTokens"))
        .and_then(Value::as_u64);
    project_isolated_subagent(
        app,
        workspace_id,
        parent_thread_id,
        store,
        forwarders,
        isolated_session_id,
        &agent,
        None,
        total_tokens,
    )
    .await
}

async fn project_workflow(
    app: &AppHandle,
    workspace_id: &str,
    parent_thread_id: &str,
    store: &SessionStore,
    forwarders: &ForwarderRegistry,
    result: &ToolResult,
    previous: WorkflowProjection,
) -> WorkflowProjection {
    let mut previous_by_run = previous
        .nodes
        .into_iter()
        .map(|node| (node.run_id.clone(), node))
        .collect::<HashMap<_, _>>();
    let mut nodes = Vec::new();
    for summary in super::workflow_node_summaries(result.details.as_ref()) {
        let Some(run_id) = summary.run_id.clone() else {
            continue;
        };
        if let Some(mut node) = previous_by_run.remove(&run_id) {
            node.key = summary.key;
            node.state = summary.state;
            node.child.agent = summary.agent;
            if summary.total_tokens.is_some() {
                node.child.total_tokens = summary.total_tokens;
            }
            nodes.push(node);
            continue;
        }
        let Some(isolated_session_id) = summary.isolated_session_id.as_deref() else {
            continue;
        };
        let Some(child) = project_isolated_subagent(
            app,
            workspace_id,
            parent_thread_id,
            store,
            forwarders,
            isolated_session_id,
            &summary.agent,
            Some(&summary.key),
            summary.total_tokens,
        )
        .await
        else {
            continue;
        };
        nodes.push(WorkflowNodeProjection {
            run_id,
            key: summary.key,
            state: summary.state,
            child,
        });
    }
    WorkflowProjection { nodes }
}

#[allow(clippy::too_many_arguments)]
async fn project_isolated_subagent(
    app: &AppHandle,
    workspace_id: &str,
    parent_thread_id: &str,
    store: &SessionStore,
    forwarders: &ForwarderRegistry,
    isolated_session_id: &str,
    agent: &str,
    nickname: Option<&str>,
    total_tokens: Option<u64>,
) -> Option<SubagentProjection> {
    let (observation, live) = store
        .subscribe_isolated(parent_thread_id, isolated_session_id, agent, nickname)
        .ok()?;
    let child_thread_id = observation.session_id();
    let projected = SubagentProjection {
        child_thread_id: child_thread_id.clone(),
        agent: agent.to_string(),
        total_tokens,
    };
    let should_start = forwarders.lock().await.insert(child_thread_id.clone());
    if should_start {
        let is_running = observation.snapshot().agent.is_running;
        let thread =
            super::thread_from_observation(&observation, parent_thread_id, agent, nickname);
        emit(
            app,
            workspace_id,
            "thread/started",
            json!({ "thread": thread }),
        );
        emit_status(
            app,
            workspace_id,
            &child_thread_id,
            if is_running { "active" } else { "idle" },
        );
        spawn_session_forwarder(
            app.clone(),
            workspace_id.to_string(),
            child_thread_id.clone(),
            child_thread_id,
            live,
            store.clone(),
            Arc::clone(forwarders),
        );
    }
    Some(projected)
}

fn subagent_tool_item(
    id: &str,
    parent_thread_id: &str,
    arguments: &Value,
    status: &str,
    projection: Option<&SubagentProjection>,
    output: Option<String>,
) -> Value {
    let agent = projection
        .map(|projection| projection.agent.as_str())
        .or_else(|| arguments.get("agent").and_then(Value::as_str))
        .unwrap_or("subagent");
    let prompt = arguments
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let child_thread_id = projection.map(|projection| projection.child_thread_id.as_str());
    json!({
        "id": id,
        "type": "collabToolCall",
        "tool": "spawn",
        "senderThreadId": parent_thread_id,
        "newThreadId": child_thread_id,
        "newAgentRole": agent,
        "prompt": prompt,
        "status": status,
        "result": output.unwrap_or_default(),
        "agentStatuses": child_thread_id.map(|thread_id| vec![json!({
            "threadId": thread_id,
            "agentRole": agent,
            "status": status,
            "totalTokens": projection.and_then(|projection| projection.total_tokens),
        })]).unwrap_or_default(),
    })
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
            "command": command,
            "cwd": cwd,
            "status": status,
            "aggregatedOutput": output.unwrap_or_default()
        })
    } else {
        json!({
            "id": id,
            "type": "mcpToolCall",
            "server": "pi",
            "tool": name,
            "arguments": arguments,
            "status": status,
            "result": output.unwrap_or_default()
        })
    }
}

fn tool_result_text(result: &ToolResult) -> String {
    content_text(&result.content)
}

fn user_item_content_with_display(
    content: &[ContentBlock],
    display_text: Option<&str>,
) -> Vec<Value> {
    if let Some(display_text) = display_text {
        let mut projected = vec![json!({ "type": "text", "text": display_text })];
        projected.extend(content.iter().filter_map(|block| match block {
            ContentBlock::Image(image) => Some(json!({
                "type": "image",
                "url": format!("data:{};base64,{}", image.mime_type, image.data)
            })),
            _ => None,
        }));
        return projected;
    }
    user_item_content(content)
}

fn user_item_content(content: &[ContentBlock]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(json!({ "type": "text", "text": text.text })),
            ContentBlock::Image(image) => Some(json!({
                "type": "image",
                "url": format!("data:{};base64,{}", image.mime_type, image.data)
            })),
            _ => None,
        })
        .collect()
}

fn notice_params(thread_id: &str, message: &str, level: pi_core::NoticeLevel) -> Value {
    json!({ "threadId": thread_id, "message": message, "level": level })
}

fn emit_snapshot_status(app: &AppHandle, workspace_id: &str, thread_id: &str, live: &LiveSession) {
    emit_status(
        app,
        workspace_id,
        thread_id,
        if live.snapshot().agent.is_running {
            "active"
        } else {
            "idle"
        },
    );
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

        let projected = user_item_content_with_display(&content, Some("/skill:review"));

        assert_eq!(projected[0]["text"], "/skill:review");
        assert_eq!(projected[1]["type"], "image");
        assert!(!json!(projected).to_string().contains("expanded skill body"));
    }

    #[test]
    fn subagent_tool_items_expose_child_identity_and_live_status() {
        let projection = SubagentProjection {
            child_thread_id: "child-session".to_string(),
            agent: "reviewer".to_string(),
            total_tokens: Some(12_400),
        };
        let item = subagent_tool_item(
            "call-1",
            "parent-session",
            &json!({"agent": "reviewer", "task": "Review the parser"}),
            "inProgress",
            Some(&projection),
            None,
        );

        assert_eq!(item["type"], "collabToolCall");
        assert_eq!(item["senderThreadId"], "parent-session");
        assert_eq!(item["newThreadId"], "child-session");
        assert_eq!(item["prompt"], "Review the parser");
        assert_eq!(item["agentStatuses"][0]["status"], "inProgress");
        assert_eq!(item["agentStatuses"][0]["totalTokens"], 12_400);
    }

    #[test]
    fn workflow_tool_items_expose_each_started_child_identity_and_state() {
        let projection = WorkflowProjection {
            nodes: vec![
                WorkflowNodeProjection {
                    run_id: "run-a".to_string(),
                    key: "first".to_string(),
                    state: "completed".to_string(),
                    child: SubagentProjection {
                        child_thread_id: "child-a".to_string(),
                        agent: "researcher".to_string(),
                        total_tokens: Some(321),
                    },
                },
                WorkflowNodeProjection {
                    run_id: "run-b".to_string(),
                    key: "second".to_string(),
                    state: "running".to_string(),
                    child: SubagentProjection {
                        child_thread_id: "child-b".to_string(),
                        agent: "reviewer".to_string(),
                        total_tokens: Some(123),
                    },
                },
            ],
        };
        let nodes = projection.display_nodes();
        let item = super::super::workflow_tool_item(
            "call-1",
            "parent-session",
            "inProgress",
            &nodes,
            None,
        );

        assert_eq!(item["type"], "collabToolCall");
        assert_eq!(item["receiverThreadIds"], json!(["child-a", "child-b"]));
        assert_eq!(item["receiverAgents"][0]["agentNickname"], "first");
        assert_eq!(item["receiverAgents"][1]["agentRole"], "reviewer");
        assert_eq!(item["agentStatuses"][0]["status"], "completed");
        assert_eq!(item["agentStatuses"][1]["status"], "inProgress");
        assert_eq!(item["agentStatuses"][1]["totalTokens"], 123);
    }
}
