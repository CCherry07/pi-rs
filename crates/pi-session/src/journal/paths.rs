use std::path::{Path, PathBuf};

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
}
