use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    Curator,
    metadata::{self, Metadata, State, atomic_json, content_lock, fingerprint},
};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Archived {
    pub id: String,
    pub name: String,
    pub reason: String,
    pub absorbed_into: Option<String>,
    pub files: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct Backup {
    names: BTreeSet<String>,
    after: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

fn id() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3f"),
        uuid::Uuid::new_v4()
    )
}

fn copy_package(source: &Path, target: &Path) -> io::Result<()> {
    let before = fingerprint(source)?;
    metadata::check_path(target)?;
    fs::create_dir(target)?;
    for entry in walkdir::WalkDir::new(source)
        .min_depth(1)
        .follow_links(false)
    {
        let entry = entry?;
        let path = target.join(
            entry
                .path()
                .strip_prefix(source)
                .map_err(io::Error::other)?,
        );
        if entry.file_type().is_dir() {
            fs::create_dir(&path)?;
        } else if entry.file_type().is_file() {
            fs::copy(entry.path(), &path)?;
        } else {
            return Err(io::Error::other(
                "Cannot snapshot a symlink or special file",
            ));
        }
    }
    if fingerprint(source)? != before || fingerprint(target)? != before {
        return Err(io::Error::other("Skill changed while taking its snapshot"));
    }
    Ok(())
}

impl Curator {
    /// Caller holds the content lock. General skill mutations and command
    /// storage operations use the same skill-store lock around publication.
    pub fn archive_locked(
        &self,
        name: &str,
        reason: &str,
        absorbed_into: Option<&str>,
    ) -> io::Result<String> {
        let source = self.root.join(metadata::name(name)?);
        let meta = Metadata::read(&source)?;
        if !meta.writable(&source) {
            return Err(io::Error::other("Skill is protected or externally changed"));
        }
        let id = id();
        let directory = self.root.join(".archive").join(&id);
        metadata::check_path(&directory)?;
        fs::create_dir_all(&directory)?;
        atomic_json(
            &directory.join("entry.json"),
            &Archived {
                id: id.clone(),
                name: name.into(),
                reason: reason.into(),
                absorbed_into: absorbed_into.map(str::to_string),
                files: fingerprint(&source)?,
            },
        )?;
        fs::rename(source, directory.join("skill"))?;
        Ok(id)
    }

    pub fn archive(&self, name: &str) -> io::Result<String> {
        let _content = content_lock(&self.root)?;
        let _store = metadata::lock(&self.root, ".skill-store.lock", true)?;
        self.archive_locked(name, "manual", None)
    }

    pub fn archives(&self) -> io::Result<Vec<Archived>> {
        let root = self.root.join(".archive");
        metadata::check_path(&root)?;
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for entry in fs::read_dir(root)? {
            let directory = entry?.path();
            metadata::check_path(&directory)?;
            if directory.join("skill").is_dir() {
                result.push(serde_json::from_slice::<Archived>(&fs::read(
                    directory.join("entry.json"),
                )?)?);
            }
        }
        result.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(result)
    }

    pub fn restore(&self, archive_id: &str) -> io::Result<String> {
        let _content = content_lock(&self.root)?;
        let _store = metadata::lock(&self.root, ".skill-store.lock", true)?;
        let archive = self.root.join(".archive").join(metadata::name(archive_id)?);
        metadata::check_path(&archive)?;
        let receipt: Archived = serde_json::from_slice(&fs::read(archive.join("entry.json"))?)?;
        let destination = self.root.join(metadata::name(&receipt.name)?);
        metadata::check_path(&destination)?;
        if destination.exists() {
            return Err(io::Error::other(
                "Restore target already exists; nothing was overwritten",
            ));
        }
        let source = archive.join("skill");
        if fingerprint(&source)? != receipt.files {
            return Err(io::Error::other("Archived package changed"));
        }
        let mut m = Metadata::read(&source)?;
        m.last_activity_at = chrono::Utc::now().timestamp_millis();
        m.state = State::Active;
        m.save(&source)?;
        fs::rename(source, destination)?;
        Ok(receipt.name)
    }

    pub fn backup_locked(&self) -> io::Result<String> {
        let id = id();
        let root = self.root.join(".backups").join(&id);
        metadata::check_path(&root)?;
        fs::create_dir_all(root.join("skills"))?;
        let mut names = BTreeSet::new();
        for row in self.rows()? {
            if row.protected_reason.is_none() {
                copy_package(
                    &self.root.join(&row.name),
                    &root.join("skills").join(&row.name),
                )?;
                names.insert(row.name);
            }
        }
        // Completion marker is published last; partial snapshots are not selectable.
        atomic_json(&root.join("backup.json"), &Backup { names, after: None })?;
        Ok(id)
    }

    pub fn backup(&self) -> io::Result<String> {
        let _content = content_lock(&self.root)?;
        let _store = metadata::lock(&self.root, ".skill-store.lock", true)?;
        self.backup_locked()
    }

    pub fn seal_backup(&self, id: &str, created: &[String]) -> io::Result<()> {
        let _content = content_lock(&self.root)?;
        let path = self
            .root
            .join(".backups")
            .join(metadata::name(id)?)
            .join("backup.json");
        metadata::check_path(&path)?;
        let mut backup: Backup = serde_json::from_slice(&fs::read(&path)?)?;
        let mut after = BTreeMap::new();
        for row in self.rows()? {
            if row.metadata.is_some_and(|m| m.curator_managed)
                && (backup.names.contains(&row.name) || created.contains(&row.name))
            {
                after.insert(row.name.clone(), fingerprint(&self.root.join(&row.name))?);
            }
        }
        backup.after = Some(after);
        atomic_json(&path, &backup)
    }

    pub fn backups(&self) -> io::Result<Vec<String>> {
        let root = self.root.join(".backups");
        metadata::check_path(&root)?;
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.path().join("backup.json").is_file() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        ids.reverse();
        Ok(ids)
    }

    /// Restore managed packages only. Validate every target before changing any;
    /// preserve replaced packages as archives and take a pre-rollback snapshot.
    pub fn rollback(&self, requested: Option<&str>) -> io::Result<String> {
        let _content = content_lock(&self.root)?;
        let _store = metadata::lock(&self.root, ".skill-store.lock", true)?;
        let id = match requested {
            Some(id) => metadata::name(id)?.to_string(),
            None => self
                .backups()?
                .into_iter()
                .next()
                .ok_or_else(|| io::Error::other("No backups"))?,
        };
        let snapshot = self.root.join(".backups").join(&id);
        metadata::check_path(&snapshot)?;
        let backup: Backup = serde_json::from_slice(&fs::read(snapshot.join("backup.json"))?)?;
        let mut names = backup.names.clone();
        if let Some(after) = &backup.after {
            names.extend(after.keys().cloned());
        }
        for name in &names {
            metadata::name(name)?;
            let current = self.root.join(name);
            metadata::check_path(&current)?;
            if current.exists() {
                let m = Metadata::read(&current)?;
                if !m.writable(&current) {
                    return Err(io::Error::other(format!(
                        "Rollback conflict: {name} is protected or changed"
                    )));
                }
                if let Some(after) = &backup.after
                    && after.get(name) != Some(&fingerprint(&current)?)
                {
                    return Err(io::Error::other(format!(
                        "Rollback conflict: {name} changed after this run"
                    )));
                }
            }
            if backup.names.contains(name) {
                let original = snapshot.join("skills").join(name);
                let m = Metadata::read(&original)?;
                if !m.curator_managed || !m.unchanged(&original) {
                    return Err(io::Error::other("Invalid backup package"));
                }
            }
        }
        let safeguard = self.backup_locked()?;
        for name in names {
            let current = self.root.join(&name);
            // Prepare replacement before moving the existing package.
            let staging = if backup.names.contains(&name) {
                let stage = self.root.join(format!(".restore-{}", uuid::Uuid::new_v4()));
                copy_package(&snapshot.join("skills").join(&name), &stage)?;
                Some(stage)
            } else {
                None
            };
            if current.exists() {
                self.archive_locked(&name, "rollback replacement", None)?;
            }
            if let Some(staging) = staging {
                fs::rename(staging, current)?;
            }
        }
        Ok(format!(
            "Restored backup {id}. Pre-rollback backup: {safeguard}. Reload to refresh skills."
        ))
    }
}
