use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::jsonl::{validate_header, validate_header_json_shape};
use crate::{
    ForkOptions, JsonlSessionCreateOptions, JsonlSessionListOptions, JsonlSessionMetadata,
    SESSION_SCHEMA_VERSION, Session, SessionError, SessionHeader, SessionLog, next_unique_id,
    now_ms,
};

#[derive(Clone)]
pub struct JsonlSessionRepo {
    sessions_root: PathBuf,
    mutation_gate: Arc<Mutex<()>>,
}

impl JsonlSessionRepo {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
            mutation_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn create(
        &self,
        options: JsonlSessionCreateOptions,
    ) -> Result<Session<SessionLog>, SessionError> {
        let _gate = self.gate();
        let (header, path) = self.prepare_create(options)?;
        SessionLog::create(path, header).map(Session::new)
    }

    pub fn open(
        &self,
        metadata: &JsonlSessionMetadata,
    ) -> Result<Session<SessionLog>, SessionError> {
        if !metadata.path.exists() {
            return Err(SessionError::NotFound(metadata.id.clone()));
        }
        let (log, _) = SessionLog::open(&metadata.path)?;
        if log.header().id != metadata.id {
            return Err(SessionError::InvalidEntry(format!(
                "session id does not match header: {}",
                metadata.id
            )));
        }
        Ok(Session::new(log))
    }

    pub fn list(
        &self,
        options: &JsonlSessionListOptions,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        list_jsonl_session_metadata(&self.sessions_root, options)
    }

    pub fn delete(&self, metadata: &JsonlSessionMetadata) -> Result<(), SessionError> {
        let _gate = self.gate();
        match std::fs::remove_file(&metadata.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn fork(
        &self,
        source: &JsonlSessionMetadata,
        options: &ForkOptions,
        mut create: JsonlSessionCreateOptions,
    ) -> Result<Session<SessionLog>, SessionError> {
        let _gate = self.gate();
        let source_log = self.open(source)?.storage().clone();
        if create.parent_session_id.is_none() {
            create.parent_session_id = Some(source.id.clone());
        }
        let (header, path) = self.prepare_create(create)?;
        source_log.fork(path, header, options).map(Session::new)
    }

    fn prepare_create(
        &self,
        options: JsonlSessionCreateOptions,
    ) -> Result<(SessionHeader, PathBuf), SessionError> {
        let id = options.id.unwrap_or_else(|| next_unique_id("session"));
        validate_session_id(&id)?;
        let cwd = absolute_path(&options.cwd)?;
        let directory = self.session_directory(&cwd)?;
        if directory.exists() {
            let suffix = format!("_{id}.jsonl");
            if std::fs::read_dir(&directory)?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(suffix.as_str())
                })
            {
                return Err(SessionError::AlreadyExists(id));
            }
        }

        let created_at = now_ms();
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}_{}.jsonl", session_timestamp(created_at), id));
        let mut header = SessionHeader::new(id, cwd);
        header.created_at = created_at;
        header.parent_session_id = options.parent_session_id;
        header.metadata = options.metadata;
        Ok((header, path))
    }

    fn session_directory(&self, cwd: &Path) -> Result<PathBuf, SessionError> {
        Ok(absolute_path(&self.sessions_root)?.join(jsonl_session_directory_name(cwd)))
    }

    fn gate(&self) -> MutexGuard<'_, ()> {
        self.mutation_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub fn list_jsonl_session_metadata(
    sessions_root: &Path,
    options: &JsonlSessionListOptions,
) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
    let root = absolute_path(sessions_root)?;
    let directories = match &options.cwd {
        Some(cwd) => {
            let cwd = absolute_path(cwd)?;
            let directory = root.join(jsonl_session_directory_name(&cwd));
            if directory.exists() {
                vec![directory]
            } else {
                Vec::new()
            }
        }
        None if !root.exists() => Vec::new(),
        None => std::fs::read_dir(&root)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir() || kind.is_symlink())
                    .map(|_| entry.path())
            })
            .collect(),
    };

    let mut metadata = Vec::new();
    for directory in directories {
        for entry in std::fs::read_dir(directory)?.filter_map(Result::ok) {
            let path = entry.path();
            let is_jsonl = path.extension() == Some(OsStr::new("jsonl"));
            let is_file = entry.file_type().is_ok_and(|kind| !kind.is_dir());
            if !is_jsonl || !is_file {
                continue;
            }
            let Some(header) = read_header_for_listing(&path) else {
                continue;
            };
            let modified_at = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0.0, |duration| duration.as_secs_f64() * 1_000.0);
            metadata.push(JsonlSessionMetadata {
                id: header.id,
                created_at: header.created_at,
                cwd: header.cwd,
                path,
                modified_at,
                source_format: SESSION_SCHEMA_VERSION,
                parent_session_id: header.parent_session_id,
                legacy_parent_session_path: header.legacy_parent_session_path,
                metadata: header.metadata,
            });
        }
    }
    metadata.sort_by(|left, right| right.modified_at.total_cmp(&left.modified_at));
    Ok(metadata)
}

pub fn load_jsonl_session(metadata: &JsonlSessionMetadata) -> Result<SessionLog, SessionError> {
    if !metadata.path.exists() {
        return Err(SessionError::NotFound(metadata.id.clone()));
    }
    let (log, _) = SessionLog::open(&metadata.path)?;
    if log.header().id != metadata.id {
        return Err(SessionError::InvalidEntry(format!(
            "session id does not match header: {}",
            metadata.id
        )));
    }
    Ok(log)
}

fn read_header_for_listing(path: &Path) -> Option<SessionHeader> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    let value: serde_json::Value =
        serde_json::from_str(line.trim_end_matches(['\r', '\n'])).ok()?;
    validate_header_json_shape(&value, 1).ok()?;
    let header: SessionHeader = serde_json::from_value(value).ok()?;
    validate_header(&header).ok().map(|()| header)
}

fn validate_session_id(id: &str) -> Result<(), SessionError> {
    let valid_edge = |byte: u8| byte.is_ascii_alphanumeric();
    let bytes = id.as_bytes();
    let valid = bytes.first().is_some_and(|byte| valid_edge(*byte))
        && bytes.last().is_some_and(|byte| valid_edge(*byte))
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(SessionError::InvalidPayload(
            "session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character"
                .to_string(),
        ))
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, SessionError> {
    std::path::absolute(path).map_err(SessionError::from)
}

fn jsonl_session_directory_name(cwd: &Path) -> String {
    let raw = cwd.to_string_lossy();
    let mut characters = raw.chars();
    let first = characters.next();
    let remainder = match first {
        Some('/' | '\\') => characters.collect::<String>(),
        Some(first) => std::iter::once(first).chain(characters).collect(),
        None => String::new(),
    };
    let encoded = remainder
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect::<String>();
    format!("--{encoded}--")
}

fn session_timestamp(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1000);
    let milliseconds = timestamp_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_date_from_unix_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}-{milliseconds:03}Z")
}

fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use pi_core::{Message, UserMessage};

    use super::*;
    use crate::{ForkPosition, SessionEntryType};

    #[test]
    fn repo_uses_pi_directory_and_filename_layout() {
        let directory = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepo::new(directory.path());
        let cwd = directory.path().join("workspace/project");
        let session = repo
            .create(JsonlSessionCreateOptions {
                id: Some("metadata".to_string()),
                cwd: cwd.clone(),
                parent_session_id: Some("parent".to_string()),
                metadata: None,
            })
            .unwrap();
        let metadata = session.metadata().unwrap();
        assert_eq!(metadata.cwd, cwd);
        assert!(
            metadata
                .path
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("--")
        );
        assert!(
            metadata
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("_metadata.jsonl")
        );
        assert_eq!(
            repo.list(&JsonlSessionListOptions::default())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn branch_and_tree_forks_match_v4_copy_scope() {
        let directory = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepo::new(directory.path());
        let cwd = directory.path().join("project");
        let source = repo
            .create(JsonlSessionCreateOptions {
                id: Some("source".to_string()),
                cwd: cwd.clone(),
                ..JsonlSessionCreateOptions::default()
            })
            .unwrap();
        let first = source
            .append_message(Message::User(UserMessage::text("first", 1)))
            .unwrap();
        source
            .append_message(Message::User(UserMessage::text("second", 2)))
            .unwrap();
        let branch = repo
            .fork(
                &source.metadata().unwrap(),
                &ForkOptions::Branch {
                    entry_id: Some(first),
                    position: Some(ForkPosition::At),
                },
                JsonlSessionCreateOptions {
                    id: Some("branch".to_string()),
                    cwd,
                    ..JsonlSessionCreateOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            branch
                .find_entries(crate::EntryQuery {
                    entry_type: Some(SessionEntryType::Message),
                    ..crate::EntryQuery::default()
                })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn timestamp_matches_javascript_iso_filename_shape() {
        assert_eq!(
            session_timestamp(1_767_225_600_000),
            "2026-01-01T00-00-00-000Z"
        );
    }
}
