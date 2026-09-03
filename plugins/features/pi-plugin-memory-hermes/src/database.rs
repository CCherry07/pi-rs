//! Derived SQLite memory and session index.

use std::fs;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use pi_core::{ContentBlock, Message, SessionEntryKind, SessionSnapshot};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::config::{char_len, char_prefix, char_suffix};
use crate::store::{MemoryCategory, MemoryIndexRecord, MemoryTarget, StoreError};

const SCHEMA_VERSION: i64 = 4;
const MAX_MESSAGE_CONTENT_LENGTH: usize = 100 * 1024;
const RECOVERY_CIRCUIT_LIMIT: usize = 3;
const RECOVERY_CIRCUIT_WINDOW_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Default, Deserialize, Serialize)]
struct RecoveryCircuitState {
    #[serde(default)]
    failures: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemorySearchOptions {
    pub(crate) project: Option<Option<String>>,
    pub(crate) target: Option<MemoryTarget>,
    pub(crate) category: Option<MemoryCategory>,
    pub(crate) limit: usize,
}

impl Default for MemorySearchOptions {
    fn default() -> Self {
        Self {
            project: None,
            target: None,
            category: None,
            limit: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemorySearchHit {
    pub(crate) id: i64,
    pub(crate) project: Option<String>,
    pub(crate) target: MemoryTarget,
    pub(crate) category: Option<MemoryCategory>,
    pub(crate) content: String,
    pub(crate) failure_reason: Option<String>,
    pub(crate) tool_state: Option<String>,
    pub(crate) corrected_to: Option<String>,
    pub(crate) created: String,
    pub(crate) last_referenced: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSearchOptions {
    pub(crate) project: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) since: Option<String>,
    pub(crate) limit: usize,
    pub(crate) snippet_chars: usize,
}

impl Default for SessionSearchOptions {
    fn default() -> Self {
        Self {
            project: None,
            role: None,
            since: None,
            limit: 10,
            snippet_chars: 2_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSearchHit {
    pub(crate) session_id: String,
    pub(crate) project: String,
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) timestamp: String,
    pub(crate) snippet: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BulkIndexResult {
    pub(crate) sessions_processed: usize,
    pub(crate) sessions_indexed: usize,
    pub(crate) sessions_skipped: usize,
    pub(crate) messages_indexed: usize,
    pub(crate) errors: Vec<String>,
    pub(crate) reached_limit: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct MemoryMirrorSync {
    pub(crate) imported: usize,
    pub(crate) skipped: usize,
    pub(crate) removed: usize,
    pub(crate) mirrored_projects: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectSessionStats {
    pub(crate) project: String,
    pub(crate) sessions: usize,
    pub(crate) messages: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SessionStats {
    pub(crate) total_sessions: usize,
    pub(crate) total_messages: usize,
    pub(crate) projects: Vec<ProjectSessionStats>,
}

#[derive(Debug, Clone)]
pub(crate) struct Database {
    path: PathBuf,
    session_roots: Vec<PathBuf>,
}

impl Database {
    pub(crate) fn new(
        path: PathBuf,
        _agent_dir: PathBuf,
        _projects_dir: String,
        session_roots: Vec<PathBuf>,
    ) -> Result<Self, StoreError> {
        let database = Self {
            path: crate::store::canonical_storage_path(&path)?,
            session_roots,
        };
        database.open()?;
        Ok(database)
    }

    pub(crate) fn session_roots(&self) -> &[PathBuf] {
        &self.session_roots
    }

    fn open(&self) -> Result<Connection, StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        match self.open_once() {
            Ok(connection) => Ok(connection),
            Err(error) if database_is_corrupt(&error) && self.path.exists() => {
                self.recover_corrupt_database()?;
                self.open_once().map_err(StoreError::Database)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    fn open_once(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_millis(5_000))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=1000;
             PRAGMA journal_size_limit=5242880;
             PRAGMA foreign_keys=ON;",
        )?;
        initialize_schema(&connection)?;
        let check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                Some(format!("SQLite quick_check failed after open: {check}")),
            ));
        }
        Ok(connection)
    }

    fn recover_corrupt_database(&self) -> Result<(), StoreError> {
        let parent = self.path.parent().expect("database path has a parent");
        fs::create_dir_all(parent)?;
        let lock_path = parent.join(".sessions.db.recovery.lock");
        let lock = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "SQLite recovery already in progress for {}; timed out after 5000ms",
                                self.path.display()
                            ),
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
        let result = (|| {
            if self.open_once().is_ok() {
                self.clear_recovery_failures_best_effort();
                return Ok(());
            }
            self.assert_recovery_circuit_closed()?;
            let recovery = (|| {
                self.cleanup_database_recovery_artifacts();
                let backup_base = self.corrupt_backup_base();
                if self
                    .rebuild_database_from_readable_rows(&backup_base)
                    .is_err()
                {
                    self.move_database_files_to_backup(&backup_base)?;
                }
                self.open_once().map_err(StoreError::Database)?;
                self.cleanup_database_recovery_artifacts();
                Ok(())
            })();
            match recovery {
                Ok(()) => {
                    self.clear_recovery_failures_best_effort();
                    Ok(())
                }
                Err(error) => {
                    self.record_recovery_failure_best_effort();
                    Err(error)
                }
            }
        })();
        let unlock = lock.unlock();
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(StoreError::Io(error)),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn rebuild_database_from_readable_rows(&self, backup_base: &Path) -> Result<(), StoreError> {
        let temporary = self.path.with_file_name(format!(
            "{}.rebuild-{}-{}.tmp",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("sessions.db"),
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        remove_database_file_set(&temporary);
        let rebuilt = (|| -> Result<(), StoreError> {
            let source = Connection::open_with_flags(
                &self.path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let target = Connection::open(&temporary)?;
            target.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=OFF;")?;
            initialize_schema(&target)?;
            for (table, columns) in [
                ("extension_metadata", &["key", "value"][..]),
                (
                    "sessions",
                    &[
                        "id",
                        "project",
                        "cwd",
                        "started_at",
                        "ended_at",
                        "message_count",
                    ][..],
                ),
                (
                    "messages",
                    &[
                        "id",
                        "session_id",
                        "role",
                        "content",
                        "timestamp",
                        "tool_calls",
                    ][..],
                ),
                (
                    "session_files",
                    &["path", "session_id", "size", "mtime_ms", "indexed_at"][..],
                ),
                (
                    "memories",
                    &[
                        "id",
                        "project",
                        "target",
                        "category",
                        "content",
                        "failure_reason",
                        "tool_state",
                        "corrected_to",
                        "created",
                        "last_referenced",
                    ][..],
                ),
            ] {
                copy_readable_rows(&source, &target, table, columns)?;
            }
            target.execute_batch(
                "INSERT INTO message_fts(message_fts) VALUES('rebuild');
                 INSERT INTO memory_fts(memory_fts) VALUES('rebuild');",
            )?;
            let foreign_key_violations: i64 =
                target.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })?;
            if foreign_key_violations != 0 {
                return Err(StoreError::Io(std::io::Error::other(format!(
                    "SQLite foreign_key_check failed after rebuild ({foreign_key_violations} violations)"
                ))));
            }
            let check: String = target.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            if check != "ok" {
                return Err(StoreError::Io(std::io::Error::other(format!(
                    "SQLite quick_check failed after corruption rebuild: {check}"
                ))));
            }
            target
                .close()
                .map_err(|(_, error)| StoreError::Database(error))?;
            source
                .close()
                .map_err(|(_, error)| StoreError::Database(error))?;
            Ok(())
        })();
        if let Err(error) = rebuilt {
            remove_database_file_set(&temporary);
            return Err(error);
        }
        let moved = self.move_database_files_to_backup(backup_base)?;
        if let Err(error) = fs::rename(&temporary, &self.path) {
            for (original, backup) in moved.into_iter().rev() {
                if backup.exists() {
                    let _ = fs::remove_file(&original);
                    let _ = fs::rename(backup, original);
                }
            }
            remove_database_file_set(&temporary);
            return Err(StoreError::Io(error));
        }
        remove_database_file_set(&temporary);
        Ok(())
    }

    fn corrupt_backup_base(&self) -> PathBuf {
        PathBuf::from(format!(
            "{}.corrupt-{}-{}-{}",
            self.path.display(),
            Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ"),
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        ))
    }

    fn move_database_files_to_backup(
        &self,
        backup_base: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, StoreError> {
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let original = PathBuf::from(format!("{}{suffix}", self.path.display()));
            if !original.exists() {
                continue;
            }
            let backup = PathBuf::from(format!("{}{suffix}", backup_base.display()));
            let _ = fs::remove_file(&backup);
            if let Err(error) = fs::rename(&original, &backup) {
                for (original, backup) in moved.iter().rev() {
                    if backup.exists() {
                        let _ = fs::rename(backup, original);
                    }
                }
                return Err(StoreError::Io(error));
            }
            moved.push((original, backup));
        }
        Ok(moved)
    }

    fn cleanup_database_recovery_artifacts(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let Some(name) = self.path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        let mut backup_groups = std::collections::HashMap::<String, std::time::SystemTime>::new();
        for entry in entries.filter_map(Result::ok) {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with(&format!("{name}.rebuild-")) {
                let _ = fs::remove_file(entry.path());
                continue;
            }
            let Some(rest) = file_name.strip_prefix(&format!("{name}.corrupt-")) else {
                continue;
            };
            let group = rest
                .strip_suffix("-wal")
                .or_else(|| rest.strip_suffix("-shm"))
                .unwrap_or(rest);
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            backup_groups
                .entry(group.to_string())
                .and_modify(|current| *current = (*current).max(modified))
                .or_insert(modified);
        }
        let mut groups = backup_groups.into_iter().collect::<Vec<_>>();
        groups.sort_by_key(|item| std::cmp::Reverse(item.1));
        for (group, _) in groups.into_iter().skip(3) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = fs::remove_file(parent.join(format!("{name}.corrupt-{group}{suffix}")));
            }
        }
    }

    fn recovery_circuit_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.recovery-state.json", self.path.display()))
    }

    fn recent_recovery_failures(&self) -> Vec<i64> {
        let Ok(raw) = fs::read(self.recovery_circuit_path()) else {
            return Vec::new();
        };
        let Ok(state) = serde_json::from_slice::<RecoveryCircuitState>(&raw) else {
            return Vec::new();
        };
        let cutoff = Utc::now()
            .timestamp_millis()
            .saturating_sub(RECOVERY_CIRCUIT_WINDOW_MS);
        state
            .failures
            .into_iter()
            .filter(|timestamp| *timestamp >= cutoff)
            .collect()
    }

    fn assert_recovery_circuit_closed(&self) -> Result<(), StoreError> {
        if self.recent_recovery_failures().len() >= RECOVERY_CIRCUIT_LIMIT {
            return Err(StoreError::Io(std::io::Error::other(format!(
                "SQLite recovery circuit is open for {}: too many failed recovery attempts within {}ms",
                self.path.display(),
                RECOVERY_CIRCUIT_WINDOW_MS,
            ))));
        }
        Ok(())
    }

    fn record_recovery_failure_best_effort(&self) {
        let state_path = self.recovery_circuit_path();
        let temporary = PathBuf::from(format!(
            "{}.tmp-{}-{}",
            state_path.display(),
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..6],
        ));
        let mut failures = self.recent_recovery_failures();
        failures.push(Utc::now().timestamp_millis());
        let result = (|| -> Result<(), std::io::Error> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            serde_json::to_writer(&mut file, &RecoveryCircuitState { failures })
                .map_err(std::io::Error::other)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, state_path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
    }

    fn clear_recovery_failures_best_effort(&self) {
        match fs::remove_file(self.recovery_circuit_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }

    pub(crate) fn sync_memories(
        &self,
        records: &[MemoryIndexRecord],
    ) -> Result<MemoryMirrorSync, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let mirrored_projects = {
            let mut statement = transaction.prepare(
                "SELECT DISTINCT project FROM memories WHERE project IS NOT NULL AND target='memory' ORDER BY project",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut sync = MemoryMirrorSync {
            mirrored_projects,
            ..MemoryMirrorSync::default()
        };
        let mut desired = std::collections::HashSet::new();
        for record in records {
            let target = record.target.index_target();
            let category = record.category.map(MemoryCategory::as_str);
            let content = record.content.trim();
            let identity = (
                record.project.clone(),
                target.to_string(),
                category.map(str::to_string),
                content.to_string(),
            );
            desired.insert(identity);
            let existing = transaction
                .query_row(
                    "SELECT id,created,last_referenced,category,failure_reason,tool_state,corrected_to
                     FROM memories
                     WHERE target=?1
                       AND ((project IS NULL AND ?2 IS NULL) OR project=?2)
                       AND ((category IS NULL AND ?3 IS NULL) OR category=?3)
                       AND content=?4
                     ORDER BY id ASC LIMIT 1",
                    params![target, record.project, category, content],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((id, created, last_referenced, old_category, reason, state, corrected)) =
                existing
            {
                sync.skipped = sync.skipped.saturating_add(1);
                transaction.execute(
                    "UPDATE memories SET category=?1,failure_reason=?2,tool_state=?3,corrected_to=?4,created=?5,last_referenced=?6 WHERE id=?7",
                    params![
                        old_category.or_else(|| category.map(str::to_string)),
                        reason.or_else(|| record.failure_reason.clone()),
                        state.or_else(|| record.tool_state.clone()),
                        corrected.or_else(|| record.corrected_to.clone()),
                        created.min(record.created.clone()),
                        last_referenced.max(record.last_referenced.clone()),
                        id,
                    ],
                )?;
            } else {
                sync.imported = sync.imported.saturating_add(1);
                transaction.execute(
                    "INSERT INTO memories(project,target,category,content,failure_reason,tool_state,corrected_to,created,last_referenced) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        record.project,
                        target,
                        category,
                        content,
                        record.failure_reason,
                        record.tool_state,
                        record.corrected_to,
                        record.created,
                        record.last_referenced,
                    ],
                )?;
            }
        }
        let rows = {
            let mut statement = transaction.prepare(
                "SELECT id,project,target,category,content FROM memories ORDER BY id ASC",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut retained = std::collections::HashSet::new();
        for (id, project, target, category, content) in rows {
            let identity = (project, target, category, content.trim().to_string());
            if !desired.contains(&identity) || !retained.insert(identity) {
                sync.removed = sync.removed.saturating_add(
                    transaction.execute("DELETE FROM memories WHERE id=?1", params![id])?,
                );
            }
        }
        transaction.commit()?;
        Ok(sync)
    }

    pub(crate) fn search_memories(
        &self,
        query: &str,
        options: &MemorySearchOptions,
    ) -> Result<Vec<MemorySearchHit>, StoreError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.open()?;
        let normalized = normalize_fts_query(query, false);
        let mut parse_failed = false;
        let exact = match run_memory_search(&connection, &normalized, options) {
            Ok(rows) => rows,
            Err(error) if is_fts_error(&error) => {
                parse_failed = true;
                Vec::new()
            }
            Err(error) => return Err(error.into()),
        };
        if !exact.is_empty() {
            return Ok(exact);
        }
        if is_short_cjk_literal(query) {
            return like_memory_search(&connection, &[query.trim().to_string()], options)
                .map_err(Into::into);
        }
        if parse_failed {
            let natural = normalize_fts_query(query, true);
            if natural.is_empty() || natural == normalized {
                return Ok(Vec::new());
            }
            let terms = collect_terms(query);
            let rows = run_memory_search(&connection, &natural, options).unwrap_or_default();
            if !rows.is_empty() {
                return Ok(rows);
            }
            if terms.len() > 1 {
                let fallback = quote_terms(&terms, " OR ");
                if fallback != natural {
                    return run_memory_search(&connection, &fallback, options).map_err(Into::into);
                }
            }
            return Ok(rows);
        }
        if has_explicit_operator(query) {
            return Ok(exact);
        }
        let terms = collect_terms(query);
        if terms.len() > 1 {
            let fallback = quote_terms(&terms, " OR ");
            if let Ok(rows) = run_memory_search(&connection, &fallback, options)
                && !rows.is_empty()
            {
                return Ok(rows);
            }
        }
        Ok(exact)
    }

    pub(crate) fn indexed_memory_count(&self) -> Result<usize, StoreError> {
        let connection = self.open()?;
        let count = connection.query_row("SELECT COUNT(*) FROM memories", [], |row| {
            row.get::<_, i64>(0)
        })?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub(crate) fn index_snapshot(&self, snapshot: &SessionSnapshot) -> Result<usize, StoreError> {
        let Some(file) = snapshot.file() else {
            return Ok(0);
        };
        if is_ephemeral_hermes_session(file) {
            return Ok(0);
        }
        let project = project_from_cwd(snapshot.cwd());
        let rows = snapshot
            .entries()
            .iter()
            .filter(|entry| matches!(entry.kind(), SessionEntryKind::Message))
            .filter_map(|entry| {
                let message =
                    serde_json::from_value::<Message>(entry.raw().get("message")?.clone()).ok()?;
                indexed_message(
                    entry.id().to_string(),
                    entry
                        .raw()
                        .get("timestamp")
                        .and_then(timestamp_value)
                        .unwrap_or_else(|| iso(0)),
                    &message,
                )
            })
            .collect::<Vec<_>>();
        let result = self.replace_session(
            snapshot.id(),
            &project,
            snapshot.cwd(),
            snapshot
                .raw_header()
                .get("createdAt")
                .and_then(timestamp_value)
                .unwrap_or_else(|| iso(0)),
            None,
            &rows,
        )?;
        let metadata = fs::metadata(file)?;
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or(0);
        self.upsert_session_file(file, snapshot.id(), metadata.len(), modified_ms)?;
        Ok(result.0)
    }

    pub(crate) fn backfill_sessions(
        &self,
        max_files_to_index: Option<usize>,
    ) -> Result<BulkIndexResult, StoreError> {
        let mut result = BulkIndexResult::default();
        let connection = self.open()?;
        let mut changed = Vec::new();
        for root in &self.session_roots {
            if !root.exists() {
                continue;
            }
            for entry in WalkDir::new(root)
                .max_depth(2)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
                })
            {
                let path = entry.path();
                let metadata = match fs::metadata(path) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        result.errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                };
                let modified_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_millis() as i64);
                let existing = connection
                    .query_row(
                        "SELECT size,mtime_ms FROM session_files WHERE path=?1",
                        params![path.to_string_lossy()],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()?;
                if existing == Some((metadata.len() as i64, modified_ms)) {
                    result.sessions_skipped += 1;
                    continue;
                }
                changed.push((path.to_path_buf(), metadata.len(), modified_ms));
            }
        }
        changed.sort_by_key(|item| std::cmp::Reverse(item.2));
        for (path, size, modified_ms) in changed {
            if max_files_to_index.is_some_and(|limit| result.sessions_processed >= limit) {
                result.reached_limit = true;
                break;
            }
            result.sessions_processed += 1;
            match parse_session_file(&path) {
                Ok(Some(document)) => {
                    match self.index_document(&path, &document, size, modified_ms) {
                        Ok((count, skipped)) => {
                            if skipped {
                                result.sessions_skipped += 1;
                            } else {
                                result.sessions_indexed += 1;
                                result.messages_indexed += count;
                            }
                        }
                        Err(error) => result.errors.push(format!("{}: {error}", path.display())),
                    }
                }
                Ok(None) => result
                    .errors
                    .push(format!("Failed to parse: {}", path.display())),
                Err(error) => result.errors.push(format!("{}: {error}", path.display())),
            }
        }
        if !result.reached_limit {
            self.touch_backfill_timestamp()?;
        }
        Ok(result)
    }

    pub(crate) fn needs_backfill(&self) -> Result<bool, StoreError> {
        let connection = self.open()?;
        let indexed: usize = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })?
            .try_into()
            .unwrap_or(usize::MAX);
        let mut files = Vec::new();
        for root in &self.session_roots {
            if !root.exists() {
                continue;
            }
            files.extend(
                WalkDir::new(root)
                    .max_depth(2)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry.file_type().is_file()
                            && entry.path().extension().and_then(|value| value.to_str())
                                == Some("jsonl")
                    })
                    .map(|entry| entry.into_path()),
            );
        }
        if files.len() > indexed {
            return Ok(true);
        }
        for path in files {
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => return Ok(true),
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                .unwrap_or(0);
            let existing = connection
                .query_row(
                    "SELECT size,mtime_ms FROM session_files WHERE path=?1",
                    params![path.to_string_lossy()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            if existing
                != Some((
                    i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                    modified_ms,
                ))
            {
                return Ok(true);
            }
        }
        let timestamp = connection
            .query_row(
                "SELECT value FROM extension_metadata WHERE key='last_session_backfill'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(timestamp
            .and_then(|timestamp| DateTime::parse_from_rfc3339(&timestamp).ok())
            .is_none_or(|timestamp| {
                Utc::now().signed_duration_since(timestamp.with_timezone(&Utc))
                    >= chrono::Duration::days(1)
            }))
    }

    fn touch_backfill_timestamp(&self) -> Result<(), StoreError> {
        self.open()?.execute(
            "INSERT INTO extension_metadata(key,value) VALUES('last_session_backfill',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn index_document(
        &self,
        path: &Path,
        document: &ParsedSession,
        size: u64,
        modified_ms: i64,
    ) -> Result<(usize, bool), StoreError> {
        let project = project_from_cwd(&document.cwd);
        let count = self.replace_session(
            &document.id,
            &project,
            &document.cwd,
            document.started_at.clone(),
            document.ended_at.clone(),
            &document.messages,
        )?;
        self.upsert_session_file(path, &document.id, size, modified_ms)?;
        Ok(count)
    }

    fn upsert_session_file(
        &self,
        path: &Path,
        session_id: &str,
        size: u64,
        modified_ms: i64,
    ) -> Result<(), StoreError> {
        self.open()?.execute(
            "INSERT INTO session_files(path,session_id,size,mtime_ms,indexed_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(path) DO UPDATE SET session_id=excluded.session_id,size=excluded.size,mtime_ms=excluded.mtime_ms,indexed_at=excluded.indexed_at",
            params![path.to_string_lossy(), session_id, i64::try_from(size).unwrap_or(i64::MAX), modified_ms, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn replace_session(
        &self,
        session_id: &str,
        project: &str,
        cwd: &Path,
        started_at: String,
        ended_at: Option<String>,
        messages: &[IndexedMessage],
    ) -> Result<(usize, bool), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT 1 FROM sessions WHERE id=?1",
                params![session_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let before: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO sessions(id,project,cwd,started_at,ended_at,message_count) VALUES(?1,?2,?3,?4,?5,?6)",
            params![session_id, project, cwd.to_string_lossy(), started_at, ended_at, messages.len() as i64],
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT OR IGNORE INTO messages(id,session_id,role,content,timestamp,tool_calls) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for message in messages {
                insert.execute(params![
                    message.id,
                    session_id,
                    message.role,
                    truncate_message(&message.content),
                    message.timestamp,
                    message
                        .tool_calls
                        .as_ref()
                        .map(|tools| serde_json::to_string(tools).unwrap_or_default()),
                ])?;
            }
        }
        transaction.execute(
            "UPDATE sessions SET project=?1,cwd=?2,ended_at=COALESCE(?3,ended_at),message_count=(SELECT COUNT(*) FROM messages WHERE session_id=?4) WHERE id=?4",
            params![project, cwd.to_string_lossy(), ended_at, session_id],
        )?;
        let after: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        let indexed = usize::try_from(after.saturating_sub(before)).unwrap_or_default();
        Ok((indexed, existing && indexed == 0))
    }

    pub(crate) fn search_sessions(
        &self,
        query: &str,
        options: &SessionSearchOptions,
    ) -> Result<Vec<SessionSearchHit>, StoreError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.open()?;
        let normalized = normalize_fts_query(query, false);
        let mut parse_failed = false;
        let exact = match run_session_search(&connection, &normalized, options) {
            Ok(rows) => rows,
            Err(error) if is_fts_error(&error) => {
                parse_failed = true;
                Vec::new()
            }
            Err(error) => return Err(error.into()),
        };
        if !exact.is_empty() {
            return Ok(exact);
        }
        if is_short_cjk_literal(query) {
            return like_session_search(&connection, &[query.trim().to_string()], options)
                .map_err(Into::into);
        }
        if has_explicit_operator(query) {
            if !parse_failed {
                return Ok(exact);
            }
            let natural = normalize_fts_query(query, true);
            if !natural.is_empty() && natural != normalized {
                if let Ok(rows) = run_session_search(&connection, &natural, options)
                    && !rows.is_empty()
                {
                    return Ok(rows);
                }
                let terms = collect_terms(query);
                if terms.len() > 1 {
                    let fallback = quote_terms(&terms, " OR ");
                    if fallback != natural
                        && let Ok(rows) = run_session_search(&connection, &fallback, options)
                        && !rows.is_empty()
                    {
                        return Ok(rows);
                    }
                }
            }
            return like_session_search(&connection, &collect_terms(query), options)
                .map_err(Into::into);
        }
        let terms = collect_terms(query);
        if terms.len() > 1 {
            let fallback = quote_terms(&terms, " OR ");
            if let Ok(rows) = run_session_search(&connection, &fallback, options)
                && !rows.is_empty()
            {
                return Ok(rows);
            }
        }
        like_session_search(&connection, &terms, options).map_err(Into::into)
    }

    pub(crate) fn indexed_message_count(&self) -> Result<usize, StoreError> {
        Ok(self
            .open()?
            .query_row("SELECT COUNT(*) FROM messages", [], |row| {
                row.get::<_, i64>(0)
            })? as usize)
    }

    pub(crate) fn session_file_inventory(&self) -> (usize, usize) {
        let mut files = 0_usize;
        let mut projects = std::collections::HashSet::new();
        for root in &self.session_roots {
            if !root.exists() {
                continue;
            }
            for entry in WalkDir::new(root)
                .max_depth(2)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().and_then(|value| value.to_str())
                            == Some("jsonl")
                })
            {
                files = files.saturating_add(1);
                if let Some(parent) = entry.path().parent()
                    && parent != root
                {
                    projects.insert(parent.to_path_buf());
                }
            }
        }
        (files, projects.len())
    }

    pub(crate) fn session_stats(&self) -> Result<SessionStats, StoreError> {
        let connection = self.open()?;
        let total_sessions = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })?
            .try_into()
            .unwrap_or(usize::MAX);
        let total_messages = connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| {
                row.get::<_, i64>(0)
            })?
            .try_into()
            .unwrap_or(usize::MAX);
        let projects = {
            let mut statement = connection.prepare(
                "SELECT s.project, COUNT(DISTINCT s.id), COUNT(m.id)
                 FROM sessions s LEFT JOIN messages m ON m.session_id=s.id
                 GROUP BY s.project ORDER BY s.project",
            )?;
            statement
                .query_map([], |row| {
                    Ok(ProjectSessionStats {
                        project: row.get(0)?,
                        sessions: usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX),
                        messages: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(usize::MAX),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(SessionStats {
            total_sessions,
            total_messages,
            projects,
        })
    }

    pub(crate) fn checkpoint(&self) -> Result<(), StoreError> {
        self.open()?
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

#[derive(Debug)]
struct ParsedSession {
    id: String,
    cwd: PathBuf,
    started_at: String,
    ended_at: Option<String>,
    messages: Vec<IndexedMessage>,
}

fn copy_readable_rows(
    source: &Connection,
    target: &Connection,
    table: &str,
    desired_columns: &[&str],
) -> Result<usize, StoreError> {
    let quoted_table = quote_identifier(table);
    let available = {
        let mut statement = source.prepare(&format!("PRAGMA table_info({quoted_table})"))?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<std::collections::HashSet<_>, _>>()?
    };
    let selected = desired_columns
        .iter()
        .copied()
        .filter(|column| available.contains(*column))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(0);
    }
    let columns = selected
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=selected.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut read = source.prepare(&format!("SELECT {columns} FROM {quoted_table} NOT INDEXED"))?;
    let mut insert = target.prepare(&format!(
        "INSERT OR IGNORE INTO {quoted_table} ({columns}) VALUES ({placeholders})"
    ))?;
    let mut rows = read.query([])?;
    let mut copied = 0_usize;
    while let Some(row) = rows.next()? {
        let values = (0..selected.len())
            .map(|index| row.get::<_, rusqlite::types::Value>(index))
            .collect::<Result<Vec<_>, _>>()?;
        if insert.execute(params_from_iter(values)).is_ok() {
            copied = copied.saturating_add(1);
        }
    }
    Ok(copied)
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn remove_database_file_set(base: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{suffix}", base.display())));
    }
}

fn parse_session_file(path: &Path) -> Result<Option<ParsedSession>, std::io::Error> {
    let file = fs::File::open(path)?;
    let mut session_id = None;
    let mut cwd = None;
    let mut started_at = None;
    let mut messages = Vec::new();

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let is_header = entry.get("type").and_then(serde_json::Value::as_str) == Some("session")
            || entry.get("kind").and_then(serde_json::Value::as_str) == Some("header");
        if is_header {
            session_id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            cwd = entry
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from);
            started_at = entry
                .get("timestamp")
                .or_else(|| entry.get("createdAt"))
                .and_then(timestamp_value);
            continue;
        }
        if entry.get("type").and_then(serde_json::Value::as_str) != Some("message") {
            continue;
        }
        let Some(id) = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(timestamp) = entry.get("timestamp").and_then(timestamp_value) else {
            continue;
        };
        let Some(message) = entry.get("message").and_then(serde_json::Value::as_object) else {
            continue;
        };
        let role = match message.get("role").and_then(serde_json::Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            Some("system") => "system",
            _ => continue,
        };
        let content_value = message.get("content");
        let content = extract_text_content(content_value);
        if content.is_empty() {
            continue;
        }
        let tool_calls = (role == "assistant")
            .then(|| extract_tool_calls(content_value))
            .filter(|calls| !calls.is_empty());
        messages.push(IndexedMessage {
            id,
            role,
            content,
            timestamp,
            tool_calls,
        });
    }

    Ok(match (session_id, cwd, started_at) {
        (Some(id), Some(cwd), Some(started_at)) => Some(ParsedSession {
            id,
            cwd,
            started_at,
            ended_at: None,
            messages,
        }),
        _ => None,
    })
}

fn extract_text_content(content: Option<&serde_json::Value>) -> String {
    if let Some(content) = content.and_then(serde_json::Value::as_str) {
        return content.to_string();
    }
    content
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            let block = block.as_object()?;
            (block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(serde_json::Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn extract_tool_calls(content: Option<&serde_json::Value>) -> Vec<String> {
    content
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            let block = block.as_object()?;
            matches!(
                block.get("type").and_then(serde_json::Value::as_str),
                Some("toolCall" | "tool_use")
            )
            .then(|| {
                block
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .flatten()
        })
        .collect()
}

fn timestamp_value(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(iso))
        .or_else(|| {
            value
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .map(iso)
        })
}

fn project_from_cwd(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| cwd.to_str().unwrap_or("unknown"))
        .to_string()
}

fn is_ephemeral_hermes_session(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| part.starts_with("pi-hermes-prompt-"))
    })
}

#[derive(Debug)]
struct IndexedMessage {
    id: String,
    role: &'static str,
    content: String,
    timestamp: String,
    tool_calls: Option<Vec<String>>,
}

fn indexed_message(id: String, timestamp: String, message: &Message) -> Option<IndexedMessage> {
    let (role, blocks) = match message {
        Message::User(message) => ("user", message.content.as_slice()),
        Message::Assistant(message) => ("assistant", message.content.as_slice()),
        Message::ToolResult(_) | Message::Custom(_) => return None,
    };
    let content = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if content.is_empty() {
        return None;
    }
    let tool_calls = matches!(message, Message::Assistant(_)).then(|| {
        blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    Some(IndexedMessage {
        id,
        role,
        content,
        timestamp,
        tool_calls: tool_calls.filter(|calls| !calls.is_empty()),
    })
}

fn truncate_message(content: &str) -> String {
    let count = char_len(content);
    if count <= MAX_MESSAGE_CONTENT_LENGTH {
        return content.to_string();
    }
    let notice = format!("\n... (truncated, {count} chars total)\n");
    let retained = MAX_MESSAGE_CONTENT_LENGTH.saturating_sub(char_len(&notice));
    let prefix = retained.div_ceil(2);
    let suffix = retained / 2;
    format!(
        "{}{}{}",
        char_prefix(content, prefix),
        notice,
        char_suffix(content, suffix)
    )
}

fn initialize_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS extension_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,project TEXT NOT NULL,cwd TEXT NOT NULL,started_at TEXT NOT NULL,ended_at TEXT,message_count INTEGER DEFAULT 0);
         CREATE TABLE IF NOT EXISTS session_files(path TEXT PRIMARY KEY,session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,size INTEGER NOT NULL,mtime_ms INTEGER NOT NULL,indexed_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,session_id TEXT NOT NULL REFERENCES sessions(id),role TEXT NOT NULL CHECK(role IN ('user','assistant','system')),content TEXT NOT NULL,timestamp TEXT NOT NULL,tool_calls TEXT);
         CREATE TABLE IF NOT EXISTS memories(id INTEGER PRIMARY KEY AUTOINCREMENT,project TEXT,target TEXT NOT NULL CHECK(target IN ('memory','user','failure')),category TEXT CHECK(category IN ('failure','correction','insight','preference','convention','tool-quirk')),content TEXT NOT NULL,failure_reason TEXT,tool_state TEXT,corrected_to TEXT,created DATE NOT NULL,last_referenced DATE NOT NULL);",
    )?;
    ensure_legacy_columns(connection)?;
    migrate_legacy_memory_target_constraint(connection)?;
    ensure_schema(connection)?;
    migrate_fts_tokenizer(connection)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn ensure_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS extension_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,project TEXT NOT NULL,cwd TEXT NOT NULL,started_at TEXT NOT NULL,ended_at TEXT,message_count INTEGER DEFAULT 0);
         CREATE TABLE IF NOT EXISTS session_files(path TEXT PRIMARY KEY,session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,size INTEGER NOT NULL,mtime_ms INTEGER NOT NULL,indexed_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,session_id TEXT NOT NULL REFERENCES sessions(id),role TEXT NOT NULL CHECK(role IN ('user','assistant','system')),content TEXT NOT NULL,timestamp TEXT NOT NULL,tool_calls TEXT);
         CREATE TABLE IF NOT EXISTS memories(id INTEGER PRIMARY KEY AUTOINCREMENT,project TEXT,target TEXT NOT NULL CHECK(target IN ('memory','user','failure')),category TEXT CHECK(category IN ('failure','correction','insight','preference','convention','tool-quirk')),content TEXT NOT NULL,failure_reason TEXT,tool_state TEXT,corrected_to TEXT,created DATE NOT NULL,last_referenced DATE NOT NULL);
         CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
         CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
         CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project);
         CREATE INDEX IF NOT EXISTS idx_memories_target ON memories(target);
         CREATE INDEX IF NOT EXISTS idx_memories_category ON memories(category);
         CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project);
         CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at);
         CREATE INDEX IF NOT EXISTS idx_session_files_session_id ON session_files(session_id);",
    )?;
    if !table_exists(connection, "message_fts")? {
        create_fts(connection, "message_fts", "messages", "content", "rowid")?;
    }
    if !table_exists(connection, "memory_fts")? {
        create_fts(connection, "memory_fts", "memories", "content", "id")?;
    }
    connection.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN INSERT INTO message_fts(rowid,content) VALUES(new.rowid,new.content); END;
         CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN INSERT INTO message_fts(message_fts,rowid,content) VALUES('delete',old.rowid,old.content); END;
         CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN INSERT INTO message_fts(message_fts,rowid,content) VALUES('delete',old.rowid,old.content); INSERT INTO message_fts(rowid,content) VALUES(new.rowid,new.content); END;
         CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN INSERT INTO memory_fts(rowid,content) VALUES(new.id,new.content); END;
         CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN INSERT INTO memory_fts(memory_fts,rowid,content) VALUES('delete',old.id,old.content); END;
         CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN INSERT INTO memory_fts(memory_fts,rowid,content) VALUES('delete',old.id,old.content); INSERT INTO memory_fts(rowid,content) VALUES(new.id,new.content); END;",
    )?;
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn create_fts(
    connection: &Connection,
    table: &str,
    content_table: &str,
    column: &str,
    rowid: &str,
) -> Result<(), rusqlite::Error> {
    connection.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {table} USING fts5({column},content='{content_table}',content_rowid='{rowid}',tokenize='trigram')"
    ))?;
    Ok(())
}

fn column_names(
    connection: &Connection,
    table: &str,
) -> Result<std::collections::HashSet<String>, rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}

fn ensure_legacy_columns(connection: &Connection) -> Result<(), rusqlite::Error> {
    let memories = column_names(connection, "memories")?;
    for (name, kind) in [
        ("project", "TEXT"),
        ("category", "TEXT"),
        ("failure_reason", "TEXT"),
        ("tool_state", "TEXT"),
        ("corrected_to", "TEXT"),
    ] {
        if !memories.contains(name) {
            connection.execute_batch(&format!("ALTER TABLE memories ADD COLUMN {name} {kind}"))?;
        }
    }
    let sessions = column_names(connection, "sessions")?;
    if !sessions.contains("project") {
        connection.execute_batch("ALTER TABLE sessions ADD COLUMN project TEXT")?;
    }
    let missing = {
        let mut statement = connection
            .prepare("SELECT id,cwd FROM sessions WHERE project IS NULL OR trim(project)='' ")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, cwd) in missing {
        let project = Path::new(&cwd)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        connection.execute(
            "UPDATE sessions SET project=?1 WHERE id=?2",
            params![project, id],
        )?;
    }
    Ok(())
}

fn migrate_legacy_memory_target_constraint(connection: &Connection) -> Result<(), rusqlite::Error> {
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_default();
    let squashed = sql
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if !squashed.contains("targettextnotnullcheck(targetin('memory','user'))") {
        return Ok(());
    }
    connection.execute_batch(
        "PRAGMA foreign_keys=OFF;
         DROP TRIGGER IF EXISTS memories_ai;
         DROP TRIGGER IF EXISTS memories_ad;
         DROP TRIGGER IF EXISTS memories_au;
         DROP TABLE IF EXISTS memory_fts;
         BEGIN IMMEDIATE;
         CREATE TABLE memories_new(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           project TEXT,
           target TEXT NOT NULL CHECK(target IN ('memory','user','failure')),
           category TEXT CHECK(category IN ('failure','correction','insight','preference','convention','tool-quirk')),
           content TEXT NOT NULL,
           failure_reason TEXT,
           tool_state TEXT,
           corrected_to TEXT,
           created DATE NOT NULL,
           last_referenced DATE NOT NULL
         );
         INSERT INTO memories_new(id,project,target,category,content,failure_reason,tool_state,corrected_to,created,last_referenced)
         SELECT id,project,target,category,content,failure_reason,tool_state,corrected_to,created,last_referenced FROM memories;
         DROP TABLE memories;
         ALTER TABLE memories_new RENAME TO memories;
         COMMIT;
         PRAGMA foreign_keys=ON;",
    )?;
    Ok(())
}

fn migrate_fts_tokenizer(connection: &Connection) -> Result<(), rusqlite::Error> {
    let uses_trigram = |table: &str| -> Result<bool, rusqlite::Error> {
        let sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(sql.is_some_and(|sql| sql.to_ascii_lowercase().contains("tokenize='trigram'")))
    };
    let version = connection
        .query_row(
            "SELECT value FROM extension_metadata WHERE key='fts5_tokenizer_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if version.as_deref() == Some("trigram-v1")
        && uses_trigram("message_fts")?
        && uses_trigram("memory_fts")?
    {
        return Ok(());
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         DROP TRIGGER IF EXISTS messages_ai;
         DROP TRIGGER IF EXISTS messages_ad;
         DROP TRIGGER IF EXISTS messages_au;
         DROP TRIGGER IF EXISTS memories_ai;
         DROP TRIGGER IF EXISTS memories_ad;
         DROP TRIGGER IF EXISTS memories_au;
         DROP TABLE IF EXISTS message_fts;
         DROP TABLE IF EXISTS memory_fts;
         CREATE VIRTUAL TABLE message_fts USING fts5(content,content='messages',content_rowid='rowid',tokenize='trigram');
         CREATE VIRTUAL TABLE memory_fts USING fts5(content,content='memories',content_rowid='id',tokenize='trigram');
         INSERT INTO message_fts(message_fts) VALUES('rebuild');
         INSERT INTO memory_fts(memory_fts) VALUES('rebuild');
         CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN INSERT INTO message_fts(rowid,content) VALUES(new.rowid,new.content); END;
         CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN INSERT INTO message_fts(message_fts,rowid,content) VALUES('delete',old.rowid,old.content); END;
         CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN INSERT INTO message_fts(message_fts,rowid,content) VALUES('delete',old.rowid,old.content); INSERT INTO message_fts(rowid,content) VALUES(new.rowid,new.content); END;
         CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN INSERT INTO memory_fts(rowid,content) VALUES(new.id,new.content); END;
         CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN INSERT INTO memory_fts(memory_fts,rowid,content) VALUES('delete',old.id,old.content); END;
         CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN INSERT INTO memory_fts(memory_fts,rowid,content) VALUES('delete',old.id,old.content); INSERT INTO memory_fts(rowid,content) VALUES(new.id,new.content); END;
         INSERT INTO extension_metadata(key,value) VALUES('fts5_tokenizer_version','trigram-v1') ON CONFLICT(key) DO UPDATE SET value=excluded.value;
         COMMIT;",
    )?;
    Ok(())
}

fn run_memory_search(
    connection: &Connection,
    query: &str,
    options: &MemorySearchOptions,
) -> Result<Vec<MemorySearchHit>, rusqlite::Error> {
    let (where_clause, mut values) = memory_filters(options);
    let sql = format!(
        "SELECT m.id,m.project,m.target,m.category,m.content,m.failure_reason,m.tool_state,m.corrected_to,m.created,m.last_referenced FROM memories m WHERE m.id IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?) {where_clause} ORDER BY m.last_referenced DESC LIMIT ?"
    );
    let mut parameters: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(query.to_string())];
    parameters.append(&mut values);
    parameters.push(Box::new(options.limit.min(20) as i64));
    query_memory_rows(connection, &sql, parameters)
}

fn like_memory_search(
    connection: &Connection,
    terms: &[String],
    options: &MemorySearchOptions,
) -> Result<Vec<MemorySearchHit>, rusqlite::Error> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let likes = (0..terms.len())
        .map(|_| "m.content LIKE ? ESCAPE '\\'")
        .collect::<Vec<_>>()
        .join(" OR ");
    let (filters, mut values) = memory_filters(options);
    let sql = format!(
        "SELECT m.id,m.project,m.target,m.category,m.content,m.failure_reason,m.tool_state,m.corrected_to,m.created,m.last_referenced FROM memories m WHERE ({likes}) {filters} ORDER BY m.last_referenced DESC LIMIT ?"
    );
    let mut parameters = terms
        .iter()
        .map(|term| Box::new(format!("%{}%", escape_like(term))) as Box<dyn rusqlite::ToSql>)
        .collect::<Vec<_>>();
    parameters.append(&mut values);
    parameters.push(Box::new(options.limit.min(20) as i64));
    query_memory_rows(connection, &sql, parameters)
}

fn memory_filters(options: &MemorySearchOptions) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clauses = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(project) = &options.project {
        match project {
            Some(project) => {
                clauses.push("m.project=?");
                values.push(Box::new(project.clone()));
            }
            None => clauses.push("m.project IS NULL"),
        }
    }
    if let Some(target) = options.target {
        if target == MemoryTarget::Project {
            clauses.push("m.target='memory' AND m.project IS NOT NULL");
        } else {
            clauses.push("m.target=?");
            values.push(Box::new(target.index_target().to_string()));
        }
    }
    if let Some(category) = options.category {
        clauses.push("m.category=?");
        values.push(Box::new(category.as_str().to_string()));
    }
    let clause = clauses
        .into_iter()
        .map(|clause| format!("AND {clause}"))
        .collect::<Vec<_>>()
        .join(" ");
    (clause, values)
}

fn query_memory_rows(
    connection: &Connection,
    sql: &str,
    values: Vec<Box<dyn rusqlite::ToSql>>,
) -> Result<Vec<MemorySearchHit>, rusqlite::Error> {
    let references = values
        .iter()
        .map(|value| value.as_ref() as &dyn rusqlite::ToSql)
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(references.as_slice(), |row| {
            let project = row.get::<_, Option<String>>(1)?;
            let target = MemoryTarget::from_index_target(
                row.get::<_, String>(2)?.as_str(),
                project.is_some(),
            )
            .unwrap_or(MemoryTarget::Memory);
            let category = row
                .get::<_, Option<String>>(3)?
                .and_then(|value| MemoryCategory::parse(&value));
            Ok(MemorySearchHit {
                id: row.get(0)?,
                project,
                target,
                category,
                content: row.get(4)?,
                failure_reason: row.get(5)?,
                tool_state: row.get(6)?,
                corrected_to: row.get(7)?,
                created: row.get(8)?,
                last_referenced: row.get(9)?,
            })
        })?
        .collect()
}

fn run_session_search(
    connection: &Connection,
    query: &str,
    options: &SessionSearchOptions,
) -> Result<Vec<SessionSearchHit>, rusqlite::Error> {
    session_search_query(connection, Some(query), &[], options)
}

fn like_session_search(
    connection: &Connection,
    terms: &[String],
    options: &SessionSearchOptions,
) -> Result<Vec<SessionSearchHit>, rusqlite::Error> {
    session_search_query(connection, None, terms, options)
}

fn session_search_query(
    connection: &Connection,
    fts: Option<&str>,
    terms: &[String],
    options: &SessionSearchOptions,
) -> Result<Vec<SessionSearchHit>, rusqlite::Error> {
    if fts.is_none() && terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut clauses = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(query) = fts {
        clauses.push(
            "m.rowid IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)".to_string(),
        );
        values.push(Box::new(query.to_string()));
    } else {
        let conditions = (0..terms.len())
            .map(|_| "m.content LIKE ? ESCAPE '\\'")
            .collect::<Vec<_>>()
            .join(" OR ");
        clauses.push(format!("({conditions})"));
        for term in terms {
            values.push(Box::new(format!("%{}%", escape_like(term))));
        }
    }
    if let Some(project) = &options.project {
        clauses.push("s.project=?".to_string());
        values.push(Box::new(project.clone()));
    }
    if let Some(role) = &options.role {
        clauses.push("m.role=?".to_string());
        values.push(Box::new(role.clone()));
    }
    if let Some(since) = &options.since {
        clauses.push("m.timestamp>=?".to_string());
        values.push(Box::new(since.clone()));
    }
    values.push(Box::new(options.limit.clamp(1, 20) as i64));
    let sql = format!(
        "SELECT m.session_id,s.project,m.role,m.content,m.timestamp FROM messages m JOIN sessions s ON s.id=m.session_id WHERE {} ORDER BY m.timestamp DESC LIMIT ?",
        clauses.join(" AND ")
    );
    let references = values
        .iter()
        .map(|value| value.as_ref() as &dyn rusqlite::ToSql)
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(&sql)?;
    statement
        .query_map(references.as_slice(), |row| {
            let content: String = row.get(3)?;
            Ok(SessionSearchHit {
                session_id: row.get(0)?,
                project: row.get(1)?,
                role: row.get(2)?,
                snippet: clip(&content, options.snippet_chars.clamp(100, 10_000)),
                content,
                timestamp: row.get(4)?,
            })
        })?
        .collect()
}

fn collect_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in query.trim().chars() {
        match character {
            '"' => {
                if quoted && !current.is_empty() {
                    terms.push(std::mem::take(&mut current));
                }
                quoted = !quoted;
            }
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    let value = std::mem::take(&mut current);
                    if !matches!(
                        value.to_ascii_lowercase().as_str(),
                        "and" | "or" | "not" | "near"
                    ) {
                        terms.push(value);
                    }
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty()
        && !matches!(
            current.to_ascii_lowercase().as_str(),
            "and" | "or" | "not" | "near"
        )
    {
        terms.push(current);
    }
    terms
}

fn normalize_fts_query(query: &str, force_natural: bool) -> String {
    if !force_natural && has_explicit_operator(query) {
        return query.trim().to_string();
    }
    quote_terms(&collect_terms(query), " ")
}

fn quote_terms(terms: &[String], separator: &str) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(separator)
}

fn has_explicit_operator(query: &str) -> bool {
    query
        .split_whitespace()
        .any(|value| matches!(value, "OR" | "AND" | "NOT" | "NEAR"))
}

fn is_short_cjk_literal(query: &str) -> bool {
    let trimmed = query.trim();
    let count = trimmed.chars().count();
    (1..=2).contains(&count)
        && trimmed
            .chars()
            .all(|character| ('\u{2e80}'..='\u{9fff}').contains(&character))
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn is_fts_error(error: &rusqlite::Error) -> bool {
    error.to_string().to_ascii_lowercase().contains("fts5")
        || error
            .to_string()
            .to_ascii_lowercase()
            .contains("unterminated string")
}

fn database_is_corrupt(error: &rusqlite::Error) -> bool {
    if matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            )
    ) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    message.contains("database disk image is malformed")
        || message.contains("file is not a database")
        || message.contains("database schema is corrupt")
        || message.contains("malformed database schema")
        || message.contains("btreeinitpage")
        || message.contains("sqlite_corrupt")
        || message.contains("sqlite_notadb")
        || message.contains("quick_check failed")
}

fn iso(milliseconds: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

fn clip(value: &str, maximum: usize) -> String {
    let count = char_len(value);
    if count <= maximum {
        return value.to_string();
    }
    let retained = maximum.saturating_sub(1);
    format!("{}…", char_prefix(value, retained))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database_at(path: PathBuf) -> Result<Database, StoreError> {
        Database::new(path, PathBuf::new(), "projects".to_string(), Vec::new())
    }

    fn insert_recovery_fixture(path: &Path) {
        let connection = Connection::open(path).expect("open recovery fixture");
        connection
            .execute(
                "INSERT INTO sessions(id,project,cwd,started_at,message_count) VALUES(?1,?2,?3,?4,?5)",
                params!["recover-session", "recover-project", "/work/recover", "2026-05-03T00:00:00Z", 50],
            )
            .expect("insert session");
        for index in 0..50 {
            connection
                .execute(
                    "INSERT INTO messages(id,session_id,role,content,timestamp) VALUES(?1,?2,?3,?4,?5)",
                    params![
                        format!("recover-msg-{index}"),
                        "recover-session",
                        if index % 2 == 0 { "user" } else { "assistant" },
                        format!("message {index}"),
                        format!("2026-05-03T00:{index:02}:00Z"),
                    ],
                )
                .expect("insert message");
        }
        connection
            .execute(
                "INSERT INTO memories(project,target,content,created,last_referenced) VALUES(NULL,'memory','recoverable memory','2026-05-03','2026-05-03')",
                [],
            )
            .expect("insert memory");
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint fixture");
    }

    fn corrupt_recoverable_index_page(path: &Path, index: &str) {
        use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

        let connection = Connection::open(path).expect("open fixture for dbstat");
        let page_size = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map(|value: i64| u64::try_from(value).expect("positive page size"))
            .expect("read page size");
        let page = connection
            .query_row(
                "SELECT pageno FROM dbstat WHERE name=?1 AND pagetype IN ('internal','leaf') ORDER BY pageno ASC LIMIT 1",
                params![index],
                |row| row.get(0),
            )
            .map(|value: i64| u64::try_from(value).expect("positive page number"))
            .expect("find index page");
        drop(connection);
        assert!(page > 1, "must not corrupt the SQLite header page");

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open database bytes");
        file.seek(SeekFrom::Start((page - 1) * page_size))
            .expect("seek to index page");
        let mut bytes = [0_u8; 16];
        file.read_exact(&mut bytes).expect("read index bytes");
        for byte in &mut bytes {
            *byte ^= 0xff;
        }
        file.seek(SeekFrom::Start((page - 1) * page_size))
            .expect("seek back to index page");
        file.write_all(&bytes).expect("corrupt index bytes");
        file.sync_all().expect("sync corruption");
    }

    #[test]
    fn query_normalization_preserves_explicit_operators_and_broadens_plain_text() {
        assert_eq!(
            normalize_fts_query("alpha beta", false),
            "\"alpha\" \"beta\""
        );
        assert_eq!(normalize_fts_query("alpha OR beta", false), "alpha OR beta");
        assert_eq!(
            quote_terms(&collect_terms("alpha beta"), " OR "),
            "\"alpha\" OR \"beta\""
        );
    }

    #[test]
    fn large_messages_keep_both_ends() {
        let value = format!("start{}end", "x".repeat(MAX_MESSAGE_CONTENT_LENGTH));
        let truncated = truncate_message(&value);
        assert!(truncated.starts_with("start"));
        assert!(truncated.ends_with("end"));
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn parser_matches_pi_jsonl_and_ignores_ephemeral_blocks() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("session.jsonl");
        fs::write(
            &path,
            [
                serde_json::json!({"type":"session","id":"s1","timestamp":"2026-05-03T00:00:00Z","cwd":"/work/demo"}).to_string(),
                "not valid json".to_string(),
                serde_json::json!({"type":"thinking_level_change","id":"ignored","timestamp":"2026-05-03T00:00:01Z"}).to_string(),
                serde_json::json!({
                    "type":"message",
                    "id":"m1",
                    "timestamp":"2026-05-03T00:01:00Z",
                    "message":{
                        "role":"assistant",
                        "content":[
                            {"type":"thinking","thinking":"private"},
                            {"type":"text","text":"I inspected the file."},
                            {"type":"tool_use","name":"read","input":{}},
                            {"type":"toolCall","name":"bash","arguments":{}},
                            {"type":"tool_result","content":[{"type":"text","text":"large output"}]}
                        ]
                    }
                }).to_string(),
                serde_json::json!({
                    "type":"message",
                    "id":"m2",
                    "timestamp":"2026-05-03T00:02:00Z",
                    "message":{"role":"toolResult","content":[{"type":"text","text":"ignored"}]}
                }).to_string(),
            ]
            .join("\n"),
        )
        .expect("write session fixture");

        let parsed = parse_session_file(&path)
            .expect("parse session")
            .expect("valid session");
        assert_eq!(parsed.id, "s1");
        assert_eq!(parsed.cwd, Path::new("/work/demo"));
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].content, "I inspected the file.");
        assert_eq!(
            parsed.messages[0].tool_calls.as_deref(),
            Some(["read".to_string(), "bash".to_string()].as_slice())
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_file_symlink_is_preserved() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let target = directory.path().join("actual.db");
        database_at(target.clone()).expect("create target database");
        let alias = directory.path().join("sessions.db");
        symlink(&target, &alias).expect("create database symlink");

        let database = database_at(alias.clone()).expect("open through symlink");
        assert_eq!(database.indexed_memory_count().expect("count memories"), 0);
        assert!(
            fs::symlink_metadata(alias)
                .expect("read alias metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_relative_database_symlink_is_preserved_and_materialized() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).expect("create target directory");
        let alias = directory.path().join("sessions.db");
        symlink("nested/actual.db", &alias).expect("create dangling database symlink");

        database_at(alias.clone()).expect("open dangling database symlink");
        assert!(nested.join("actual.db").is_file());
        assert!(
            fs::symlink_metadata(alias)
                .expect("read alias metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_symlink_loop_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let first = directory.path().join("first.db");
        let second = directory.path().join("second.db");
        symlink("second.db", &first).expect("first link");
        symlink("first.db", &second).expect("second link");

        assert!(database_at(first).is_err());
    }

    #[test]
    fn unrecoverable_database_is_quarantined_and_recreated() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("sessions.db");
        fs::write(&path, "not a sqlite database").expect("write corrupt database");

        let database = database_at(path).expect("recover corrupt database");
        assert_eq!(database.indexed_memory_count().expect("count memories"), 0);
        assert!(
            fs::read_dir(directory.path())
                .expect("list backups")
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sessions.db.corrupt-"))
        );
    }

    #[test]
    fn recoverable_index_corruption_preserves_readable_rows() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("sessions.db");
        database_at(path.clone()).expect("create database");
        insert_recovery_fixture(&path);
        corrupt_recoverable_index_page(&path, "idx_messages_timestamp");

        let database = database_at(path).expect("rebuild corrupt database");
        assert_eq!(database.indexed_memory_count().expect("count memories"), 1);
        let stats = database.session_stats().expect("read recovered stats");
        assert_eq!(stats.total_sessions, 1);
        assert_eq!(stats.total_messages, 50);
        assert!(
            fs::read_dir(directory.path())
                .expect("list backups")
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sessions.db.corrupt-"))
        );
    }

    #[test]
    fn recovery_circuit_uses_recent_failures_and_ignores_legacy_attempt_shape() {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = Database {
            path: directory.path().join("sessions.db"),
            session_roots: Vec::new(),
        };
        let state_path = database.recovery_circuit_path();
        fs::write(&state_path, r#"{"attempts":99}"#).expect("write legacy state");
        assert!(database.assert_recovery_circuit_closed().is_ok());

        let now = Utc::now().timestamp_millis();
        fs::write(
            &state_path,
            serde_json::to_vec(&RecoveryCircuitState {
                failures: vec![now, now, now],
            })
            .expect("serialize state"),
        )
        .expect("write state");
        let error = database
            .assert_recovery_circuit_closed()
            .expect_err("three recent failures open the circuit");
        assert!(error.to_string().contains("recovery circuit is open"));

        database.clear_recovery_failures_best_effort();
        assert!(!state_path.exists());
    }
}
