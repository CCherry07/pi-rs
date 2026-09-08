use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const FILE: &str = "curator.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Active,
    Stale,
}

/// New-format metadata only. Missing/invalid metadata never grants write authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Metadata {
    pub schema_version: u32,
    pub curator_managed: bool,
    pub created_by: String,
    pub pinned: bool,
    pub state: State,
    pub created_at: i64,
    pub last_activity_at: i64,
    pub use_count: u64,
    pub view_count: u64,
    pub patch_count: u64,
    pub files: BTreeMap<String, String>,
}

impl Metadata {
    pub fn new(directory: &Path, managed: bool, creator: &str, now: i64) -> io::Result<Self> {
        Ok(Self {
            schema_version: 1,
            curator_managed: managed,
            created_by: creator.into(),
            pinned: false,
            state: State::Active,
            created_at: now,
            last_activity_at: now,
            use_count: 0,
            view_count: 0,
            patch_count: 0,
            files: fingerprint(directory)?,
        })
    }

    pub fn read(directory: &Path) -> io::Result<Self> {
        check_path(&directory.join(FILE))?;
        let value: Self = serde_json::from_slice(&fs::read(directory.join(FILE))?)?;
        if value.schema_version != 1 || value.created_at < 0 || value.last_activity_at < 0 {
            return Err(io::Error::other("Unsupported or invalid Skill metadata"));
        }
        Ok(value)
    }

    pub fn unchanged(&self, directory: &Path) -> bool {
        fingerprint(directory).is_ok_and(|files| files == self.files)
    }

    pub fn writable(&self, directory: &Path) -> bool {
        self.curator_managed && !self.pinned && self.unchanged(directory)
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        atomic_json(&directory.join(FILE), self)
    }
}

pub(crate) fn check_path(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::other(
                    "Symlinked curator paths are not supported",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Owned storage always ends in /skills. Ancestors above that root
        // may legitimately be aliases (macOS /var, a configured home, etc.).
        if ancestor.file_name().is_some_and(|part| part == "skills") {
            break;
        }
    }
    Ok(())
}

pub(crate) fn name(value: &str) -> io::Result<&str> {
    if value.is_empty()
        || value.starts_with('.')
        || value.contains(['/', '\\'])
        || Path::new(value)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::other(
            "Expected a single Skill name or archive ID",
        ));
    }
    Ok(value)
}

pub(crate) fn fingerprint(directory: &Path) -> io::Result<BTreeMap<String, String>> {
    check_path(directory)?;
    let mut files = BTreeMap::new();
    for entry in walkdir::WalkDir::new(directory).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink()
            || (!entry.file_type().is_dir() && !entry.file_type().is_file())
        {
            return Err(io::Error::other(
                "Skill packages must contain regular files and directories",
            ));
        }
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(directory)
                .map_err(io::Error::other)?;
            if relative == Path::new(FILE) {
                continue;
            }
            files.insert(
                relative.to_string_lossy().replace('\\', "/"),
                format!("{:x}", Sha256::digest(fs::read(entry.path())?)),
            );
        }
    }
    if !files.contains_key("SKILL.md") {
        return Err(io::Error::other("Skill package has no SKILL.md"));
    }
    Ok(files)
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    check_path(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Lock order: content gate, then the existing skills store lock. Never hold
/// either across a provider call. The separate run lock spans one maintenance run.
pub(crate) fn lock(root: &Path, file: &str, wait: bool) -> io::Result<File> {
    let path = root.join(file);
    check_path(&path)?;
    fs::create_dir_all(root)?;
    let handle = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if wait {
        FileExt::lock_exclusive(&handle)?;
    } else {
        FileExt::try_lock_exclusive(&handle)?;
    }
    Ok(handle)
}

pub(crate) fn content_lock(root: &Path) -> io::Result<File> {
    lock(root, ".curator-content.lock", true)
}

/// A process-held lease, rather than an expiring timestamp, protects long user
/// requests in other sessions. Crashed processes automatically release the lock.
pub(crate) struct ActivityLease {
    file: Option<File>,
    path: std::path::PathBuf,
}

impl ActivityLease {
    pub fn acquire(root: &Path) -> io::Result<Self> {
        let directory = root.join(".curator-active");
        let id = uuid::Uuid::new_v4().to_string();
        let file = lock(&directory, &id, false)?;
        Ok(Self {
            file: Some(file),
            path: directory.join(id),
        })
    }
}

impl Drop for ActivityLease {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn foreground_active(root: &Path) -> io::Result<bool> {
    let directory = root.join(".curator-active");
    check_path(&directory)?;
    if !directory.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        check_path(&path)?;
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {} // A stale lease from a crashed process is inactive.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(true),
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}
