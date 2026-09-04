//! Pi stdin/stdout RPC Adapter.
//!
//! Commands and responses use strict LF-delimited JSON. Session events are
//! projected by `pi_json_wire`, so JSON and RPC cannot drift independently.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use pi_agent::QueueMode;
use pi_core::{
    ContentBlock, ImageContent, Message, ModelSpec, StopReason, ThinkingLevel, UserMessage,
};
use pi_session::{
    AgentMessage, AgentSession, AgentSessionReplacement, ForkPosition, PiSession, QueueKind,
    SessionDocument, SessionEntry, SessionRecord, ShellExecutionOptions, aggregate_document_usage,
    current_session_context_tokens,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::json_wire::{compaction_result_json, session_entry_json, session_event_json};

const MAX_RPC_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct RpcOutput {
    stdout: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
}

impl RpcOutput {
    fn new() -> Self {
        Self {
            stdout: Arc::new(tokio::sync::Mutex::new(tokio::io::stdout())),
        }
    }

    async fn emit(&self, value: Value) -> Result<(), String> {
        let mut encoded = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        let mut stdout = self.stdout.lock().await;
        stdout
            .write_all(&encoded)
            .await
            .map_err(|error| error.to_string())?;
        stdout.flush().await.map_err(|error| error.to_string())
    }
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum RpcCommand {
    Prompt {
        #[serde(default)]
        id: Option<String>,
        message: String,
        #[serde(default)]
        images: Vec<ImageContent>,
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer {
        #[serde(default)]
        id: Option<String>,
        message: String,
        #[serde(default)]
        images: Vec<ImageContent>,
    },
    FollowUp {
        #[serde(default)]
        id: Option<String>,
        message: String,
        #[serde(default)]
        images: Vec<ImageContent>,
    },
    Abort {
        id: Option<String>,
    },
    ClearQueue {
        id: Option<String>,
    },
    NewSession {
        id: Option<String>,
        parent_session: Option<String>,
    },
    GetState {
        id: Option<String>,
    },
    SetModel {
        id: Option<String>,
        provider: String,
        model_id: String,
    },
    CycleModel {
        id: Option<String>,
    },
    GetAvailableModels {
        id: Option<String>,
    },
    SetThinkingLevel {
        id: Option<String>,
        level: ThinkingLevel,
    },
    CycleThinkingLevel {
        id: Option<String>,
    },
    GetAvailableThinkingLevels {
        id: Option<String>,
    },
    SetSteeringMode {
        id: Option<String>,
        mode: RpcQueueMode,
    },
    SetFollowUpMode {
        id: Option<String>,
        mode: RpcQueueMode,
    },
    Compact {
        id: Option<String>,
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        id: Option<String>,
        enabled: bool,
    },
    SetAutoRetry {
        id: Option<String>,
        enabled: bool,
    },
    AbortRetry {
        id: Option<String>,
    },
    Bash {
        id: Option<String>,
        command: String,
        #[serde(default)]
        exclude_from_context: bool,
    },
    AbortBash {
        id: Option<String>,
    },
    GetSessionStats {
        id: Option<String>,
    },
    ExportHtml {
        id: Option<String>,
        output_path: Option<String>,
    },
    SwitchSession {
        id: Option<String>,
        session_path: String,
    },
    Fork {
        id: Option<String>,
        entry_id: String,
    },
    #[serde(rename = "clone")]
    CloneSession {
        id: Option<String>,
    },
    GetForkMessages {
        id: Option<String>,
    },
    GetEntries {
        id: Option<String>,
        since: Option<String>,
    },
    GetTree {
        id: Option<String>,
    },
    GetLastAssistantText {
        id: Option<String>,
    },
    SetSessionName {
        id: Option<String>,
        name: String,
    },
    GetMessages {
        id: Option<String>,
    },
    GetCommands {
        id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StreamingBehavior {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum RpcQueueMode {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

impl From<RpcQueueMode> for QueueMode {
    fn from(value: RpcQueueMode) -> Self {
        match value {
            RpcQueueMode::All => Self::All,
            RpcQueueMode::OneAtATime => Self::OneAtATime,
        }
    }
}

impl RpcCommand {
    fn id(&self) -> Option<&str> {
        match self {
            Self::Prompt { id, .. }
            | Self::Steer { id, .. }
            | Self::FollowUp { id, .. }
            | Self::Abort { id }
            | Self::ClearQueue { id }
            | Self::NewSession { id, .. }
            | Self::GetState { id }
            | Self::SetModel { id, .. }
            | Self::CycleModel { id }
            | Self::GetAvailableModels { id }
            | Self::SetThinkingLevel { id, .. }
            | Self::CycleThinkingLevel { id }
            | Self::GetAvailableThinkingLevels { id }
            | Self::SetSteeringMode { id, .. }
            | Self::SetFollowUpMode { id, .. }
            | Self::Compact { id, .. }
            | Self::SetAutoCompaction { id, .. }
            | Self::SetAutoRetry { id, .. }
            | Self::AbortRetry { id }
            | Self::Bash { id, .. }
            | Self::AbortBash { id }
            | Self::GetSessionStats { id }
            | Self::ExportHtml { id, .. }
            | Self::SwitchSession { id, .. }
            | Self::Fork { id, .. }
            | Self::CloneSession { id }
            | Self::GetForkMessages { id }
            | Self::GetEntries { id, .. }
            | Self::GetTree { id }
            | Self::GetLastAssistantText { id }
            | Self::SetSessionName { id, .. }
            | Self::GetMessages { id }
            | Self::GetCommands { id } => id.as_deref(),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Prompt { .. } => "prompt",
            Self::Steer { .. } => "steer",
            Self::FollowUp { .. } => "follow_up",
            Self::Abort { .. } => "abort",
            Self::ClearQueue { .. } => "clear_queue",
            Self::NewSession { .. } => "new_session",
            Self::GetState { .. } => "get_state",
            Self::SetModel { .. } => "set_model",
            Self::CycleModel { .. } => "cycle_model",
            Self::GetAvailableModels { .. } => "get_available_models",
            Self::SetThinkingLevel { .. } => "set_thinking_level",
            Self::CycleThinkingLevel { .. } => "cycle_thinking_level",
            Self::GetAvailableThinkingLevels { .. } => "get_available_thinking_levels",
            Self::SetSteeringMode { .. } => "set_steering_mode",
            Self::SetFollowUpMode { .. } => "set_follow_up_mode",
            Self::Compact { .. } => "compact",
            Self::SetAutoCompaction { .. } => "set_auto_compaction",
            Self::SetAutoRetry { .. } => "set_auto_retry",
            Self::AbortRetry { .. } => "abort_retry",
            Self::Bash { .. } => "bash",
            Self::AbortBash { .. } => "abort_bash",
            Self::GetSessionStats { .. } => "get_session_stats",
            Self::ExportHtml { .. } => "export_html",
            Self::SwitchSession { .. } => "switch_session",
            Self::Fork { .. } => "fork",
            Self::CloneSession { .. } => "clone",
            Self::GetForkMessages { .. } => "get_fork_messages",
            Self::GetEntries { .. } => "get_entries",
            Self::GetTree { .. } => "get_tree",
            Self::GetLastAssistantText { .. } => "get_last_assistant_text",
            Self::SetSessionName { .. } => "set_session_name",
            Self::GetMessages { .. } => "get_messages",
            Self::GetCommands { .. } => "get_commands",
        }
    }
}

pub type HtmlExporter = fn(&AgentSession, Option<&str>) -> Result<PathBuf, String>;

pub async fn run(session: PiSession, export_html: HtmlExporter) -> Result<(), String> {
    let output = RpcOutput::new();
    let events = tokio::spawn(forward_events(session.clone(), output.clone()));
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = Vec::new();
    let mut commands = tokio::task::JoinSet::new();

    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_RPC_LINE_BYTES {
            output
                .emit(error_response(None, "parse", "RPC command exceeds 16 MiB"))
                .await?;
            continue;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let bytes = line.clone();
        let command_session = session.clone();
        let command_output = output.clone();
        commands.spawn(async move {
            handle_line(command_session, command_output, &bytes, export_html).await
        });
    }

    session.abort();
    session.current().abort_shell();
    commands.abort_all();
    while commands.join_next().await.is_some() {}
    events.abort();
    let mut stdout = output.stdout.lock().await;
    stdout.flush().await.map_err(|error| error.to_string())
}

async fn forward_events(session: PiSession, output: RpcOutput) {
    let mut replacements = session.subscribe();
    loop {
        let current = Arc::clone(&replacements.borrow_and_update());
        let mut subscription = current.subscribe();
        loop {
            tokio::select! {
                changed = replacements.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
                event = subscription.events.recv() => match event {
                    Ok(event) if event.revision > subscription.snapshot.revision => {
                        match session_event_json(event.event, &current) {
                            Ok(Some(value)) => {
                                if output.emit(value).await.is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                let _ = output.emit(json!({
                                    "type":"extension_error",
                                    "extensionPath":"pi-rs/json-wire",
                                    "event":"projection",
                                    "error":error,
                                })).await;
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        subscription.snapshot = current.snapshot();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn handle_line(
    session: PiSession,
    output: RpcOutput,
    line: &[u8],
    export_html: HtmlExporter,
) -> Result<(), String> {
    let parsed = match serde_json::from_slice::<Value>(line) {
        Ok(value) => value,
        Err(error) => {
            return output
                .emit(error_response(
                    None,
                    "parse",
                    &format!("Failed to parse command: {error}"),
                ))
                .await;
        }
    };
    if parsed.get("type").and_then(Value::as_str) == Some("extension_ui_response") {
        // The JS UI bridge is intentionally outside this P0 protocol change;
        // unsolicited responses are ignored like Pi ignores unknown request IDs.
        return Ok(());
    }
    let fallback_id = parsed.get("id").and_then(Value::as_str).map(str::to_string);
    let fallback_command = parsed
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("parse")
        .to_string();
    let command = match serde_json::from_value::<RpcCommand>(parsed) {
        Ok(command) => command,
        Err(error) => {
            return output
                .emit(error_response(
                    fallback_id.as_deref(),
                    &fallback_command,
                    &format!("Invalid command: {error}"),
                ))
                .await;
        }
    };
    let id = command.id().map(str::to_string);
    let name = command.name();
    match handle_command(&session, command, export_html).await {
        Ok(data) => {
            output
                .emit(success_response(id.as_deref(), name, data))
                .await
        }
        Err(error) => {
            output
                .emit(error_response(id.as_deref(), name, &error))
                .await
        }
    }
}

async fn handle_command(
    session: &PiSession,
    command: RpcCommand,
    export_html: HtmlExporter,
) -> Result<Option<Value>, String> {
    match command {
        RpcCommand::Prompt {
            message,
            images,
            streaming_behavior,
            ..
        } => {
            let current = session.current();
            if current.snapshot().agent.is_running {
                let behavior = streaming_behavior.ok_or_else(|| {
                    "Agent is streaming; specify streamingBehavior as steer or followUp".to_string()
                })?;
                queue_message(&current, message, images, behavior).await?;
            } else {
                spawn_prompt(current, message, images);
            }
            Ok(None)
        }
        RpcCommand::Steer {
            message, images, ..
        } => {
            queue_message(
                &session.current(),
                message,
                images,
                StreamingBehavior::Steer,
            )
            .await?;
            Ok(None)
        }
        RpcCommand::FollowUp {
            message, images, ..
        } => {
            queue_message(
                &session.current(),
                message,
                images,
                StreamingBehavior::FollowUp,
            )
            .await?;
            Ok(None)
        }
        RpcCommand::Abort { .. } => {
            session.abort();
            Ok(None)
        }
        RpcCommand::ClearQueue { .. } => {
            let queue = session
                .current()
                .clear_queue()
                .map_err(|error| error.to_string())?;
            Ok(Some(json!({
                "steering":queue.steering,
                "followUp":queue.follow_up,
            })))
        }
        RpcCommand::NewSession { parent_session, .. } => {
            let current = session.current();
            let path = new_session_path(current.log().path());
            let replacement = match parent_session {
                Some(parent) => {
                    session
                        .new_session_with_parent(current.runtime().cwd(), &path, parent)
                        .await
                }
                None => session.new_session(current.runtime().cwd(), &path).await,
            }
            .map_err(|error| error.to_string())?;
            Ok(Some(
                json!({"cancelled":replacement_cancelled(replacement)}),
            ))
        }
        RpcCommand::GetState { .. } => Ok(Some(state_json(&session.current())?)),
        RpcCommand::SetModel {
            provider, model_id, ..
        } => {
            let current = session.current();
            let model = current
                .runtime()
                .model(&provider.clone().into(), &model_id.clone().into())
                .ok_or_else(|| format!("Model not found: {provider}/{model_id}"))?;
            current
                .set_model(provider.into(), model_id.into())
                .map_err(|error| error.to_string())?;
            clamp_active_thinking(&current)?;
            Ok(Some(model_json(&model)?))
        }
        RpcCommand::CycleModel { .. } => {
            let current = session.current();
            let models = current.runtime().available_models();
            if models.is_empty() {
                return Ok(Some(Value::Null));
            }
            let state = current.snapshot().agent;
            let index = models
                .iter()
                .position(|model| model.provider == state.provider_id && model.id == state.model_id)
                .map_or(0, |index| (index + 1) % models.len());
            let model = models[index].clone();
            current
                .set_model(model.provider.clone(), model.id.clone())
                .map_err(|error| error.to_string())?;
            clamp_active_thinking(&current)?;
            Ok(Some(json!({
                "model":model_json(&model)?,
                "thinkingLevel":current.snapshot().agent.thinking_level,
                "isScoped":false,
            })))
        }
        RpcCommand::GetAvailableModels { .. } => {
            let models = session
                .current()
                .runtime()
                .available_models()
                .iter()
                .map(model_json)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(json!({"models":models})))
        }
        RpcCommand::SetThinkingLevel { level, .. } => {
            let current = session.current();
            let level = clamp_thinking_level(level, &available_thinking_levels(&current));
            current
                .set_thinking_level(level)
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        RpcCommand::CycleThinkingLevel { .. } => {
            let current = session.current();
            let levels = available_thinking_levels(&current);
            if levels == [ThinkingLevel::Off] {
                return Ok(Some(Value::Null));
            }
            let active = current.snapshot().agent.thinking_level;
            let index = levels
                .iter()
                .position(|level| *level == active)
                .map_or(0, |index| (index + 1) % levels.len());
            current
                .set_thinking_level(levels[index])
                .map_err(|error| error.to_string())?;
            Ok(Some(json!({"level":levels[index]})))
        }
        RpcCommand::GetAvailableThinkingLevels { .. } => Ok(Some(json!({
            "levels":available_thinking_levels(&session.current()),
        }))),
        RpcCommand::SetSteeringMode { mode, .. } => {
            session.current().set_steering_mode(mode.into());
            Ok(None)
        }
        RpcCommand::SetFollowUpMode { mode, .. } => {
            session.current().set_follow_up_mode(mode.into());
            Ok(None)
        }
        RpcCommand::Compact {
            custom_instructions,
            ..
        } => {
            let current = session.current();
            current
                .compact(custom_instructions)
                .await
                .map_err(|error| error.to_string())?;
            let document = current.log().load().map_err(|error| error.to_string())?;
            let record = document
                .entries
                .iter()
                .rev()
                .find(|record| matches!(record.entry, SessionEntry::Compaction(_)))
                .ok_or_else(|| "compaction completed without a session entry".to_string())?;
            Ok(Some(compaction_result_json(record, &document)?))
        }
        RpcCommand::SetAutoCompaction { enabled, .. } => {
            session.current().set_auto_compaction_enabled(enabled);
            Ok(None)
        }
        RpcCommand::SetAutoRetry { enabled, .. } => {
            session.current().set_auto_retry_enabled(enabled);
            Ok(None)
        }
        RpcCommand::AbortRetry { .. } => {
            session.current().abort_retry();
            Ok(None)
        }
        RpcCommand::Bash {
            id,
            command,
            exclude_from_context,
        } => {
            let result = session
                .current()
                .execute_shell(
                    command,
                    ShellExecutionOptions {
                        id,
                        exclude_from_context,
                        ..ShellExecutionOptions::default()
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some(omit_null_fields(json!({
                "output":result.output,
                "exitCode":result.exit_code,
                "cancelled":result.cancelled,
                "truncated":result.truncated,
                "fullOutputPath":result.full_output_path,
            }))))
        }
        RpcCommand::AbortBash { .. } => {
            session.current().abort_shell();
            Ok(None)
        }
        RpcCommand::GetSessionStats { .. } => Ok(Some(session_stats_json(&session.current())?)),
        RpcCommand::ExportHtml { output_path, .. } => {
            let current = session.current();
            let path = export_html(&current, output_path.as_deref())?;
            Ok(Some(json!({"path":path})))
        }
        RpcCommand::SwitchSession { session_path, .. } => {
            let replacement = session
                .resume_session(session_path)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some(
                json!({"cancelled":replacement_cancelled(replacement)}),
            ))
        }
        RpcCommand::Fork { entry_id, .. } => {
            let selected_text = session
                .current()
                .log()
                .get_entry(&entry_id)
                .and_then(|record| user_entry_text(&record));
            let replacement = session
                .fork_session(entry_id, ForkPosition::Before)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some(json!({
                "text":selected_text.unwrap_or_default(),
                "cancelled":replacement_cancelled(replacement),
            })))
        }
        RpcCommand::CloneSession { .. } => {
            let leaf = session
                .current()
                .log()
                .leaf_id()
                .ok_or_else(|| "Cannot clone session: no current entry selected".to_string())?;
            let replacement = session
                .fork_session(leaf, ForkPosition::At)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some(
                json!({"cancelled":replacement_cancelled(replacement)}),
            ))
        }
        RpcCommand::GetForkMessages { .. } => {
            let document = session
                .current()
                .log()
                .load()
                .map_err(|error| error.to_string())?;
            let messages = document
                .entries
                .iter()
                .filter_map(|record| {
                    user_entry_text(record).map(|text| json!({"entryId":record.id,"text":text}))
                })
                .collect::<Vec<_>>();
            Ok(Some(json!({"messages":messages})))
        }
        RpcCommand::GetEntries { since, .. } => {
            let document = session
                .current()
                .log()
                .load()
                .map_err(|error| error.to_string())?;
            let start = match since {
                Some(id) => document
                    .entries
                    .iter()
                    .position(|entry| entry.id == id)
                    .map(|index| index + 1)
                    .ok_or_else(|| format!("Entry not found: {id}"))?,
                None => 0,
            };
            let entries = document.entries[start..]
                .iter()
                .map(|record| session_entry_json(record, &document))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(json!({
                "entries":entries,
                "leafId":document.leaf_id(pi_session::MAIN_LANE).map_err(|error| error.to_string())?,
            })))
        }
        RpcCommand::GetTree { .. } => {
            let document = session
                .current()
                .log()
                .load()
                .map_err(|error| error.to_string())?;
            Ok(Some(json!({
                "tree":tree_json(&document)?,
                "leafId":document.leaf_id(pi_session::MAIN_LANE).map_err(|error| error.to_string())?,
            })))
        }
        RpcCommand::GetLastAssistantText { .. } => Ok(Some(json!({
            "text":last_assistant_text(&session.current()),
        }))),
        RpcCommand::SetSessionName { name, .. } => {
            let name = name.trim();
            if name.is_empty() {
                return Err("Session name cannot be empty".to_string());
            }
            session
                .current()
                .set_name(Some(name.to_string()))
                .await
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        RpcCommand::GetMessages { .. } => Ok(Some(json!({
            "messages":session.current().snapshot().agent.messages,
        }))),
        RpcCommand::GetCommands { .. } => {
            let commands = session
                .current()
                .runtime()
                .command_specs()
                .into_iter()
                .map(|command| {
                    json!({
                        "name":command.name,
                        "description":command.description,
                        "source":"extension",
                        "sourceInfo":{
                            "path":"pi-rs://runtime/commands",
                            "source":"pi-rs runtime",
                            "scope":"temporary",
                            "origin":"top-level",
                        },
                    })
                })
                .collect::<Vec<_>>();
            Ok(Some(json!({"commands":commands})))
        }
    }
}

fn spawn_prompt(session: Arc<AgentSession>, message: String, images: Vec<ImageContent>) {
    tokio::spawn(async move {
        if images.is_empty() {
            let _ = session.submit(message).await;
        } else {
            let _ = session.prompt(vec![user_message(message, images)]).await;
        }
    });
}

async fn queue_message(
    session: &Arc<AgentSession>,
    text: String,
    images: Vec<ImageContent>,
    behavior: StreamingBehavior,
) -> Result<(), String> {
    if images.is_empty() {
        match behavior {
            StreamingBehavior::Steer => session.steer(text).await,
            StreamingBehavior::FollowUp => session.follow_up(text).await,
        }
        .map_err(|error| error.to_string())?;
    } else {
        let kind = match behavior {
            StreamingBehavior::Steer => QueueKind::Steer,
            StreamingBehavior::FollowUp => QueueKind::FollowUp,
        };
        session
            .enqueue_message(user_message(text, images), kind)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn user_message(text: String, images: Vec<ImageContent>) -> Message {
    let mut content = vec![ContentBlock::Text(pi_core::TextContent::new(text))];
    content.extend(images.into_iter().map(ContentBlock::Image));
    Message::User(UserMessage {
        content,
        timestamp_ms: now_ms(),
    })
}

fn state_json(session: &AgentSession) -> Result<Value, String> {
    let snapshot = session.snapshot();
    let model = session
        .runtime()
        .model(&snapshot.agent.provider_id, &snapshot.agent.model_id)
        .map(|model| model_json(&model))
        .transpose()?;
    Ok(omit_null_fields(json!({
        "model": model,
        "thinkingLevel": snapshot.agent.thinking_level,
        "isStreaming": snapshot.agent.is_running,
        "isCompacting": snapshot.compaction.is_some(),
        "steeringMode": queue_mode_name(session.steering_mode()),
        "followUpMode": queue_mode_name(session.follow_up_mode()),
        "sessionFile": session.log().is_materialized().then(|| session.log().path()),
        "sessionId": session.log().header().id,
        "sessionName": snapshot.name,
        "autoCompactionEnabled": session.auto_compaction_enabled(),
        "messageCount": snapshot.agent.messages.len(),
        "pendingMessageCount": snapshot.queue.steering.len() + snapshot.queue.follow_up.len(),
    })))
}

fn available_thinking_levels(session: &AgentSession) -> Vec<ThinkingLevel> {
    let state = session.snapshot().agent;
    let Some(model) = session.runtime().model(&state.provider_id, &state.model_id) else {
        return all_thinking_levels().to_vec();
    };
    supported_thinking_levels(&model)
}

const fn all_thinking_levels() -> &'static [ThinkingLevel; 7] {
    &[
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::XHigh,
        ThinkingLevel::Max,
    ]
}

fn supported_thinking_levels(model: &ModelSpec) -> Vec<ThinkingLevel> {
    if !model.reasoning {
        return vec![ThinkingLevel::Off];
    }
    all_thinking_levels()
        .iter()
        .copied()
        .filter(|level| match model.thinking_level_map.get(level.as_str()) {
            Some(None) => false,
            Some(Some(_)) => true,
            None => !matches!(level, ThinkingLevel::XHigh | ThinkingLevel::Max),
        })
        .collect()
}

fn clamp_thinking_level(requested: ThinkingLevel, available: &[ThinkingLevel]) -> ThinkingLevel {
    if available.contains(&requested) {
        return requested;
    }
    let levels = all_thinking_levels();
    let requested_index = levels
        .iter()
        .position(|level| *level == requested)
        .unwrap_or_default();
    levels[requested_index..]
        .iter()
        .chain(levels[..requested_index].iter().rev())
        .copied()
        .find(|level| available.contains(level))
        .or_else(|| available.first().copied())
        .unwrap_or(ThinkingLevel::Off)
}

fn clamp_active_thinking(session: &AgentSession) -> Result<(), String> {
    let active = session.snapshot().agent.thinking_level;
    let effective = clamp_thinking_level(active, &available_thinking_levels(session));
    if effective != active {
        session
            .set_thinking_level(effective)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn model_json(model: &ModelSpec) -> Result<Value, String> {
    let mut value = serde_json::to_value(model).map_err(|error| error.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "model projection is not an object".to_string())?;
    if object.get("baseUrl").is_none_or(Value::is_null) {
        object.insert(
            "baseUrl".to_string(),
            Value::String(model.base_url.clone().unwrap_or_default()),
        );
    }
    Ok(value)
}

fn omit_null_fields(mut value: Value) -> Value {
    if let Value::Object(object) = &mut value {
        object.retain(|_, value| !value.is_null());
    }
    value
}

fn queue_mode_name(mode: QueueMode) -> &'static str {
    match mode {
        QueueMode::All => "all",
        QueueMode::OneAtATime => "one-at-a-time",
    }
}

fn replacement_cancelled(replacement: AgentSessionReplacement) -> bool {
    replacement == AgentSessionReplacement::Cancelled
}

fn new_session_path(current: &Path) -> PathBuf {
    current
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{}.jsonl", uuid::Uuid::now_v7()))
}

fn success_response(id: Option<&str>, command: &str, data: Option<Value>) -> Value {
    let mut object = Map::new();
    if let Some(id) = id {
        object.insert("id".to_string(), Value::String(id.to_string()));
    }
    object.insert("type".to_string(), Value::String("response".to_string()));
    object.insert("command".to_string(), Value::String(command.to_string()));
    object.insert("success".to_string(), Value::Bool(true));
    if let Some(data) = data {
        object.insert("data".to_string(), data);
    }
    Value::Object(object)
}

fn error_response(id: Option<&str>, command: &str, error: &str) -> Value {
    let mut object = Map::new();
    if let Some(id) = id {
        object.insert("id".to_string(), Value::String(id.to_string()));
    }
    object.insert("type".to_string(), Value::String("response".to_string()));
    object.insert("command".to_string(), Value::String(command.to_string()));
    object.insert("success".to_string(), Value::Bool(false));
    object.insert("error".to_string(), Value::String(error.to_string()));
    Value::Object(object)
}

fn session_stats_json(session: &AgentSession) -> Result<Value, String> {
    let document = session.log().load().map_err(|error| error.to_string())?;
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut tool_results = 0usize;
    let mut tool_calls = 0usize;
    let mut total_messages = 0usize;
    for record in &document.entries {
        if let SessionEntry::Message(message) = &record.entry {
            total_messages += 1;
            match message.message.as_standard() {
                Some(Message::User(_)) => user_messages += 1,
                Some(Message::Assistant(message)) => {
                    assistant_messages += 1;
                    tool_calls += message.tool_calls().len();
                }
                Some(Message::ToolResult(_)) => {
                    tool_results += 1;
                }
                Some(Message::Custom(_)) | None => {}
            }
        }
    }
    let usage = aggregate_document_usage(&document);
    let mut value = json!({
        "sessionFile":session.log().is_materialized().then(|| session.log().path()),
        "sessionId":session.log().header().id,
        "userMessages":user_messages,
        "assistantMessages":assistant_messages,
        "toolCalls":tool_calls,
        "toolResults":tool_results,
        "totalMessages":total_messages,
        "tokens":{
            "input":usage.input,
            "output":usage.output,
            "cacheRead":usage.cache_read,
            "cacheWrite":usage.cache_write,
            "total":usage.input.saturating_add(usage.output).saturating_add(usage.cache_read).saturating_add(usage.cache_write),
        },
        "cost":usage.cost.total,
    });
    let context_window = session.active_context_window().filter(|window| *window > 0);
    if let Some(context_window) = context_window {
        let branch = document
            .branch()
            .map_err(|error| error.to_string())?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let messages = session
            .snapshot()
            .agent
            .messages
            .into_iter()
            .map(AgentMessage::from)
            .collect::<Vec<_>>();
        let tokens = current_session_context_tokens(&branch, &messages).map(|usage| usage.tokens);
        value["contextUsage"] = json!({
            "tokens":tokens,
            "contextWindow":context_window,
            "percent":tokens.map(|tokens| tokens as f64 / context_window as f64 * 100.0),
        });
    }
    Ok(omit_null_fields(value))
}

fn tree_json(document: &SessionDocument) -> Result<Vec<Value>, String> {
    fn children(document: &SessionDocument, parent: Option<&str>) -> Result<Vec<Value>, String> {
        document
            .entries
            .iter()
            .filter(|record| record.parent_id.as_deref() == parent)
            .map(|record| {
                let mut node = json!({
                    "entry":session_entry_json(record, document)?,
                    "children":children(document, Some(&record.id))?,
                });
                if let Some(label) = document.labels.get(&record.id) {
                    node["label"] = json!(label);
                }
                Ok(node)
            })
            .collect()
    }
    children(document, None)
}

fn user_entry_text(record: &SessionRecord) -> Option<String> {
    let SessionEntry::Message(message) = &record.entry else {
        return None;
    };
    let Some(Message::User(user)) = message.message.as_standard() else {
        return None;
    };
    Some(
        user.content
            .iter()
            .filter_map(|content| match content {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    )
}

fn last_assistant_text(session: &AgentSession) -> Option<String> {
    session
        .snapshot()
        .agent
        .messages
        .into_iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message)
                if !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) =>
            {
                Some(
                    message
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                )
            }
            _ => None,
        })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_wire_omits_absent_ids_and_data() {
        assert_eq!(
            success_response(None, "abort", None),
            json!({"type":"response","command":"abort","success":true})
        );
        assert_eq!(
            error_response(Some("r1"), "set_model", "missing"),
            json!({
                "id":"r1",
                "type":"response",
                "command":"set_model",
                "success":false,
                "error":"missing",
            })
        );
    }

    #[test]
    fn command_parser_uses_pi_camel_case_fields() {
        let command: RpcCommand = serde_json::from_value(json!({
            "id":"r1",
            "type":"prompt",
            "message":"hello",
            "streamingBehavior":"followUp",
            "images":[{"type":"image","data":"YWJj","mimeType":"image/png"}],
        }))
        .unwrap();
        assert_eq!(command.id(), Some("r1"));
        assert_eq!(command.name(), "prompt");
        let RpcCommand::Prompt {
            images,
            streaming_behavior,
            ..
        } = command
        else {
            panic!("prompt command expected");
        };
        assert_eq!(images.len(), 1);
        assert!(matches!(
            streaming_behavior,
            Some(StreamingBehavior::FollowUp)
        ));
    }

    #[test]
    fn model_wire_requires_a_string_base_url() {
        let model = ModelSpec::new("test", "model", "Model", "test-api");
        let projected = model_json(&model).unwrap();
        assert_eq!(projected["baseUrl"], "");
        assert!(projected.get("compat").is_none());
    }

    #[test]
    fn thinking_levels_respect_reasoning_and_explicit_maps() {
        let plain = ModelSpec::new("test", "plain", "Plain", "test-api");
        assert_eq!(supported_thinking_levels(&plain), vec![ThinkingLevel::Off]);

        let mut reasoning = ModelSpec::new("test", "reasoning", "Reasoning", "test-api");
        reasoning.reasoning = true;
        assert_eq!(
            supported_thinking_levels(&reasoning),
            vec![
                ThinkingLevel::Off,
                ThinkingLevel::Minimal,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
            ]
        );

        reasoning.thinking_level_map.insert("off".to_string(), None);
        reasoning
            .thinking_level_map
            .insert("xhigh".to_string(), Some("xhigh".to_string()));
        reasoning.thinking_level_map.insert("max".to_string(), None);
        assert_eq!(
            supported_thinking_levels(&reasoning),
            vec![
                ThinkingLevel::Minimal,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::XHigh,
            ]
        );
    }
}
