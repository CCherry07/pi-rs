use std::path::PathBuf;

use tokio::sync::Mutex;

use crate::storage::write_settings;
use crate::types::AppSettings;
use crate::utils::normalize_windows_namespace_path;

pub(crate) async fn get_app_settings_core(app_settings: &Mutex<AppSettings>) -> AppSettings {
    app_settings.lock().await.clone()
}

pub(crate) async fn update_app_settings_core(
    mut settings: AppSettings,
    app_settings: &Mutex<AppSettings>,
    settings_path: &PathBuf,
) -> Result<AppSettings, String> {
    settings.global_worktrees_folder = settings
        .global_worktrees_folder
        .map(|path| normalize_windows_namespace_path(&path));
    write_settings(settings_path, &settings)?;
    let mut current = app_settings.lock().await;
    *current = settings.clone();
    Ok(settings)
}
