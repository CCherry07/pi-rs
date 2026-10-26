use std::path::{Path, PathBuf};

use crate::utils::normalize_windows_namespace_path;

pub(crate) const WORKTREE_SETUP_MARKERS_DIR: &str = "worktree-setup";
pub(crate) const WORKTREE_SETUP_MARKER_EXT: &str = "ran";
pub(crate) fn normalize_setup_script(script: Option<String>) -> Option<String> {
    match script {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value),
        None => None,
    }
}

pub(crate) fn worktree_setup_marker_path(data_dir: &Path, workspace_id: &str) -> PathBuf {
    data_dir
        .join(WORKTREE_SETUP_MARKERS_DIR)
        .join(format!("{workspace_id}.{WORKTREE_SETUP_MARKER_EXT}"))
}

pub(crate) fn is_workspace_path_dir_core(path: &str) -> bool {
    normalize_workspace_path_input(path).is_dir()
}

pub(crate) fn normalize_workspace_path_input(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = crate::agent_paths::home_dir() {
            return home.join(rest);
        }
    }
    if trimmed == "~" {
        if let Some(home) = crate::agent_paths::home_dir() {
            return home;
        }
    }
    PathBuf::from(trimmed)
}

pub(crate) fn workspace_path_to_string(path: &Path) -> String {
    normalize_windows_namespace_path(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::{normalize_workspace_path_input, workspace_path_to_string};
    use std::path::PathBuf;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn normalize_workspace_path_input_expands_home_prefix() {
        let _guard = ENV_LOCK.lock().expect("lock env");
        let previous_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", "/tmp/cm-home");

        assert_eq!(
            normalize_workspace_path_input("~/dev/repo"),
            PathBuf::from("/tmp/cm-home/dev/repo")
        );

        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn workspace_path_to_string_strips_windows_namespace_prefixes() {
        assert_eq!(
            workspace_path_to_string(&PathBuf::from(r"\\?\I:\projects\sample-project")),
            r"I:\projects\sample-project"
        );
    }
}
