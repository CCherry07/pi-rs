use std::path::{Path, PathBuf};

/// Returns `path` unchanged when absolute, otherwise resolves it from the
/// process working directory. If the directory cannot be read, preserves the
/// historical fallback of resolving from `.`.
pub fn absolute_from_current_dir(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Renders a path with `/` separators for text formats that are platform-neutral.
pub fn slash_path(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{absolute_from_current_dir, slash_path};
    use std::path::Path;

    #[test]
    fn absolute_paths_are_preserved() {
        let path = if cfg!(windows) {
            Path::new(r"C:\workspace")
        } else {
            Path::new("/workspace")
        };
        assert_eq!(absolute_from_current_dir(path), path.to_path_buf());
    }

    #[test]
    fn slash_path_normalizes_backslashes() {
        assert_eq!(slash_path(r"dir\child"), "dir/child");
    }
}
