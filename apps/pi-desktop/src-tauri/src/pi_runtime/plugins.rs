//! Native package Adapter: listing never constructs a session or loads native code.
use pi_sdk::plugins::{PluginLibrary, PluginLibrarySnapshot, PluginOperation};
use serde::Serialize;
use tauri::State;

use super::{workspace_path, PiRuntimeState};
use crate::state::AppState;

async fn library(
    workspace_id: Option<String>,
    state: &AppState,
    pi: &PiRuntimeState,
) -> Result<PluginLibrary, String> {
    let cwd = match workspace_id {
        Some(id) => Some(workspace_path(state, &id).await?),
        None => None,
    };
    Ok(PluginLibrary::new(
        pi.info.agent_dir.clone(),
        cwd,
        pi.project_trust.clone(),
    ))
}

#[tauri::command]
pub(crate) async fn pi_plugins_read(
    workspace_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<PluginLibrarySnapshot, String> {
    library(workspace_id, &state, &pi).await?.read().await
}

#[tauri::command]
pub(crate) async fn pi_plugins_operation(
    workspace_id: Option<String>,
    operation: PluginOperation,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<PluginLibrarySnapshot, String> {
    library(workspace_id, &state, &pi)
        .await?
        .operate(operation)
        .await
}

/// ID membership only. Neither installation scope, loaded version/hash nor freshness
/// can be inferred from this generation-local inventory. Never join these IDs to a
/// scope's lock and label that package/version "loaded".
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginRuntimeSnapshot {
    workspace_id: String,
    thread_id: String,
    configured_native_plugin_ids: Vec<String>,
}

#[tauri::command]
pub(crate) async fn pi_plugins_runtime(
    workspace_id: String,
    thread_id: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<PluginRuntimeSnapshot, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    let store = pi.store.clone();
    tokio::task::spawn_blocking(move || {
        let ids = store.existing_native_plugin_ids(&cwd, &thread_id)?;
        Ok(PluginRuntimeSnapshot {
            workspace_id,
            thread_id,
            configured_native_plugin_ids: ids,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_wire_contract_does_not_claim_package_scope_or_freshness() {
        let value = serde_json::to_value(PluginRuntimeSnapshot {
            workspace_id: "project".into(),
            thread_id: "thread".into(),
            configured_native_plugin_ids: vec!["example".into()],
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "workspaceId": "project", "threadId": "thread",
                "configuredNativePluginIds": ["example"],
            })
        );
    }
}
