use std::path::PathBuf;
use tauri::State;

use self::io::TextFileResponse;
use self::policy::{FileKind, FileScope};
use crate::shared::files_core::{file_read_core, file_write_core};
use crate::state::AppState;

mod image;
pub(crate) mod io;
pub(crate) mod ops;
pub(crate) mod policy;

async fn file_read_impl(
    scope: FileScope,
    kind: FileKind,
    workspace_id: Option<String>,
    state: &AppState,
) -> Result<TextFileResponse, String> {
    file_read_core(&state.workspaces, scope, kind, workspace_id).await
}

async fn file_write_impl(
    scope: FileScope,
    kind: FileKind,
    workspace_id: Option<String>,
    content: String,
    state: &AppState,
) -> Result<(), String> {
    file_write_core(&state.workspaces, scope, kind, workspace_id, content).await
}

#[tauri::command]
pub(crate) async fn file_read(
    scope: FileScope,
    kind: FileKind,
    workspace_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<TextFileResponse, String> {
    file_read_impl(scope, kind, workspace_id, &state).await
}

#[tauri::command]
pub(crate) async fn file_write(
    scope: FileScope,
    kind: FileKind,
    workspace_id: Option<String>,
    content: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    file_write_impl(scope, kind, workspace_id, content, &state).await
}

#[tauri::command]
pub(crate) async fn read_image_as_data_url(path: String) -> Result<String, String> {
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return Err("Image path is required".to_string());
    }

    let normalized = image::normalize_path(trimmed_path);
    if normalized.is_empty() {
        return Err("Image path is required".to_string());
    }

    image::read_as_data_url(&normalized)
}

#[tauri::command]
pub(crate) fn write_text_file(path: String, content: String) -> Result<(), String> {
    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err("Path is required".to_string());
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create export directory: {err}"))?;
        }
    }
    std::fs::write(&target, content).map_err(|err| format!("Failed to write export file: {err}"))
}
