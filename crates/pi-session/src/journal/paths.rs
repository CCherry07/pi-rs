use std::path::{Path, PathBuf};

/// Produces a unique temporary path next to a session file so the final
/// rename stays on the same filesystem.
pub(crate) fn sibling_transaction_path(path: &Path, purpose: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "session".into(), |name| name.to_os_string());
    name.push(format!(".{purpose}-{}.tmp", uuid::Uuid::now_v7()));
    path.with_file_name(name)
}

/// Normalizes existing paths and gives not-yet-created session files a stable
/// identity by canonicalizing their nearest existing parent.
pub(crate) fn comparable_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(file_name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(file_name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparable_path_canonicalizes_the_parent_of_a_missing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.jsonl");

        assert_eq!(
            comparable_path(&path),
            directory
                .path()
                .canonicalize()
                .unwrap()
                .join("missing.jsonl")
        );
    }

    #[test]
    fn transaction_paths_are_unique_siblings() {
        let path = Path::new("sessions/example.jsonl");
        let first = sibling_transaction_path(path, "import");
        let second = sibling_transaction_path(path, "import");

        assert_eq!(first.parent(), path.parent());
        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("example.jsonl.import-")
        );
        assert_eq!(
            first.extension().and_then(|value| value.to_str()),
            Some("tmp")
        );
    }
}
