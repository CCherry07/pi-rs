use super::{workspace_path, PiRuntimeState};
use crate::state::AppState;
use pi_sdk::desktop_extensions::{read_catalog, DesktopExtensionCatalog};
use tauri::State;

#[tauri::command]
pub(crate) async fn pi_desktop_extensions(
    workspace_id: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<DesktopExtensionCatalog, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    let agent_dir = pi.info.agent_dir.clone();
    let trust = pi.project_trust.clone();
    tokio::task::spawn_blocking(move || read_catalog(&agent_dir, &cwd, &trust))
        .await
        .map_err(|error| error.to_string())
}
