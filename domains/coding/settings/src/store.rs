use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde_json::{Map, Value};

use crate::{SettingsError, document};

pub(crate) fn replace_top_level(
    path: &Path,
    key: &str,
    value: Value,
) -> Result<Map<String, Value>, SettingsError> {
    let parent = path
        .parent()
        .ok_or_else(|| SettingsError::InvalidDocument {
            path: path.to_path_buf(),
            message: "settings path has no parent directory".to_string(),
        })?;
    fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
        operation: "create settings directory for",
        path: parent.to_path_buf(),
        source,
    })?;
    let guard = FileGuard::acquire(&lock_path(path))?;
    let mut current = document::read(path)?;
    current.insert(key.to_string(), value);
    persist(path, &current)?;
    drop(guard);
    Ok(current)
}

fn persist(path: &Path, document: &Map<String, Value>) -> Result<(), SettingsError> {
    let parent = path.parent().expect("validated settings parent");
    let mut encoded =
        serde_json::to_vec_pretty(document).map_err(|error| SettingsError::Encode {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    encoded.push(b'\n');
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| SettingsError::Io {
            operation: "create temporary file for",
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(&encoded)
        .and_then(|_| temporary.flush())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|source| SettingsError::Io {
            operation: "write",
            path: path.to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| SettingsError::Io {
        operation: "replace",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_directory(parent, path)
}

fn sync_directory(directory: &Path, settings_path: &Path) -> Result<(), SettingsError> {
    #[cfg(unix)]
    {
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| SettingsError::Io {
                operation: "sync directory for",
                path: settings_path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "settings.json".into(), std::ffi::OsString::from);
    name.push(".lock");
    path.with_file_name(name)
}

struct FileGuard {
    file: File,
}

impl FileGuard {
    fn acquire(path: &Path) -> Result<Self, SettingsError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| SettingsError::Io {
                operation: "open lock for",
                path: path.to_path_buf(),
                source,
            })?;
        file.lock_exclusive().map_err(|source| SettingsError::Io {
            operation: "lock",
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self { file })
    }
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
