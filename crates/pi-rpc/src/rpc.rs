//! Pi stdin/stdout RPC Adapter.
//!
//! Commands and responses use strict LF-delimited JSON. Session events are
//! projected by `pi_json_wire`, so JSON and RPC cannot drift independently.

use std::future::{Future, poll_fn};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use pi_agent::QueueMode;
use pi_core::{ContentBlock, ImageContent, Message, ModelSpec, StopReason, ThinkingLevel};
use pi_session::{
    AgentSession, AgentSessionReplacement, ForkPosition, PiSession, PromptDisposition,
    SessionDocument, SessionEntry, SessionInput, SessionRecord, ShellExecutionOptions,
    aggregate_document_usage, current_session_context_tokens,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::json_wire::{compaction_result_json, session_entry_json};
use crate::output::RpcOutput;

type PromptReceipt = Box<dyn FnOnce(PromptDisposition) + Send>;

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
    #[serde(other)]
    Unknown,
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
            Self::Unknown => None,
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
            Self::Unknown => "unknown",
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
    run_connection(session, export_html, crate::ui::RpcConnection::stdio()).await
}

/// Runs with the same connection whose UI bridge was installed before session construction.
pub async fn run_connection(
    session: PiSession,
    export_html: HtmlExporter,
    connection: crate::ui::RpcConnection,
) -> Result<(), String> {
    run_connection_with_shutdown(session, export_html, connection, std::future::pending()).await
}

/// Extension-requested shutdown waits for outstanding responses and session settlement.
/// Closing stdin still cancels outstanding work and disposes the connection immediately.
pub async fn run_connection_with_shutdown(
    session: PiSession,
    export_html: HtmlExporter,
    mut connection: crate::ui::RpcConnection,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let output = connection.bridge.output.clone();
    connection.bridge.bind_session(&session.current());
    let mut commands = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    let mut shutdown_requested = false;
    let result = async {
        loop {
            while let Some(result) = commands.try_join_next() {
                result.map_err(|error| error.to_string())??;
            }
            let current = session.current();
            connection.bridge.bind_session(&current);
            let line = tokio::select! {
                biased;
                // Read commands already delivered before an idle shutdown request.
                line = connection.commands.recv() => match line {
                    Some(line) => line,
                    None => break,
                },
                () = output.closed() => return Err("RPC output closed".to_string()),
                result = commands.join_next(), if !commands.is_empty() => {
                    if let Some(result) = result {
                        result.map_err(|error| error.to_string())??;
                    }
                    continue;
                },
                () = &mut shutdown, if !shutdown_requested => {
                    shutdown_requested = true;
                    continue;
                },
                () = current.wait_for_idle(), if shutdown_requested && commands.is_empty() => break,
            };
            let mut line = line?;
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let command_session = session.clone();
            let command_bridge = connection.bridge.clone();
            let mut command = Box::pin(async move {
                handle_line(command_session, command_bridge, &line, export_html).await
            });
            // Pi starts each async handler as its input line arrives. Poll once before
            // spawning so synchronous queries enqueue their responses before EOF can
            // cancel pending commands. The output flush below owns delivery; a handler
            // waiting on output, a hook or a long operation remains concurrent.
            match poll_fn(|cx| Poll::Ready(command.as_mut().poll(cx))).await {
                Poll::Ready(result) => result?,
                Poll::Pending => {
                    commands.spawn(command);
                }
            }
        }
        Ok::<(), String>(())
    }
    .await;

    connection.bridge.close_events();
    connection.bridge.cancel_pending();
    session.abort();
    session.current().abort_shell();
    commands.abort_all();
    while commands.join_next().await.is_some() {}
    let flushed = output.flush().await;
    result.and(flushed)
}

async fn handle_line(
    session: PiSession,
    bridge: Arc<crate::ui::RpcUiBridge>,
    line: &[u8],
    export_html: HtmlExporter,
) -> Result<(), String> {
    let output = &bridge.output;
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
    if let Some(response) =
        dispatch_with_receipt(&session, parsed, export_html, Some(output.clone())).await
    {
        // Publications caused by this command must enter the shared writer first.
        // This waits for synchronous projection, not I/O, so piped queries can
        // still enqueue their response during the initial poll before stdin EOF.
        bridge.publication_barrier(&session.current()).await?;
        output.emit(response).await?;
    }
    Ok(())
}

/// Dispatch a Pi wire request against an in-process managed session.
///
/// Transport adapters own delivery and event subscriptions. This shares command
/// decoding, response correlation and session operations with the stdio adapter
/// without starting a CLI process or acquiring stdin/stdout.
pub async fn dispatch(
    session: &PiSession,
    parsed: Value,
    export_html: HtmlExporter,
) -> Option<Value> {
    dispatch_with_receipt(session, parsed, export_html, None).await
}

async fn dispatch_with_receipt(
    session: &PiSession,
    parsed: Value,
    export_html: HtmlExporter,
    prompt_output: Option<RpcOutput>,
) -> Option<Value> {
    if parsed.get("type").and_then(Value::as_str) == Some("extension_ui_response") {
        // The connection routes UI responses before command dispatch. Embedders using this
        // command-only API own their UI transport; unsolicited responses remain ignored.
        return None;
    }
    let fallback_id = parsed.get("id").and_then(Value::as_str).map(str::to_string);
    let fallback_command = parsed
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("parse")
        .to_string();
    let command = match serde_json::from_value::<RpcCommand>(parsed) {
        Ok(RpcCommand::Unknown) => {
            return Some(error_response(
                fallback_id.as_deref(),
                &fallback_command,
                &format!("Unknown command: {fallback_command}"),
            ));
        }
        Ok(command) => command,
        Err(error) => {
            return Some(error_response(
                fallback_id.as_deref(),
                &fallback_command,
                &format!("Invalid command: {error}"),
            ));
        }
    };
    let id = command.id().map(str::to_string);
    let name = command.name();
    let sent = Arc::new(AtomicBool::new(false));
    let receipt: Option<PromptReceipt> = if matches!(&command, RpcCommand::Prompt { .. }) {
        prompt_output.map(|output| {
            let sent = sent.clone();
            let id = id.clone();
            Box::new(move |disposition| {
                // The runtime invokes this before agent_start; enqueue here rather
                // than after waking a separate command task. The writer owns I/O.
                let _ = output.enqueue(success_response(
                    id.as_deref(),
                    "prompt",
                    Some(disposition_json(disposition)),
                ));
                sent.store(true, Ordering::Release);
            }) as PromptReceipt
        })
    } else {
        None
    };
    let result = handle_command(session, command, export_html, receipt).await;
    if sent.load(Ordering::Acquire) {
        return None;
    }
    Some(match result {
        Ok(data) => success_response(id.as_deref(), name, data),
        Err(error) => error_response(id.as_deref(), name, &error),
    })
}

async fn handle_command(
    session: &PiSession,
    command: RpcCommand,
    export_html: HtmlExporter,
    receipt: Option<PromptReceipt>,
) -> Result<Option<Value>, String> {
    match command {
        RpcCommand::Unknown => Err("Unknown command".into()),
        RpcCommand::Prompt {
            message,
            images,
            streaming_behavior,
            ..
        } => {
            let disposition = start_prompt(
                session.current(),
                message,
                images,
                streaming_behavior,
                receipt,
            )
            .await?;
            Ok(Some(disposition_json(disposition)))
        }
        RpcCommand::Steer {
            message, images, ..
        } => {
            let disposition = queue_message(
                &session.current(),
                message,
                images,
                StreamingBehavior::Steer,
            )
            .await?;
            Ok(Some(disposition_json(disposition)))
        }
        RpcCommand::FollowUp {
            message, images, ..
        } => {
            let disposition = queue_message(
                &session.current(),
                message,
                images,
                StreamingBehavior::FollowUp,
            )
            .await?;
            Ok(Some(disposition_json(disposition)))
        }
        RpcCommand::Abort { .. } => {
            session.current().abort_and_wait().await;
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
            let path = new_session_path(current.history().path());
            let replacement = match parent_session.filter(|parent| !parent.is_empty()) {
                Some(parent) => {
                    session
                        .new_session_with_parent_reference(current.catalog().cwd(), &path, parent)
                        .await
                }
                None => session.new_session(current.catalog().cwd(), &path).await,
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
                .catalog()
                .available_models()
                .into_iter()
                .find(|model| model.provider.as_str() == provider && model.id.as_str() == model_id)
                .ok_or_else(|| format!("Model not found: {provider}/{model_id}"))?;
            current
                .set_model_with_hooks(provider.into(), model_id.into())
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some(model_json(&model)?))
        }
        RpcCommand::CycleModel { .. } => {
            let current = session.current();
            let Some(result) = current
                .cycle_model_with_hooks()
                .await
                .map_err(|error| error.to_string())?
            else {
                return Ok(Some(Value::Null));
            };
            Ok(Some(json!({
                "model":model_json(&result.model)?,
                "thinkingLevel":result.thinking_level,
                "isScoped":result.is_scoped,
            })))
        }
        RpcCommand::GetAvailableModels { .. } => {
            let models = session
                .current()
                .catalog()
                .available_models()
                .iter()
                .map(model_json)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(json!({"models":models})))
        }
        RpcCommand::SetThinkingLevel { level, .. } => {
            let current = session.current();
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
            session
                .current()
                .update_setting(pi_session::SessionSetting::SteeringMode(mode.into()))
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        RpcCommand::SetFollowUpMode { mode, .. } => {
            session
                .current()
                .update_setting(pi_session::SessionSetting::FollowUpMode(mode.into()))
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        RpcCommand::Compact {
            custom_instructions,
            ..
        } => {
            let current = session.current();
            let completed = current
                .compact_with_result(custom_instructions)
                .await
                .map_err(|error| match error {
                    pi_session::SessionError::Cancelled(_) => "Compaction cancelled".to_string(),
                    error => error.to_string(),
                })?;
            let document = current
                .history()
                .load()
                .map_err(|error| error.to_string())?;
            let mut result = compaction_result_json(completed.record(), &document)?;
            result["estimatedTokensAfter"] = json!(completed.estimated_tokens_after());
            Ok(Some(result))
        }
        RpcCommand::SetAutoCompaction { enabled, .. } => {
            session
                .current()
                .update_setting(pi_session::SessionSetting::AutoCompaction(enabled))
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        RpcCommand::SetAutoRetry { enabled, .. } => {
            session
                .current()
                .update_setting(pi_session::SessionSetting::AutoRetry(enabled))
                .map_err(|error| error.to_string())?;
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
                        timeout: None,
                        ..ShellExecutionOptions::default()
                    },
                )
                .await
                .map_err(session_error_message)?;
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
                .history()
                .get_entry(&entry_id)
                .and_then(|record| user_entry_text(&record));
            let replacement = session
                .fork_session(entry_id, ForkPosition::Before)
                .await
                .map_err(fork_error)?;
            Ok(Some(json!({
                "text":(!replacement_cancelled(replacement)).then_some(selected_text).flatten(),
                "cancelled":replacement_cancelled(replacement),
            }))
            .map(omit_null_fields))
        }
        RpcCommand::CloneSession { .. } => {
            let leaf = session
                .current()
                .history()
                .leaf_id()
                .ok_or_else(|| "Cannot clone session: no current entry selected".to_string())?;
            let replacement = session
                .fork_session(leaf, ForkPosition::At)
                .await
                .map_err(fork_error)?;
            Ok(Some(
                json!({"cancelled":replacement_cancelled(replacement)}),
            ))
        }
        RpcCommand::GetForkMessages { .. } => {
            let document = session
                .current()
                .history()
                .load()
                .map_err(|error| error.to_string())?;
            let messages = document
                .entries
                .iter()
                .filter_map(|record| {
                    user_entry_text(record)
                        .filter(|text| !text.is_empty())
                        .map(|text| json!({"entryId":record.id,"text":text}))
                })
                .collect::<Vec<_>>();
            Ok(Some(json!({"messages":messages})))
        }
        RpcCommand::GetEntries { since, .. } => {
            let document = session
                .current()
                .history()
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
                .history()
                .load()
                .map_err(|error| error.to_string())?;
            Ok(Some(json!({
                "tree":tree_json(&document)?,
                "leafId":document.leaf_id(pi_session::MAIN_LANE).map_err(|error| error.to_string())?,
            })))
        }
        RpcCommand::GetLastAssistantText { .. } => Ok(Some(omit_null_fields(json!({
            "text":last_assistant_text(&session.current()),
        })))),
        RpcCommand::SetSessionName { name, .. } => {
            let name = name.trim();
            if name.is_empty() {
                return Err("Session name cannot be empty".to_string());
            }
            session
                .current()
                .set_session_name(Some(name.to_string()))
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        RpcCommand::GetMessages { .. } => Ok(Some(json!({
            "messages":session.current().context_messages().map_err(|error| error.to_string())?,
        }))),
        RpcCommand::GetCommands { .. } => {
            let mut catalog = session.current().catalog().command_catalog();
            catalog.sort_by_key(|command| match command.metadata.source {
                pi_plugin::CommandSource::Extension => 0,
                pi_plugin::CommandSource::Prompt => 1,
                pi_plugin::CommandSource::Skill => 2,
            });
            let commands = catalog
                .into_iter()
                .map(|command| {
                    json!({
                        "name":command.spec.name,
                        "description":command.spec.description,
                        "source":command.metadata.source,
                        "sourceInfo":command.metadata.source_info,
                    })
                })
                .collect::<Vec<_>>();
            Ok(Some(json!({"commands":commands})))
        }
    }
}

async fn start_prompt(
    session: Arc<AgentSession>,
    message: String,
    images: Vec<ImageContent>,
    streaming_behavior: Option<StreamingBehavior>,
    receipt: Option<PromptReceipt>,
) -> Result<PromptDisposition, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let accepted = Arc::new(AtomicBool::new(false));
    let admitted = accepted.clone();
    let running = tokio::spawn(async move {
        session
            .prompt_with_streaming_behavior_and_receipt(
                session_input(message, images),
                streaming_behavior.map(|behavior| match behavior {
                    StreamingBehavior::Steer => pi_plugin::InputStreamingBehavior::Steer,
                    StreamingBehavior::FollowUp => pi_plugin::InputStreamingBehavior::FollowUp,
                }),
                move |disposition| {
                    admitted.store(true, Ordering::Release);
                    if let Some(receipt) = receipt {
                        receipt(disposition);
                    }
                    let _ = sender.send(disposition);
                },
            )
            .await
    });
    // Admission belongs to this request. A disconnected client must not leave
    // an unacknowledged command or input hook running in a detached task.
    let mut pending = PendingPrompt {
        task: Some(running.abort_handle()),
        accepted,
    };
    match receiver.await {
        Ok(disposition) => {
            // Once admitted, the session owns settlement and cancellation.
            pending.task.take();
            Ok(disposition)
        }
        Err(_) => running
            .await
            .map_err(|error| error.to_string())?
            .map(|outcome| outcome.disposition())
            .map_err(session_error_message),
    }
}

struct PendingPrompt {
    task: Option<tokio::task::AbortHandle>,
    accepted: Arc<AtomicBool>,
}

impl Drop for PendingPrompt {
    fn drop(&mut self) {
        if !self.accepted.load(Ordering::Acquire)
            && let Some(task) = self.task.take()
        {
            task.abort();
        }
    }
}

fn disposition_json(disposition: PromptDisposition) -> Value {
    json!({"disposition": match disposition {
        PromptDisposition::Started => "started",
        PromptDisposition::Handled => "handled",
        PromptDisposition::Queued => "queued",
    }})
}

async fn queue_message(
    session: &Arc<AgentSession>,
    text: String,
    images: Vec<ImageContent>,
    behavior: StreamingBehavior,
) -> Result<PromptDisposition, String> {
    let input = session_input(text, images);
    session
        .queue_user_input(
            input,
            match behavior {
                StreamingBehavior::Steer => pi_plugin::InputStreamingBehavior::Steer,
                StreamingBehavior::FollowUp => pi_plugin::InputStreamingBehavior::FollowUp,
            },
        )
        .await
        .map(|outcome| outcome.disposition())
        .map_err(session_error_message)
}

fn session_error_message(error: pi_session::SessionError) -> String {
    match error {
        pi_session::SessionError::Plugin(error) => error.diagnostic_message(),
        pi_session::SessionError::Runtime(message) => message,
        other => other.to_string(),
    }
}

fn session_input(text: String, images: Vec<ImageContent>) -> SessionInput {
    let input = SessionInput::new(text).with_source(pi_plugin::InputSource::Rpc);
    if images.is_empty() {
        input
    } else {
        input.with_images(images)
    }
}

fn state_json(session: &AgentSession) -> Result<Value, String> {
    let snapshot = session.snapshot();
    let model = session
        .catalog()
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
        "sessionFile": session.history().is_persistent().then(|| session.history().path()),
        "sessionId": session.history().header().id,
        "sessionName": snapshot.name,
        "autoCompactionEnabled": session.auto_compaction_enabled(),
        "messageCount": session.context_messages().map_err(|error| error.to_string())?.len(),
        "pendingMessageCount": snapshot.queue.steering.len() + snapshot.queue.follow_up.len(),
    })))
}

fn available_thinking_levels(session: &AgentSession) -> Vec<ThinkingLevel> {
    let state = session.snapshot().agent;
    let Some(model) = session.catalog().model(&state.provider_id, &state.model_id) else {
        return ThinkingLevel::ALL.to_vec();
    };
    model.supported_thinking_levels()
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
    let document = session
        .history()
        .load()
        .map_err(|error| error.to_string())?;
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
                Some(Message::System(_) | Message::Custom(_)) | None => {}
            }
        }
    }
    let usage = aggregate_document_usage(&document);
    let mut value = json!({
        "sessionFile":session.history().is_persistent().then(|| session.history().path()),
        "sessionId":session.history().header().id,
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
            .context_messages()
            .map_err(|error| error.to_string())?;
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
        let mut entries = document
            .entries
            .iter()
            .filter(|record| record.parent_id.as_deref() == parent)
            .collect::<Vec<_>>();
        if parent.is_some() {
            entries.sort_by_key(|record| record.timestamp_ms);
        }
        entries.into_iter()
            .map(|record| {
                let mut node = json!({
                    "entry":session_entry_json(record, document)?,
                    "children":children(document, Some(&record.id))?,
                });
                if let Some(label) = document.labels.get(&record.id) {
                    node["label"] = json!(label);
                    if let Some(change) = document.entries.iter().rev().find(|change| {
                        matches!(&change.entry, SessionEntry::Custom(custom)
                            if custom.custom_type == "pi.coding-agent.legacy.label"
                            && custom.data.as_ref().and_then(|data| data.get("targetId")).and_then(Value::as_str) == Some(record.id.as_str()))
                    }) {
                        node["labelTimestamp"] = session_entry_json(change, document)?["timestamp"].clone();
                    }
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

fn fork_error(error: pi_session::MultiSessionManagerError) -> String {
    match error {
        pi_session::MultiSessionManagerError::Session(
            pi_session::SessionError::InvalidForkTarget(_),
        ) => "Invalid entry ID for forking".to_string(),
        error => error.to_string(),
    }
}

fn last_assistant_text(session: &AgentSession) -> Option<String> {
    session
        .context_messages()
        .ok()?
        .iter()
        .rev()
        .filter_map(|message| match message.as_standard() {
            Some(Message::Assistant(message)) => Some(message),
            _ => None,
        })
        .find(|message| {
            message.stop_reason == StopReason::Stop || message.content.iter()
            .any(|block| matches!(block, ContentBlock::Text(text) if !text.text.trim().is_empty()))
        })
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|text| !text.is_empty())
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
        assert_eq!(plain.supported_thinking_levels(), vec![ThinkingLevel::Off]);

        let mut reasoning = ModelSpec::new("test", "reasoning", "Reasoning", "test-api");
        reasoning.reasoning = true;
        assert_eq!(
            reasoning.supported_thinking_levels(),
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
            reasoning.supported_thinking_levels(),
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
