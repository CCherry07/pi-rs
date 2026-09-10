//! Thin Desktop MCP configuration adapter. No sessions are constructed here.
use pi_sdk::mcp::{McpDocument, McpLibrary, McpScope};
use tauri::State;

use super::{workspace_path, PiRuntimeState};
use crate::state::AppState;

async fn library(
    workspace_id: Option<String>,
    state: &AppState,
    pi: &PiRuntimeState,
) -> Result<McpLibrary, String> {
    let cwd = match workspace_id {
        Some(id) => Some(workspace_path(state, &id).await?),
        None => None,
    };
    let agent_dir = pi.info.agent_dir.clone();
    let trust = pi.project_trust.clone();
    tokio::task::spawn_blocking(move || McpLibrary::for_desktop(&agent_dir, cwd.as_deref(), &trust))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn pi_mcp_read(
    workspace_id: Option<String>,
    scope: McpScope,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<McpDocument, String> {
    let library = library(workspace_id, &state, &pi).await?;
    tokio::task::spawn_blocking(move || library.read(scope))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn pi_mcp_save(
    workspace_id: Option<String>,
    scope: McpScope,
    revision: String,
    content: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<McpDocument, String> {
    let library = library(workspace_id, &state, &pi).await?;
    tokio::task::spawn_blocking(move || library.save(scope, &revision, &content))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn pi_mcp_test(
    workspace_id: Option<String>,
    name: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Vec<String>, String> {
    let library = library(workspace_id, &state, &pi).await?;
    library.test(&name).await
}
