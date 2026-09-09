mod projection;
mod session_store;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use pi_agent::AgentStateSnapshot;
use pi_core::{
    ContentBlock, ImageContent, Message, ModelId, ModelSpec, PresentationMode, ProviderId,
    ThinkingLevel,
};
use pi_sdk::{Pi, ProductConfig};
use pi_session::{
    AgentSession, AgentSessionSnapshot, BranchQuery, EntryOrder, EntryQuery,
    IsolatedSessionObservation, QueueSnapshot, SessionEntry, SessionInput, SubmitOutcome,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use crate::backend::events::PiEvent;
use crate::state::AppState;

use session_store::{
    content_text, document_token_usage, token_usage, SessionModelCatalog, SessionStore,
    SessionSummary, SessionTokenUsage, StoredIsolatedSession,
};

pub(crate) struct PiRuntimeState {
    store: SessionStore,
    info: PiDesktopInfo,
    forwarders: projection::ForwarderRegistry,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PiDesktopInfo {
    agent_dir: PathBuf,
    provider: String,
    model: String,
    ready: bool,
    reason: Option<String>,
}

pub(crate) fn create_state() -> Result<PiRuntimeState, String> {
    dotenvy::dotenv().ok();
    let agent_dir = crate::agent_paths::agent_dir()?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut config = ProductConfig::new(cwd, agent_dir.clone());
    config.discover_extensions = false;
    let ready = config.api_key.is_some() || !config.base_url.contains("api.openai.com");
    let info = PiDesktopInfo {
        agent_dir: agent_dir.clone(),
        provider: config.provider.clone(),
        model: config
            .model
            .clone()
            .unwrap_or_else(|| config.fallback_model.clone()),
        ready,
        reason: (!ready).then(|| {
            "Set OPENAI_API_KEY or configure a local OPENAI_BASE_URL before sending a prompt"
                .to_string()
        }),
    };
    let sdk = Pi::builder(config)
        .presentation_mode(PresentationMode::Rpc)
        .build()?;
    Ok(PiRuntimeState {
        store: SessionStore::new(sdk.session_manager(), agent_dir),
        info,
        forwarders: Arc::new(Mutex::new(HashSet::new())),
    })
}

#[tauri::command]
pub(crate) fn pi_desktop_info(state: State<'_, PiRuntimeState>) -> PiDesktopInfo {
    state.info.clone()
}

#[tauri::command]
pub(crate) async fn pi_start_thread(
    workspace_id: String,
    prepare_only: Option<bool>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    if prepare_only.unwrap_or(false) {
        let session = pi.store.prepare_thread(&cwd).await?;
        return Ok(json!({ "thread": thread_from_session(&session) }));
    }
    let session = pi.store.start_thread(&cwd).await?;
    ensure_forwarder(&app, &pi, &workspace_id, session.log().header().id.clone()).await?;
    let thread = thread_from_session(&session);
    emit(
        &app,
        &workspace_id,
        "thread/started",
        json!({ "thread": thread.clone() }),
    );
    Ok(json!({ "thread": thread }))
}

#[tauri::command]
pub(crate) async fn pi_resume_thread(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    if let Some(observed) = pi.store.observed_isolated(&thread_id) {
        return Ok(json!({
            "thread": thread_from_observation(
                &observed.observation,
                &observed.parent_thread_id,
                &observed.agent,
            )
        }));
    }
    if let Ok(stored) = pi.store.read_isolated(&thread_id) {
        return Ok(json!({ "thread": thread_from_stored_isolated(&stored)? }));
    }
    let session = pi.store.open(&thread_id).await?;
    ensure_forwarder(&app, &pi, &workspace_id, thread_id.clone()).await?;
    Ok(json!({ "thread": thread_from_session(&session) }))
}

#[tauri::command]
pub(crate) async fn pi_read_thread(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    if let Some(observed) = pi.store.observed_isolated(&thread_id) {
        return Ok(json!({
            "thread": thread_from_observation(
                &observed.observation,
                &observed.parent_thread_id,
                &observed.agent,
            )
        }));
    }
    if let Ok(stored) = pi.store.read_isolated(&thread_id) {
        return Ok(json!({ "thread": thread_from_stored_isolated(&stored)? }));
    }
    let session = pi.store.open(&thread_id).await?;
    ensure_forwarder(&app, &pi, &workspace_id, thread_id.clone()).await?;
    Ok(json!({ "thread": thread_from_session(&session) }))
}

#[tauri::command]
pub(crate) async fn pi_thread_live_subscribe(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    if pi.store.observed_isolated(&thread_id).is_some() {
        return Ok(json!({
            "subscriptionId": format!("{workspace_id}:{thread_id}"),
            "state": "live"
        }));
    }
    if pi.store.read_isolated(&thread_id).is_ok() {
        return Ok(json!({
            "subscriptionId": format!("{workspace_id}:{thread_id}"),
            "state": "snapshot"
        }));
    }
    pi.store.open(&thread_id).await?;
    ensure_forwarder(&app, &pi, &workspace_id, thread_id.clone()).await?;
    Ok(json!({
        "subscriptionId": format!("{workspace_id}:{thread_id}"),
        "state": "live"
    }))
}

#[tauri::command]
pub(crate) async fn pi_thread_live_unsubscribe(
    workspace_id: String,
    thread_id: String,
) -> Result<Value, String> {
    Ok(json!({
        "ok": true,
        "subscriptionId": format!("{workspace_id}:{thread_id}")
    }))
}

#[tauri::command]
pub(crate) async fn pi_fork_thread(
    workspace_id: String,
    thread_id: String,
    entry_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = pi.store.fork(&thread_id, entry_id).await?;
    let new_id = session.log().header().id.clone();
    ensure_forwarder(&app, &pi, &workspace_id, new_id).await?;
    Ok(json!({ "thread": thread_from_session(&session) }))
}

#[tauri::command]
pub(crate) async fn pi_list_threads(
    workspace_id: String,
    _cursor: Option<String>,
    limit: Option<u32>,
    _sort_key: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    let mut sessions = pi.store.list(&cwd)?;
    if let Some(limit) = limit {
        sessions.truncate(limit as usize);
    }
    Ok(json!({
        "data": sessions.iter().map(summary_thread).collect::<Vec<_>>(),
        "nextCursor": null
    }))
}

#[tauri::command]
pub(crate) async fn pi_list_archived_threads(
    workspace_id: String,
    _cursor: Option<String>,
    limit: Option<u32>,
    _sort_key: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    let mut sessions = pi.store.list_archived(&cwd)?;
    if let Some(limit) = limit {
        sessions.truncate(limit as usize);
    }
    Ok(json!({
        "data": sessions.iter().map(summary_thread).collect::<Vec<_>>(),
        "nextCursor": null
    }))
}

#[tauri::command]
pub(crate) async fn pi_archive_thread(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    pi.store.archive(&thread_id).await?;
    emit(
        &app,
        &workspace_id,
        "thread/archived",
        json!({ "threadId": thread_id }),
    );
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub(crate) async fn pi_unarchive_thread(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    pi.store.unarchive(&thread_id).await?;
    let thread = summary_thread(&pi.store.summary(&thread_id)?);
    emit(
        &app,
        &workspace_id,
        "thread/unarchived",
        json!({ "threadId": thread_id, "thread": thread.clone() }),
    );
    Ok(json!({ "ok": true, "thread": thread }))
}

#[tauri::command]
pub(crate) async fn pi_delete_thread(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    pi.store.delete(&thread_id).await?;
    emit(
        &app,
        &workspace_id,
        "thread/deleted",
        json!({ "threadId": thread_id }),
    );
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub(crate) async fn pi_compact_thread(
    workspace_id: String,
    thread_id: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = pi.store.open(&thread_id).await?;
    ensure_forwarder(&app, &pi, &workspace_id, thread_id).await?;
    tauri::async_runtime::spawn(async move {
        let _ = session.compact(None).await;
    });
    Ok(json!({ "status": "started" }))
}

#[tauri::command]
pub(crate) async fn pi_reload_thread(
    workspace_id: String,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = if let Some(thread_id) = thread_id {
        let session = pi.store.reload(&thread_id).await?;
        ensure_forwarder(&app, &pi, &workspace_id, thread_id).await?;
        session
    } else {
        let cwd = workspace_path(&state, &workspace_id).await?;
        pi.store.reload_prepared(&cwd).await?
    };
    Ok(json!({ "thread": thread_from_session(&session) }))
}

#[tauri::command]
pub(crate) async fn pi_set_thread_name(
    workspace_id: String,
    thread_id: String,
    name: String,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    pi.store.rename(&thread_id, name.clone()).await?;
    emit(
        &app,
        &workspace_id,
        "thread/name/updated",
        json!({ "threadId": thread_id, "threadName": name }),
    );
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub(crate) async fn pi_configure_thread(
    _workspace_id: String,
    thread_id: String,
    model: Option<String>,
    effort: Option<String>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let session = pi.store.open(&thread_id).await?;
    configure_session(
        &session,
        model.as_deref().filter(|value| !value.trim().is_empty()),
        effort.as_deref().filter(|value| !value.trim().is_empty()),
    )?;
    Ok(session_configuration(&session))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn pi_send_user_message(
    workspace_id: String,
    thread_id: String,
    text: String,
    model: Option<String>,
    effort: Option<String>,
    _service_tier: Option<Option<String>>,
    images: Option<Vec<String>>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = pi.store.open(&thread_id).await?;
    ensure_forwarder(&app, &pi, &workspace_id, thread_id.clone()).await?;
    configure_session(
        &session,
        model.as_deref().filter(|value| !value.trim().is_empty()),
        effort.as_deref().filter(|value| !value.trim().is_empty()),
    )?;
    let input = session_input(text, images)?;
    // submit owns command dispatch, input hooks, and queuing. Never pre-execute
    // a command here: transformed input must pass through that pipeline once.
    let (_, handle) = pi
        .store
        .handle(&thread_id)
        .ok_or("session is no longer active")?;
    let outcome = session
        .submit(input)
        .await
        .map_err(|error| error.to_string())?;
    Ok(submit_receipt(&outcome, &handle.current()))
}

fn submit_receipt(outcome: &SubmitOutcome, session: &AgentSession) -> Value {
    let mut receipt = match outcome {
        SubmitOutcome::Handled => json!({ "status": "handled" }),
        SubmitOutcome::Queued { entry_id, .. } => {
            json!({ "status": "queued", "entryId": entry_id })
        }
        SubmitOutcome::Agent(_) => json!({ "status": "completed" }),
        _ => json!({ "status": "handled" }),
    };
    receipt["isRunning"] = json!(session.snapshot().agent.is_running);
    receipt["threadId"] = json!(session.log().header().id);
    receipt
}

#[tauri::command]
pub(crate) async fn pi_turn_steer(
    workspace_id: String,
    thread_id: String,
    turn_id: String,
    text: String,
    images: Option<Vec<String>>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let session = pi.store.open(&thread_id).await?;
    ensure_forwarder(&app, &pi, &workspace_id, thread_id.clone()).await?;
    let input = session_input(text, images)?;
    let (_, handle) = pi
        .store
        .handle(&thread_id)
        .ok_or("session is no longer active")?;
    let outcome = session
        .submit(input)
        .await
        .map_err(|error| error.to_string())?;
    let mut receipt = submit_receipt(&outcome, &handle.current());
    receipt["turnId"] = json!(turn_id);
    Ok(receipt)
}

#[tauri::command]
pub(crate) async fn pi_turn_interrupt(
    _workspace_id: String,
    thread_id: String,
    _turn_id: String,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    pi.store.open(&thread_id).await?.abort();
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub(crate) async fn pi_model_list(
    workspace_id: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    let SessionModelCatalog {
        models,
        selected_provider,
        selected_model,
        selected_thinking,
    } = pi.store.model_catalog(&cwd).await?;
    Ok(json!({
        "data": models.iter().map(|model| json!({
            "id": format!("{}/{}", model.provider, model.id),
            "model": model.id,
            "displayName": model.name,
            "description": format!("{} via pi-rs", model.provider),
            "supportedReasoningEfforts": reasoning_efforts(model),
            "defaultReasoningEffort": default_thinking_level(model, selected_thinking)
                .map(ThinkingLevel::as_str),
            "isDefault": model.provider == selected_provider && model.id == selected_model
        })).collect::<Vec<_>>(),
        "selectedThinkingLevel": selected_thinking.as_str()
    }))
}

#[tauri::command]
pub(crate) async fn pi_skills_list(
    workspace_id: String,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let commands = match thread_id {
        Some(id) => pi.store.command_catalog(&id).await?,
        None => {
            let cwd = workspace_path(&state, &workspace_id).await?;
            pi.store
                .prepare_thread(&cwd)
                .await?
                .runtime()
                .command_specs()
        }
    };
    let skills = commands
        .into_iter()
        .filter_map(|command| {
            command.name.strip_prefix("skill:").map(|name| {
                json!({
                    "name": name,
                    "path": "",
                    "description": command.description
                })
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "data": [{ "skills": skills }] }))
}

#[tauri::command]
pub(crate) async fn pi_generate_run_metadata(
    workspace_id: String,
    prompt: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Value, String> {
    let task = prompt.trim();
    if task.is_empty() {
        return Err("Task prompt is required".to_string());
    }
    let request = crate::shared::ai_tasks_core::build_run_metadata_prompt(task);
    let response = run_background_prompt(&state, &pi, &workspace_id, request, None).await?;
    crate::shared::ai_tasks_core::parse_run_metadata(&response)
}

pub(crate) async fn run_background_prompt(
    state: &AppState,
    pi: &PiRuntimeState,
    workspace_id: &str,
    prompt: String,
    model: Option<&str>,
) -> Result<String, String> {
    let cwd = workspace_path(state, workspace_id).await?;
    run_background_prompt_in_cwd(pi, &cwd, prompt, model).await
}

async fn run_background_prompt_in_cwd(
    pi: &PiRuntimeState,
    cwd: &Path,
    prompt: String,
    model: Option<&str>,
) -> Result<String, String> {
    let session = pi.store.create(cwd).await?;
    let session_id = session.log().header().id.clone();

    let result = async {
        if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
            set_session_model(&session, model)?;
        }

        let outcome = session
            .prompt(prompt)
            .await
            .map_err(|error| error.to_string())?;
        let response = outcome
            .new_messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(message) => {
                    let text = message
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_string();
                    (!text.is_empty()).then_some(text)
                }
                _ => None,
            })
            .ok_or_else(|| "No response was generated".to_string())?;
        Ok(response)
    }
    .await;

    let _ = pi.store.delete(&session_id).await;
    result
}

fn set_session_model(session: &AgentSession, requested: &str) -> Result<(), String> {
    configure_session(session, Some(requested), None)
}

fn configure_session(
    session: &AgentSession,
    requested_model: Option<&str>,
    requested_effort: Option<&str>,
) -> Result<(), String> {
    let state = session.runtime().agent().state();
    let models = session.runtime().available_models();
    let selected = requested_model
        .map(|requested| {
            select_model(&models, &state.provider_id, requested)
                .ok_or_else(|| format!("unknown model: {requested}"))
        })
        .transpose()?;
    let thinking_level = requested_effort
        .map(str::parse::<ThinkingLevel>)
        .transpose()?;
    if let Some(model) = selected {
        if state.provider_id != model.provider || state.model_id != model.id {
            session
                .set_model(model.provider, model.id)
                .map_err(|error| error.to_string())?;
        }
    }
    if let Some(level) = thinking_level {
        session
            .set_thinking_level(level)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn session_configuration(session: &AgentSession) -> Value {
    let state = session.runtime().agent().state();
    json!({
        "provider": state.provider_id,
        "model": state.model_id,
        "thinkingLevel": state.thinking_level.as_str()
    })
}

fn select_model(
    models: &[ModelSpec],
    current_provider: &ProviderId,
    requested: &str,
) -> Option<ModelSpec> {
    if let Some(model) = models
        .iter()
        .find(|model| format!("{}/{}", model.provider, model.id) == requested)
    {
        return Some(model.clone());
    }
    let candidates = models
        .iter()
        .filter(|model| model.id.as_str() == requested)
        .collect::<Vec<_>>();
    candidates
        .iter()
        .find(|model| model.provider == *current_provider)
        .or_else(|| candidates.first())
        .map(|model| (*model).clone())
}

fn reasoning_efforts(model: &ModelSpec) -> Vec<Value> {
    model
        .supported_thinking_levels()
        .into_iter()
        .map(|level| {
            json!({
                "reasoningEffort": level.as_str(),
                "description": format!("{} reasoning effort", level.as_str())
            })
        })
        .collect()
}

fn default_thinking_level(model: &ModelSpec, current: ThinkingLevel) -> Option<ThinkingLevel> {
    (!model.supported_thinking_levels().is_empty()).then(|| model.clamp_thinking_level(current))
}

async fn ensure_forwarder(
    app: &AppHandle,
    pi: &PiRuntimeState,
    workspace_id: &str,
    thread_id: String,
) -> Result<(), String> {
    let key = pi.store.forwarder_key(&thread_id)?;
    {
        let mut forwarders = pi.forwarders.lock().await;
        if !forwarders.insert(key.clone()) {
            return Ok(());
        }
    }
    let live = match pi.store.subscribe(&thread_id) {
        Ok(live) => live,
        Err(error) => {
            pi.forwarders.lock().await.remove(&key);
            return Err(error);
        }
    };
    let app = app.clone();
    let workspace_id = workspace_id.to_string();
    let store = pi.store.clone();
    projection::spawn_session_forwarder(
        app,
        workspace_id,
        thread_id,
        key,
        live,
        store,
        Arc::clone(&pi.forwarders),
    );
    Ok(())
}

async fn workspace_path(state: &AppState, workspace_id: &str) -> Result<PathBuf, String> {
    state
        .workspaces
        .lock()
        .await
        .get(workspace_id)
        .map(|entry| PathBuf::from(&entry.path))
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))
}

fn summary_thread(summary: &SessionSummary) -> Value {
    json!({
        "id": summary.id,
        "name": summary.title,
        "preview": summary.title,
        "cwd": summary.cwd,
        "createdAt": summary.updated_at_ms,
        "updatedAt": summary.updated_at_ms,
        "model": summary.model,
        "source": "pi-rs",
        "status": { "type": "idle" },
        "messageCount": summary.message_count,
        "turns": []
    })
}

fn command_catalog_json(session: &AgentSession) -> Value {
    json!(session
        .runtime()
        .command_specs()
        .iter()
        .map(|command| json!({
            "name": command.name,
            "description": command.description,
            "argumentHint": command.argument_hint,
        }))
        .collect::<Vec<_>>())
}

fn thread_from_subscription(live: &session_store::LiveSession) -> Value {
    // Hydrate the subscription snapshot so subsequent events cannot duplicate
    // messages already included in the replacement payload.
    let session = live.primary().expect("replacement is a primary session");
    let mut thread = thread_from_snapshot(
        &live.subscription.snapshot,
        &session.log().header().id,
        session.runtime().cwd(),
        SessionTokenUsage {
            total_tokens: None,
            context_tokens: None,
            model_context_window: session.active_context_window(),
        },
        json!("pi-rs"),
        None,
        message_entry_ids(session),
    );
    thread["commands"] = command_catalog_json(session);
    thread
}

fn thread_from_session(session: &AgentSession) -> Value {
    let snapshot = session.snapshot();
    let id = session.log().header().id;
    let mut thread = thread_from_snapshot(
        &snapshot,
        &id,
        session.runtime().cwd(),
        token_usage(session),
        json!("pi-rs"),
        None,
        message_entry_ids(session),
    );
    thread["commands"] = command_catalog_json(session);
    thread
}

fn thread_from_observation(
    observation: &IsolatedSessionObservation,
    parent_thread_id: &str,
    agent: &str,
) -> Value {
    let snapshot = observation.snapshot();
    let id = observation.session_id();
    let cwd = observation.cwd();
    let token_usage = observation
        .usage_snapshot()
        .map(|snapshot| SessionTokenUsage {
            total_tokens: Some(snapshot.usage.total_tokens),
            context_tokens: snapshot.context_tokens,
            model_context_window: snapshot.model_context_window,
        })
        .unwrap_or_default();
    thread_from_snapshot(
        &snapshot,
        &id,
        &cwd,
        token_usage,
        json!({
            "subAgent": {
                "kind": agent,
                "threadSpawn": {
                    "parentThreadId": parent_thread_id,
                    "agentRole": agent,
                }
            }
        }),
        Some(agent),
        HashMap::new(),
    )
}

fn thread_from_stored_isolated(stored: &StoredIsolatedSession) -> Result<Value, String> {
    let branch = stored
        .document
        .branch()
        .map_err(|error| error.to_string())?;
    let messages = branch
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Message(entry) => entry.message.as_standard().cloned(),
            SessionEntry::CustomMessage(entry) => Some(entry.to_message(record.timestamp_ms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let model_selection = branch.iter().rev().find_map(|record| match &record.entry {
        SessionEntry::ModelChange(change) => {
            Some((change.provider.clone(), change.model_id.clone()))
        }
        SessionEntry::Message(entry) => match entry.message.as_standard() {
            Some(Message::Assistant(message)) => {
                Some((message.provider.clone(), message.model.clone()))
            }
            _ => None,
        },
        _ => None,
    });
    let thinking_level = branch
        .iter()
        .rev()
        .find_map(|record| match &record.entry {
            SessionEntry::ThinkingLevelChange(change) => change.thinking_level.parse().ok(),
            _ => None,
        })
        .unwrap_or(ThinkingLevel::Off);
    let (provider_id, model_id) =
        model_selection.unwrap_or_else(|| (ProviderId::new("unknown"), ModelId::new("unknown")));
    let snapshot = AgentSessionSnapshot {
        revision: 0,
        agent: AgentStateSnapshot {
            system_prompt: String::new(),
            provider_id,
            model_id,
            thinking_level,
            active_tools: Vec::new(),
            messages,
            is_running: false,
            streaming_message: None,
            pending_tool_calls: HashSet::new(),
            error_message: None,
        },
        queue: QueueSnapshot::default(),
        compaction: None,
        auto_retry: None,
        bash: None,
        name: stored.document.name.clone(),
    };
    Ok(thread_from_snapshot(
        &snapshot,
        &stored.document.header.id,
        &stored.document.header.cwd,
        document_token_usage(&stored.document, None),
        json!({
            "subAgent": {
                "kind": stored.agent,
                "threadSpawn": {
                    "parentThreadId": stored.parent_thread_id,
                    "agentRole": stored.agent,
                }
            }
        }),
        Some(&stored.agent),
        HashMap::new(),
    ))
}

type MessageEntryIds = HashMap<bool, VecDeque<(i64, String)>>;

fn message_key(message: &Message) -> Option<(bool, i64)> {
    match message {
        Message::User(message) => Some((true, message.timestamp_ms)),
        Message::Assistant(message) => Some((false, message.timestamp_ms)),
        _ => None,
    }
}

fn message_entry_ids(session: &AgentSession) -> MessageEntryIds {
    let query = BranchQuery {
        entries: EntryQuery {
            order: EntryOrder::OldestFirst,
            ..EntryQuery::default()
        },
        ..BranchQuery::default()
    };
    let Ok(records) = session.log().find_entries_on_branch(&query) else {
        return HashMap::new();
    };
    let mut ids = MessageEntryIds::new();
    for record in records {
        let SessionEntry::Message(entry) = &record.entry else {
            continue;
        };
        let Some(message) = entry.message.as_standard() else {
            continue;
        };
        let Some(key) = message_key(message) else {
            continue;
        };
        ids.entry(key.0).or_default().push_back((key.1, record.id));
    }
    ids
}

fn take_message_entry_id(ids: &mut MessageEntryIds, message: &Message) -> Option<String> {
    const MAX_TIMESTAMP_DRIFT_MS: u64 = 1_000;

    let (is_user, timestamp_ms) = message_key(message)?;
    let candidates = ids.get_mut(&is_user)?;
    let index = candidates
        .iter()
        .position(|(candidate_timestamp, _)| *candidate_timestamp == timestamp_ms)
        .or_else(|| {
            candidates
                .iter()
                .enumerate()
                .min_by_key(|(_, (candidate_timestamp, _))| {
                    candidate_timestamp.abs_diff(timestamp_ms)
                })
                .filter(|(_, (candidate_timestamp, _))| {
                    candidate_timestamp.abs_diff(timestamp_ms) <= MAX_TIMESTAMP_DRIFT_MS
                })
                .map(|(index, _)| index)
        })?;
    candidates.remove(index).map(|(_, id)| id)
}

fn thread_from_snapshot(
    snapshot: &AgentSessionSnapshot,
    id: &str,
    cwd: &Path,
    token_usage: SessionTokenUsage,
    source: Value,
    display_name: Option<&str>,
    mut message_entry_ids: MessageEntryIds,
) -> Value {
    let messages = &snapshot.agent.messages;
    let mut turns = Vec::<Value>::new();
    let mut items = Vec::<Value>::new();
    let mut turn_started_at = 0_i64;
    let mut turn_index = 0_usize;
    let mut preview = String::new();
    let mut created_at = 0_i64;
    let mut updated_at = 0_i64;
    let mut pending_tools = HashMap::<String, (String, Value)>::new();
    let historical_context = HistoricalToolContext {
        cwd,
        parent_thread_id: id,
    };

    let flush = |turns: &mut Vec<Value>,
                 items: &mut Vec<Value>,
                 started_at: i64,
                 index: usize,
                 is_running: bool| {
        if !items.is_empty() {
            turns.push(json!({
                "id": format!("turn-{index}-{started_at}"),
                "status": if is_running { "inProgress" } else { "completed" },
                "createdAt": started_at,
                "items": std::mem::take(items)
            }));
        }
    };

    for message in messages {
        let entry_id = take_message_entry_id(&mut message_entry_ids, message);
        let timestamp = message_timestamp(message);
        if created_at == 0 {
            created_at = timestamp;
        }
        updated_at = updated_at.max(timestamp);
        match message {
            Message::User(message) => {
                flush(&mut turns, &mut items, turn_started_at, turn_index, false);
                turn_index += 1;
                turn_started_at = message.timestamp_ms;
                let content = user_content(&message.content);
                if preview.is_empty() {
                    preview = content
                        .iter()
                        .filter_map(|item| item.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" ");
                }
                items.push(json!({
                    "id": format!("user-{turn_index}-{}", message.timestamp_ms),
                    "type": "userMessage",
                    "entryId": entry_id,
                    "content": content
                }));
            }
            Message::Assistant(message) => {
                for (content_index, block) in message.content.iter().enumerate() {
                    match block {
                        ContentBlock::Text(text) if !text.text.is_empty() => items.push(json!({
                            "id": format!("agent-{turn_index}-{}-{content_index}", message.timestamp_ms),
                            "type": "agentMessage",
                            "entryId": entry_id,
                            "text": text.text
                        })),
                        ContentBlock::Thinking(thinking) if !thinking.thinking.is_empty() => {
                            items.push(json!({
                                "id": format!("reasoning-{turn_index}-{}-{content_index}", message.timestamp_ms),
                                "type": "reasoning",
                                "summary": [],
                                "content": [thinking.thinking]
                            }));
                        }
                        ContentBlock::ToolCall(call) => {
                            pending_tools.insert(
                                call.id.to_string(),
                                (call.name.clone(), call.arguments.clone()),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Message::ToolResult(result) => {
                let (name, arguments) = pending_tools
                    .remove(result.tool_call_id.as_str())
                    .unwrap_or_else(|| (result.tool_name.clone(), Value::Null));
                items.push(historical_tool_item(
                    result.tool_call_id.as_str(),
                    &name,
                    arguments,
                    &content_text(&result.content),
                    result.is_error,
                    result.details.as_ref(),
                    historical_context,
                ));
            }
            Message::Custom(custom) if custom.display => {
                items.push(json!({
                    "id": format!("custom-{turn_index}-{}", custom.timestamp_ms),
                    "type": "userMessage",
                    "content": [{ "type": "text", "text": content_text(&custom.content.to_blocks()) }]
                }));
            }
            Message::Custom(_) => {}
        }
    }
    for (tool_id, (name, arguments)) in pending_tools {
        items.push(historical_tool_item(
            &tool_id,
            &name,
            arguments,
            "",
            false,
            None,
            historical_context,
        ));
    }
    flush(
        &mut turns,
        &mut items,
        turn_started_at,
        turn_index,
        snapshot.agent.is_running,
    );
    let active_turn_id = snapshot.agent.is_running.then(|| {
        turns
            .last()
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });
    let now = chrono::Utc::now().timestamp_millis();
    json!({
        "id": id,
        "name": snapshot.name.as_deref().or(display_name),
        "preview": display_name.unwrap_or_else(|| {
            if preview.is_empty() { "Untitled session" } else { &preview }
        }),
        "cwd": cwd,
        "createdAt": if created_at == 0 { now } else { created_at },
        "updatedAt": if updated_at == 0 { now } else { updated_at },
        "model": snapshot.agent.model_id.as_str(),
        "modelProvider": snapshot.agent.provider_id.as_str(),
        "effort": snapshot.agent.thinking_level.as_str(),
        "source": source,
        "status": { "type": if snapshot.agent.is_running { "active" } else { "idle" } },
        "activeTurnId": active_turn_id,
        "tokenUsage": token_usage,
        "turns": turns
    })
}

#[derive(Clone, Copy)]
struct HistoricalToolContext<'a> {
    cwd: &'a Path,
    parent_thread_id: &'a str,
}

fn historical_tool_item(
    id: &str,
    name: &str,
    arguments: Value,
    output: &str,
    is_error: bool,
    details: Option<&Value>,
    context: HistoricalToolContext<'_>,
) -> Value {
    let HistoricalToolContext {
        cwd,
        parent_thread_id,
    } = context;
    let status = if is_error { "failed" } else { "completed" };
    if name == "bash" {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("bash");
        json!({
            "id": id,
            "type": "commandExecution",
            "command": command,
            "cwd": cwd,
            "status": status,
            "aggregatedOutput": output
        })
    } else if name == "subagent" {
        let agent = details
            .and_then(|value| value.get("agent"))
            .and_then(Value::as_str)
            .or_else(|| arguments.get("agent").and_then(Value::as_str))
            .unwrap_or("subagent");
        let child_thread_id = details
            .and_then(|value| value.get("sessionId"))
            .and_then(Value::as_str);
        let total_tokens = details
            .and_then(|value| value.get("usage"))
            .and_then(|usage| usage.get("totalTokens"))
            .and_then(Value::as_u64);
        let prompt = arguments
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or_default();
        json!({
            "id": id,
            "type": "collabToolCall",
            "tool": "spawn",
            "senderThreadId": parent_thread_id,
            "newThreadId": child_thread_id,
            "newAgentRole": agent,
            "prompt": prompt,
            "status": status,
            "result": output,
            "agentStatuses": child_thread_id.map(|thread_id| vec![json!({
                "threadId": thread_id,
                "agentRole": agent,
                "status": status,
                "totalTokens": total_tokens,
            })]).unwrap_or_default(),
        })
    } else {
        json!({
            "id": id,
            "type": "mcpToolCall",
            "server": "pi",
            "tool": name,
            "arguments": arguments,
            "status": status,
            "result": output
        })
    }
}

fn user_content(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
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

fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::User(message) => message.timestamp_ms,
        Message::Assistant(message) => message.timestamp_ms,
        Message::ToolResult(message) => message.timestamp_ms,
        Message::Custom(message) => message.timestamp_ms,
    }
}

fn session_input(text: String, images: Option<Vec<String>>) -> Result<SessionInput, String> {
    let input = SessionInput::new(text);
    let Some(images) = images else {
        return Ok(input);
    };
    let images = images
        .into_iter()
        .enumerate()
        .map(|(index, image)| {
            image_content_from_data_url(&image)
                .map_err(|error| format!("Invalid image attachment {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(input.with_images(images))
}

fn image_content_from_data_url(value: &str) -> Result<ImageContent, String> {
    let (metadata, data) = value
        .split_once(',')
        .ok_or_else(|| "expected a base64 data URL".to_string())?;
    let mime_type = metadata
        .strip_prefix("data:")
        .and_then(|metadata| metadata.strip_suffix(";base64"))
        .filter(|mime_type| mime_type.starts_with("image/"))
        .ok_or_else(|| "expected data:image/<type>;base64,...".to_string())?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|error| format!("invalid base64 payload: {error}"))?;
    if bytes.is_empty() {
        return Err("image payload is empty".to_string());
    }
    Ok(ImageContent {
        data: data.to_string(),
        mime_type: mime_type.to_string(),
    })
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
    use pi_agent::AgentOptions;
    use pi_core::{ModelId, ProviderId};
    use pi_runtime::PiRuntime;
    use pi_session::{
        AgentSessionOptions, AgentSessionRuntimeRequest, AgentSessionRuntimeTarget,
        MultiSessionManager, SessionHeader, SessionLog,
    };
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

    use super::*;

    #[derive(Default)]
    struct CommandFixture {
        builds: std::sync::atomic::AtomicUsize,
        calls: std::sync::atomic::AtomicUsize,
        fail_reload: std::sync::atomic::AtomicBool,
        binding: pi_session::PluginContextBinding,
        wait_for_abort: bool,
        skill_paths: Vec<PathBuf>,
    }

    struct FixturePlugin(Arc<CommandFixture>, usize);
    struct FixtureCommand(Arc<CommandFixture>, usize);

    #[pi_core::agent_plugin]
    impl pi_core::AgentPlugin for FixturePlugin {
        fn id(&self) -> pi_core::PluginId {
            pi_core::PluginId::new("desktop-commands")
        }
        fn register(&self, context: &mut pi_core::RegisterContext<'_>) -> pi_core::Result<()> {
            context.register_command(Arc::new(FixtureCommand(Arc::clone(&self.0), self.1)))
        }
    }

    #[pi_core::__plugin_async_trait]
    impl pi_core::Command for FixtureCommand {
        fn spec(&self) -> pi_core::CommandSpec {
            pi_core::CommandSpec {
                name: "native".into(),
                description: format!("generation {}", self.1),
                argument_hint: Some("[action]".into()),
            }
        }
        async fn execute(
            &self,
            context: pi_core::CommandContext,
            arguments: String,
        ) -> Result<pi_core::CommandOutcome, pi_core::CommandError> {
            self.0
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match arguments.as_str() {
                "reload" => {
                    context.session.reload().await?;
                }
                "reload-send" => {
                    let next = context.session.reload().await?;
                    next.send_user_message(
                        pi_core::CustomMessageContent::Text("after reload".into()),
                        Default::default(),
                    )
                    .await?;
                }
                "transform" => {
                    return Ok(pi_core::CommandOutcome::TransformInput("expanded".into()))
                }
                "error" => return Err(pi_core::CommandError::Execution("fixture error".into())),
                _ => context
                    .ui
                    .notify(pi_core::NoticeLevel::Info, "native notice")?,
            }
            Ok(pi_core::CommandOutcome::Handled)
        }
    }

    fn scripted_store(agent_dir: PathBuf) -> SessionStore {
        scripted_store_with_fixture(agent_dir, Arc::new(CommandFixture::default()))
    }

    struct ScriptedFactory(Arc<CommandFixture>);

    #[pi_core::__plugin_async_trait]
    impl pi_session::AgentSessionRuntimeFactory for ScriptedFactory {
        fn session_registered(&self, session: &pi_session::PiSession) {
            self.0.binding.bind(session.clone());
        }
        async fn prepare(
            &self,
            request: AgentSessionRuntimeRequest,
        ) -> Result<pi_session::PreparedAgentSession, pi_session::SessionError> {
            let fixture = Arc::clone(&self.0);
            if fixture
                .fail_reload
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(pi_session::SessionError::Runtime(
                    "fixture reload failed".to_string(),
                ));
            }
            let generation = fixture
                .builds
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let cwd = match &request.target {
                AgentSessionRuntimeTarget::Create { cwd, .. } => cwd.clone(),
                AgentSessionRuntimeTarget::Open { path } => SessionLog::read(path)?.header.cwd,
                AgentSessionRuntimeTarget::Reuse { log } => log.header().cwd,
            };
            let context = Arc::new(pi_session::PiPluginContext::new(
                PresentationMode::Rpc,
                true,
                fixture.binding.clone(),
            ));
            let turn = if fixture.wait_for_abort {
                ScriptedTurn::WaitForAbort
            } else {
                ScriptedTurn::Text("hello from pi-rs".to_string())
            };
            let mut skills = pi_plugin_skills::SkillLoaderOptions::new(&cwd, cwd.join("agent"));
            skills.include_defaults = false;
            skills.additional_paths = fixture.skill_paths.clone();
            let runtime = PiRuntime::builder()
                .plugin_context(context.clone())
                .agent_plugin(pi_plugin_skills::SkillsPlugin::new(skills))
                .agent_plugin(FixturePlugin(fixture, generation))
                .provider_plugin(ScriptedProviderPlugin::scripted([turn]))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("desktop-test"),
                    cwd,
                    ..AgentOptions::default()
                })
                .build()?;
            let prepared = match request.target {
                AgentSessionRuntimeTarget::Create {
                    path,
                    parent_session,
                    session_id,
                    ..
                } => {
                    AgentSession::prepare_create_with_options(
                        runtime,
                        path,
                        AgentSessionOptions::default()
                            .parent_session_path(parent_session)
                            .session_id(session_id),
                    )
                    .await
                }
                AgentSessionRuntimeTarget::Open { path } => {
                    AgentSession::prepare_open_with_options(
                        runtime,
                        path,
                        AgentSessionOptions::default(),
                    )
                    .await
                }
                AgentSessionRuntimeTarget::Reuse { log } => {
                    AgentSession::prepare_reuse_with_options(
                        runtime,
                        log,
                        AgentSessionOptions::default(),
                    )
                    .await
                }
            }?;
            context.bind_generation_session(prepared.session());
            Ok(prepared)
        }
    }

    fn scripted_store_with_fixture(
        agent_dir: PathBuf,
        fixture: Arc<CommandFixture>,
    ) -> SessionStore {
        SessionStore::new(
            MultiSessionManager::new(ScriptedFactory(fixture)),
            agent_dir,
        )
    }

    async fn observe_latest(live: &mut session_store::LiveSession) -> Arc<AgentSession> {
        let changes = live.changes.as_mut().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), changes.changed())
            .await
            .unwrap()
            .unwrap();
        let current = Arc::clone(&changes.borrow_and_update());
        live.replace(Arc::clone(&current));
        current
    }

    async fn wait_for_standard_message_entries(
        subscription: &mut pi_session::AgentSessionSubscription,
        expected: usize,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut count = 0;
            while count < expected {
                let event = subscription.events.recv().await.unwrap();
                if matches!(
                    event.event,
                    pi_session::AgentSessionEvent::EntryAppended { ref entry }
                        if matches!(
                            &entry.entry,
                            SessionEntry::Message(message)
                                if message.message.as_standard().is_some()
                        )
                ) {
                    count += 1;
                }
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn workspace_prepares_commands_and_skills_once_and_reuses_the_draft_on_send() {
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let skill_root = directory.path().join("skills/startup");
        std::fs::create_dir_all(&skill_root).unwrap();
        std::fs::write(skill_root.join("SKILL.md"),
            "---\nname: startup\ndescription: Workspace startup checks\n---\nInspect the project before editing.\n",
        ).unwrap();
        let fixture = Arc::new(CommandFixture {
            skill_paths: vec![skill_root],
            ..CommandFixture::default()
        });
        let store =
            scripted_store_with_fixture(directory.path().join("agent"), Arc::clone(&fixture));
        let (first, second) = tokio::join!(
            store.prepare_thread(directory.path()),
            store.prepare_thread(directory.path()),
        );
        let draft = first.unwrap();
        assert!(Arc::ptr_eq(&draft, &second.unwrap()));
        assert_eq!(fixture.builds.load(SeqCst), 1);
        let thread = thread_from_session(&draft);
        let commands = thread["commands"].as_array().unwrap();
        assert!(commands.iter().any(|command| command["name"] == "native"));
        assert!(commands
            .iter()
            .any(|command| command["name"] == "skill:startup"));
        assert!(!draft.log().path().exists());
        assert!(store.list(directory.path()).unwrap().is_empty());

        // The workspace model selector reads the same prepared runtime.
        store.model_catalog(directory.path()).await.unwrap();
        assert_eq!(fixture.builds.load(SeqCst), 1);
        let started = store.start_thread(directory.path()).await.unwrap();
        assert!(Arc::ptr_eq(&draft, &started));
        assert_eq!(fixture.builds.load(SeqCst), 1);
        assert_eq!(command_catalog_json(&started), thread["commands"]);
        assert!(matches!(
            started.submit("/native notice").await.unwrap(),
            SubmitOutcome::Handled
        ));
        assert_eq!(fixture.calls.load(SeqCst), 1);
        assert!(!started.log().path().exists());
        assert!(matches!(
            started.submit("/skill:startup").await.unwrap(),
            SubmitOutcome::Agent(_)
        ));
        assert!(started.log().path().exists());
        assert_eq!(store.list(directory.path()).unwrap().len(), 1);

        let next = store.start_thread(directory.path()).await.unwrap();
        assert_ne!(next.log().header().id, started.log().header().id);
        assert_eq!(fixture.builds.load(SeqCst), 2);
    }

    #[tokio::test]
    async fn workspace_reload_rebuilds_the_prepared_generation_without_materializing_it() {
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let fixture = Arc::new(CommandFixture::default());
        let store =
            scripted_store_with_fixture(directory.path().join("agent"), Arc::clone(&fixture));
        let first = store.prepare_thread(directory.path()).await.unwrap();
        assert_eq!(fixture.builds.load(SeqCst), 1);

        let reloaded = store.reload_prepared(directory.path()).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &reloaded));
        assert_eq!(fixture.builds.load(SeqCst), 2);
        assert_eq!(
            reloaded.runtime().command_specs()[0].description,
            "generation 2"
        );
        assert!(!reloaded.log().path().exists());
        assert!(store.list(directory.path()).unwrap().is_empty());
        let started = store.start_thread(directory.path()).await.unwrap();
        assert!(Arc::ptr_eq(&reloaded, &started));

        let active = store.reload(&started.log().header().id).await.unwrap();
        assert!(!Arc::ptr_eq(&started, &active));
        assert_eq!(fixture.builds.load(SeqCst), 3);
        assert_eq!(
            active.runtime().command_specs()[0].description,
            "generation 3"
        );
        assert!(!active.log().path().exists());
    }

    #[tokio::test]
    async fn workspace_drafts_are_separate_from_other_workspaces_and_background_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let other = directory.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let first = store.prepare_thread(directory.path()).await.unwrap();
        let second = store.prepare_thread(&other).await.unwrap();
        assert_ne!(first.log().header().id, second.log().header().id);
        let background = store.create(directory.path()).await.unwrap();
        assert_ne!(background.log().header().id, first.log().header().id);
        assert!(Arc::ptr_eq(
            &first,
            &store.start_thread(directory.path()).await.unwrap()
        ));
        assert!(Arc::ptr_eq(
            &second,
            &store.start_thread(&other).await.unwrap()
        ));
    }

    #[tokio::test]
    async fn failed_workspace_preparation_can_be_retried() {
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let fixture = Arc::new(CommandFixture::default());
        let store =
            scripted_store_with_fixture(directory.path().join("agent"), Arc::clone(&fixture));
        fixture.fail_reload.store(true, SeqCst);
        assert!(store.prepare_thread(directory.path()).await.is_err());
        fixture.fail_reload.store(false, SeqCst);
        let draft = store.prepare_thread(directory.path()).await.unwrap();
        assert!(Arc::ptr_eq(
            &draft,
            &store.start_thread(directory.path()).await.unwrap()
        ));
        assert_eq!(fixture.builds.load(SeqCst), 1);
    }

    #[tokio::test]
    async fn command_catalog_uses_live_generation_and_failed_reload_keeps_subscription() {
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let fixture = Arc::new(CommandFixture::default());
        let store =
            scripted_store_with_fixture(directory.path().join("agent"), Arc::clone(&fixture));
        let first = store.create(directory.path()).await.unwrap();
        let id = first.log().header().id;
        let mut live = store.subscribe(&id).unwrap();
        for _ in 0..3 {
            assert_eq!(
                store.command_catalog(&id).await.unwrap()[0].description,
                "generation 1"
            );
        }
        assert_eq!(fixture.builds.load(SeqCst), 1);
        assert!(!first.log().path().exists());
        assert_eq!(
            submit_receipt(&first.submit("/native notice").await.unwrap(), &first)["status"],
            "handled"
        );
        let event = live.subscription.events.recv().await.unwrap();
        assert!(matches!(
            event.event,
            pi_session::AgentSessionEvent::PluginNotice { .. }
        ));
        assert_eq!(fixture.calls.load(SeqCst), 1);
        first.submit("/native reload").await.unwrap();
        let next = observe_latest(&mut live).await;
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(
            store.command_catalog(&id).await.unwrap()[0].description,
            "generation 2"
        );
        fixture.fail_reload.store(true, SeqCst);
        assert!(next
            .submit("/native reload")
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture reload failed"));
        assert!(!live.changes.as_ref().unwrap().has_changed().unwrap());
        assert!(Arc::ptr_eq(&store.open(&id).await.unwrap(), &next));
        next.submit("/native notice").await.unwrap();
        assert!(matches!(
            live.subscription.events.recv().await.unwrap().event,
            pi_session::AgentSessionEvent::PluginNotice { .. }
        ));
        assert_eq!(
            store.command_catalog(&id).await.unwrap()[0].description,
            "generation 2"
        );
    }

    #[tokio::test]
    async fn replacement_observation_hydrates_latest_history_status_and_commands() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let first = store.create(directory.path()).await.unwrap();
        let id = first.log().header().id;
        let mut live = store.subscribe(&id).unwrap();
        first.submit("/native reload").await.unwrap();
        store
            .open(&id)
            .await
            .unwrap()
            .submit("/native reload-send")
            .await
            .unwrap();

        // Even if the UI has not consumed a change yet, it catches up to the
        // latest complete snapshot without needing intermediate generations.
        let current = observe_latest(&mut live).await;
        assert!(Arc::ptr_eq(&current, &store.open(&id).await.unwrap()));
        let thread = thread_from_subscription(&live);
        assert_eq!(thread["id"], id);
        assert_eq!(thread["status"]["type"], "idle");
        assert_eq!(thread["commands"][0]["description"], "generation 3");
        assert_eq!(thread["turns"].as_array().unwrap().len(), 1);
        assert!(thread.to_string().contains("after reload"));
        assert!(thread.to_string().contains("hello from pi-rs"));
        assert!(live.subscription.events.try_recv().is_err());
        assert!(!live.changes.as_ref().unwrap().has_changed().unwrap());
    }

    #[tokio::test]
    async fn navigation_hydrates_history_once_at_the_new_subscription_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let first = store.create(directory.path()).await.unwrap();
        first.submit("persisted branch").await.unwrap();
        let id = first.log().header().id;
        let mut live = store.subscribe(&id).unwrap();
        let handle = store.handle(&id).unwrap().1;
        handle
            .new_session(directory.path(), directory.path().join("next.jsonl"))
            .await
            .unwrap();
        let current = handle.current();
        current.submit("immediate message").await.unwrap();
        observe_latest(&mut live).await;
        // Snapshot contains messages produced before the forwarder caught up.
        let hydrated = thread_from_subscription(&live);
        assert_ne!(hydrated["id"], id);
        assert_eq!(hydrated["turns"].as_array().unwrap().len(), 1);
        assert!(hydrated.to_string().contains("immediate message"));
        assert!(!hydrated.to_string().contains("persisted branch"));
        assert!(live.subscription.events.try_recv().is_err());

        // Messages after subscription are delivered only by the new event stream.
        current.submit("later message").await.unwrap();
        assert!(!thread_from_subscription(&live)
            .to_string()
            .contains("later message"));
        let mut users = 0;
        while let Ok(event) = live.subscription.events.try_recv() {
            assert!(event.revision > live.subscription.snapshot.revision);
            if matches!(event.event, pi_session::AgentSessionEvent::Agent(event)
                if matches!(*event, pi_core::AgentEvent::MessageStart { message: Message::User(_) }))
            {
                users += 1;
            }
        }
        assert_eq!(users, 1);

        handle.resume_session(first.log().path()).await.unwrap();
        handle.current().submit("after resume").await.unwrap();
        observe_latest(&mut live).await;
        let hydrated = thread_from_subscription(&live);
        assert_eq!(hydrated["id"], id);
        assert_eq!(hydrated["turns"].as_array().unwrap().len(), 2);
        assert!(hydrated.to_string().contains("persisted branch"));
        assert!(hydrated.to_string().contains("after resume"));
        assert!(live.subscription.events.try_recv().is_err());
    }

    #[tokio::test]
    async fn closing_managed_session_closes_watch_without_polling() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();
        session.submit("persist").await.unwrap();
        let id = session.log().header().id;
        let mut live = store.subscribe(&id).unwrap();
        store.archive(&id).await.unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            live.changes.as_mut().unwrap().changed(),
        )
        .await
        .unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn replacement_snapshot_recovers_a_response_that_started_before_subscription() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Arc::new(CommandFixture {
            wait_for_abort: true,
            ..CommandFixture::default()
        });
        let store = scripted_store_with_fixture(directory.path().join("agent"), fixture);
        let first = store.create(directory.path()).await.unwrap();
        let id = first.log().header().id;
        let mut live = store.subscribe(&id).unwrap();
        first.submit("/native reload").await.unwrap();
        let current = store.open(&id).await.unwrap();
        let mut events = current.subscribe();
        let run = tokio::spawn({
            let current = Arc::clone(&current);
            async move { current.submit("response before subscription").await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if matches!(events.events.recv().await.unwrap().event,
                    pi_session::AgentSessionEvent::Agent(event)
                    if matches!(*event, pi_core::AgentEvent::AgentStart))
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        observe_latest(&mut live).await;
        assert!(live.subscription.snapshot.agent.is_running);
        assert_eq!(thread_from_subscription(&live)["status"]["type"], "active");
        current.abort();
        tokio::time::timeout(std::time::Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // The forwarder refreshes from this boundary on AgentSettled.
        live.replace(current);
        let thread = thread_from_subscription(&live);
        assert_eq!(thread["status"]["type"], "idle");
        assert!(thread.to_string().contains("response before subscription"));
    }

    #[tokio::test]
    async fn busy_commands_dispatch_once_and_preserve_active_run() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Arc::new(CommandFixture {
            wait_for_abort: true,
            ..CommandFixture::default()
        });
        let store =
            scripted_store_with_fixture(directory.path().join("agent"), Arc::clone(&fixture));
        let session = store.create(directory.path()).await.unwrap();
        let mut events = session.subscribe();
        let run = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.submit("start").await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if matches!(events.events.recv().await.unwrap().event,
                    pi_session::AgentSessionEvent::Agent(event) if matches!(*event, pi_core::AgentEvent::AgentStart)) { break; }
            }
        }).await.unwrap();
        let handled = session.submit("/native notice").await.unwrap();
        assert_eq!(submit_receipt(&handled, &session)["status"], "handled");
        assert_eq!(submit_receipt(&handled, &session)["isRunning"], true);
        let queued = session.submit("/native transform").await.unwrap();
        assert_eq!(submit_receipt(&queued, &session)["status"], "queued");
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        session.abort();
        tokio::time::timeout(std::time::Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn replacement_identity_never_routes_old_id_to_new_thread() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let first = store.create(directory.path()).await.unwrap();
        let old_id = first.log().header().id;
        let (key, handle) = store.handle(&old_id).unwrap();
        let mut live = store.subscribe(&old_id).unwrap();
        handle
            .new_session(directory.path(), directory.path().join("next.jsonl"))
            .await
            .unwrap();
        observe_latest(&mut live).await;
        let next = handle.current();
        let new_id = next.log().header().id;
        assert_ne!(old_id, new_id);
        assert_eq!(store.forwarder_key(&new_id).unwrap(), key);
        assert!(store.open(&old_id).await.is_err());
        assert!(Arc::ptr_eq(&store.open(&new_id).await.unwrap(), &next));
    }

    #[tokio::test]
    async fn fork_and_resume_keep_distinct_store_and_forwarder_identities() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let first = store.create(directory.path()).await.unwrap();
        let mut subscription = first.subscribe();
        first.submit("persist").await.unwrap();
        wait_for_standard_message_entries(&mut subscription, 2).await;
        let old_id = first.log().header().id;
        let original_key = store.forwarder_key(&old_id).unwrap();
        let entry_id = first
            .log()
            .find_entries_on_branch(&BranchQuery {
                entries: EntryQuery {
                    order: EntryOrder::OldestFirst,
                    ..EntryQuery::default()
                },
                ..BranchQuery::default()
            })
            .unwrap()
            .into_iter()
            .find(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Message(entry)
                        if matches!(entry.message.as_standard(), Some(Message::User(_)))
                )
            })
            .unwrap()
            .id;
        let fork = store.fork(&old_id, entry_id.clone()).await.unwrap();
        let fork_id = fork.log().header().id;
        assert_eq!(fork.log().leaf_id().as_deref(), Some(entry_id.as_str()));
        assert_eq!(store.forwarder_key(&fork_id).unwrap(), original_key);
        let (left, right) = tokio::join!(store.open(&old_id), store.open(&old_id));
        assert!(Arc::ptr_eq(&left.unwrap(), &right.unwrap()));
        assert_ne!(store.forwarder_key(&old_id).unwrap(), original_key);
        assert_eq!(
            store.open(&fork_id).await.unwrap().log().header().id,
            fork_id
        );
    }

    #[tokio::test]
    async fn transformed_command_runs_once_and_receipt_is_not_in_progress() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Arc::new(CommandFixture::default());
        let store =
            scripted_store_with_fixture(directory.path().join("agent"), Arc::clone(&fixture));
        let session = store.create(directory.path()).await.unwrap();
        let outcome = session.submit("/native transform").await.unwrap();
        let receipt = submit_receipt(&outcome, &session);
        assert_eq!(receipt["status"], "completed");
        assert_eq!(receipt["isRunning"], false);
        assert!(receipt.get("turn").is_none());
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(session.snapshot().agent.messages.len(), 2);
        assert!(session.submit("/native error").await.is_err());
        let queued = SubmitOutcome::Queued {
            kind: pi_session::QueueKind::Steer,
            entry_id: "entry".into(),
        };
        assert_eq!(submit_receipt(&queued, &session)["status"], "queued");
    }

    #[test]
    fn desktop_image_data_url_becomes_image_content() {
        let image = image_content_from_data_url("data:image/png;base64,aW1hZ2U=").unwrap();

        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.data, "aW1hZ2U=");
    }

    #[test]
    fn desktop_image_data_url_rejects_non_image_or_invalid_payloads() {
        assert!(image_content_from_data_url("https://example.com/image.png").is_err());
        assert!(image_content_from_data_url("data:text/plain;base64,aW1hZ2U=").is_err());
        assert!(image_content_from_data_url("data:image/png;base64,not-base64").is_err());
        assert!(image_content_from_data_url("data:image/png;base64,").is_err());
    }

    #[test]
    fn desktop_model_selection_preserves_provider_identity() {
        let models = [
            ModelSpec::new("first", "shared", "First", "test"),
            ModelSpec::new("second", "shared", "Second", "test"),
        ];

        let current = select_model(&models, &ProviderId::new("second"), "shared").unwrap();
        let explicit = select_model(&models, &ProviderId::new("second"), "first/shared").unwrap();

        assert_eq!(current.provider.as_str(), "second");
        assert_eq!(explicit.provider.as_str(), "first");
    }

    #[test]
    fn historical_subagent_results_project_a_selectable_child_link() {
        let item = historical_tool_item(
            "call-1",
            "subagent",
            json!({"agent": "reviewer", "task": "Review the parser"}),
            "review complete",
            false,
            Some(&json!({
                "agent": "reviewer",
                "sessionId": "child-session",
                "state": "completed",
                "usage": { "totalTokens": 321 },
            })),
            HistoricalToolContext {
                cwd: Path::new("/workspace"),
                parent_thread_id: "parent-session",
            },
        );

        assert_eq!(item["type"], "collabToolCall");
        assert_eq!(item["senderThreadId"], "parent-session");
        assert_eq!(item["newThreadId"], "child-session");
        assert_eq!(item["newAgentRole"], "reviewer");
        assert_eq!(item["agentStatuses"][0]["status"], "completed");
        assert_eq!(item["agentStatuses"][0]["totalTokens"], 321);
    }

    #[test]
    fn desktop_thinking_defaults_follow_model_capabilities() {
        let plain = ModelSpec::new("provider", "plain", "Plain", "test");
        let mut reasoning = ModelSpec::new("provider", "reasoning", "Reasoning", "test");
        reasoning.reasoning = true;

        assert_eq!(
            default_thinking_level(&plain, ThinkingLevel::High),
            Some(ThinkingLevel::Off)
        );
        assert_eq!(
            default_thinking_level(&reasoning, ThinkingLevel::High),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            default_thinking_level(&reasoning, ThinkingLevel::Max),
            Some(ThinkingLevel::High)
        );
    }

    #[tokio::test]
    async fn desktop_configuration_clamps_unsupported_thinking_like_pi() {
        struct Catalog;

        #[pi_core::provider_plugin]
        impl pi_core::ProviderPlugin for Catalog {
            fn id(&self) -> pi_core::PluginId {
                pi_core::PluginId::new("desktop-thinking-catalog")
            }

            fn register(
                &self,
                context: &mut pi_core::ProviderRegisterContext<'_>,
            ) -> pi_core::Result<()> {
                let mut model =
                    ModelSpec::new("scripted", "desktop-reasoning", "Reasoning", "test");
                model.reasoning = true;
                context.register_model(model)
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin(Catalog)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("desktop-reasoning"),
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create(runtime, directory.path().join("session.jsonl"))
            .await
            .unwrap();

        configure_session(&session, None, Some("max")).unwrap();

        assert_eq!(
            session.runtime().agent().state().thinking_level,
            ThinkingLevel::High
        );
    }

    #[tokio::test]
    async fn unchanged_desktop_thinking_level_keeps_new_session_unsaved() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        configure_session(&session, None, Some("off")).unwrap();

        assert!(!session.log().path().exists());
    }

    #[tokio::test]
    async fn desktop_session_filename_ends_with_exposed_session_id() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();
        session
            .submit("materialize the desktop session")
            .await
            .unwrap();

        let path = session.log().path();
        let document = SessionLog::read(path).unwrap();
        let projected = thread_from_session(&session);
        let copied_id = projected["id"].as_str().unwrap();

        assert_eq!(copied_id, document.header.id);
        assert!(
            path.file_name().is_some_and(|name| name
                .to_string_lossy()
                .ends_with(&format!("_{copied_id}.jsonl"))),
            "desktop copy ID must match the JSONL filename suffix"
        );
    }

    #[tokio::test]
    async fn pi_session_projects_into_desktop_thread_shape() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();
        let mut subscription = session.subscribe();

        session.submit("hello desktop").await.unwrap();
        wait_for_standard_message_entries(&mut subscription, 2).await;
        let thread = thread_from_session(&session);

        assert_eq!(thread["source"], "pi-rs");
        assert_eq!(thread["preview"], "hello desktop");
        assert_eq!(thread["turns"][0]["items"][0]["type"], "userMessage");
        assert_eq!(thread["turns"][0]["items"][1]["type"], "agentMessage");
        assert_eq!(thread["turns"][0]["items"][1]["text"], "hello from pi-rs");
        assert!(thread["turns"][0]["items"][0]["entryId"].as_str().is_some());
        assert!(thread["turns"][0]["items"][1]["entryId"].as_str().is_some());
        assert_ne!(
            thread["turns"][0]["items"][0]["entryId"],
            thread["turns"][0]["items"][1]["entryId"]
        );
        assert!(thread["tokenUsage"]["totalTokens"].as_u64().is_some());
        assert!(thread["tokenUsage"]["contextTokens"].as_u64().is_some());

        let summaries = store.list(directory.path()).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title, "hello desktop");
    }

    #[tokio::test]
    async fn background_prompt_returns_text_without_leaving_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let pi = PiRuntimeState {
            store: scripted_store(directory.path().join("agent")),
            info: PiDesktopInfo {
                agent_dir: directory.path().join("agent"),
                provider: "scripted".to_string(),
                model: "desktop-test".to_string(),
                ready: true,
                reason: None,
            },
            forwarders: Arc::new(Mutex::new(HashSet::new())),
        };

        let response = run_background_prompt_in_cwd(
            &pi,
            directory.path(),
            "generate a commit message".to_string(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(response, "hello from pi-rs");
        assert!(pi.store.list(directory.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn stable_primary_keys_do_not_block_isolated_observation() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let parent = store.create(directory.path()).await.unwrap();
        let parent_id = parent.log().header().id;
        let (key, handle) = store.handle(&parent_id).unwrap();
        assert_ne!(key, parent_id);
        let child = handle
            .launch_isolated_session(pi_core::IsolatedSessionRequest::new(
                pi_core::CustomMessageContent::Text("child".into()),
            ))
            .await
            .unwrap();
        let (observation, live) = store
            .subscribe_isolated(&parent_id, child.as_str(), "worker")
            .unwrap();
        assert!(live.primary().is_none());
        assert!(live.changes.is_none());
        assert!(store.observed_isolated(&observation.session_id()).is_some());
        handle.wait_for_isolated_session(&child).await.unwrap();
        assert_eq!(live.token_usage().total_tokens, Some(0));
        assert!(live.token_usage().context_tokens.is_some());
    }

    #[tokio::test]
    async fn isolated_sessions_stay_on_disk_but_out_of_top_level_discovery() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        session.submit("parent").await.unwrap();
        let parent_path = session.log().path().to_path_buf();
        let child_path = parent_path
            .parent()
            .unwrap()
            .join(parent_path.file_stem().unwrap())
            .join("isolated")
            .join("child.jsonl");
        let child_id = "child-session";
        SessionLog::create(
            &child_path,
            SessionHeader::new(child_id, directory.path().to_path_buf()),
        )
        .unwrap();

        assert!(child_path.exists());
        let sessions = store.list(directory.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.log().header().id);
        let stored_child = store.read_isolated(child_id).unwrap();
        assert_eq!(stored_child.document.header.id, child_id);
        assert_eq!(stored_child.parent_thread_id, session.log().header().id);
        assert_eq!(stored_child.agent, "subagent");
        let child_thread = thread_from_stored_isolated(&stored_child).unwrap();
        assert_eq!(child_thread["id"], child_id);
        assert_eq!(child_thread["tokenUsage"]["totalTokens"], 0);
        assert_eq!(
            child_thread["source"]["subAgent"]["threadSpawn"]["parentThreadId"],
            session.log().header().id
        );
        assert!(store.open(child_id).await.is_err());
    }

    #[tokio::test]
    async fn deleting_parent_session_removes_its_isolated_session_directory() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        session.submit("parent to delete").await.unwrap();
        let parent_id = session.log().header().id.clone();
        let parent_path = session.log().path().to_path_buf();
        let isolated_dir = parent_path
            .parent()
            .unwrap()
            .join(parent_path.file_stem().unwrap())
            .join("isolated");
        SessionLog::create(
            isolated_dir.join("child.jsonl"),
            SessionHeader::new("child-to-delete", directory.path().to_path_buf()),
        )
        .unwrap();

        store.delete(&parent_id).await.unwrap();

        assert!(!parent_path.exists());
        assert!(!isolated_dir.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn pi_session_archive_moves_file_out_of_active_list() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        session.submit("archive me").await.unwrap();
        let session_id = session.log().header().id.clone();
        let session_path = session.log().path().to_path_buf();
        let sessions_path = directory.path().join("agent").join("sessions");
        let archived_path = sessions_path
            .join("archived")
            .join(session_path.strip_prefix(&sessions_path).unwrap());
        let companion = session_path
            .parent()
            .unwrap()
            .join(session_path.file_stem().unwrap());
        let child_path = companion.join("isolated/child.jsonl");
        SessionLog::create(
            &child_path,
            SessionHeader::new("archived-child", directory.path().to_path_buf()),
        )
        .unwrap();
        let archived_child = archived_path
            .parent()
            .unwrap()
            .join(archived_path.file_stem().unwrap())
            .join("isolated/child.jsonl");

        store.archive(&session_id).await.unwrap();

        assert!(!session_path.exists());
        assert!(!companion.exists());
        assert!(archived_path.exists());
        assert!(archived_child.exists());
        assert!(store.list(directory.path()).unwrap().is_empty());
        assert!(store.open(&session_id).await.is_err());
    }

    #[tokio::test]
    async fn pi_session_unarchive_moves_file_back_to_active_list() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        session.submit("restore me").await.unwrap();
        session
            .set_name(Some("Restored session title".to_string()))
            .await
            .unwrap();
        let session_id = session.log().header().id.clone();
        let session_path = session.log().path().to_path_buf();
        let sessions_path = directory.path().join("agent").join("sessions");
        let archived_path = sessions_path
            .join("archived")
            .join(session_path.strip_prefix(&sessions_path).unwrap());
        let companion = session_path
            .parent()
            .unwrap()
            .join(session_path.file_stem().unwrap());
        let child_path = companion.join("isolated/child.jsonl");
        SessionLog::create(
            &child_path,
            SessionHeader::new("restored-child", directory.path().to_path_buf()),
        )
        .unwrap();
        let archived_companion = archived_path
            .parent()
            .unwrap()
            .join(archived_path.file_stem().unwrap());

        store.archive(&session_id).await.unwrap();
        assert!(store.list(directory.path()).unwrap().is_empty());
        assert_eq!(store.list_archived(directory.path()).unwrap().len(), 1);

        store.unarchive(&session_id).await.unwrap();

        assert!(session_path.exists());
        assert!(child_path.exists());
        assert!(!archived_path.exists());
        assert!(!archived_companion.exists());
        let restored = store.list(directory.path()).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].title, "Restored session title");
        assert_eq!(restored[0].message_count, 2);
        assert_eq!(restored[0].model.as_deref(), Some("test"));
        assert!(store.list_archived(directory.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn pi_session_delete_removes_active_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        session.submit("delete me").await.unwrap();
        let session_id = session.log().header().id.clone();
        let session_path = session.log().path().to_path_buf();

        store.delete(&session_id).await.unwrap();

        assert!(!session_path.exists());
        assert!(store.list(directory.path()).unwrap().is_empty());
        assert!(store.open(&session_id).await.is_err());
    }

    #[tokio::test]
    async fn pi_session_delete_removes_archived_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();

        session.submit("delete archived").await.unwrap();
        let session_id = session.log().header().id.clone();
        let session_path = session.log().path().to_path_buf();
        let archived_path = directory
            .path()
            .join("agent")
            .join("sessions")
            .join("archived")
            .join(session_path.file_name().unwrap());

        store.archive(&session_id).await.unwrap();
        store.delete(&session_id).await.unwrap();

        assert!(!session_path.exists());
        assert!(!archived_path.exists());
        assert!(store.list(directory.path()).unwrap().is_empty());
    }
}
