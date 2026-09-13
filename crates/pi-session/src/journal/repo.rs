use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::{JsonlSessionMetadata, SESSION_SCHEMA_VERSION, SessionError, SessionHeader, now_ms};

use super::jsonl::{validate_header, validate_header_json_shape};

#[derive(Clone)]
pub struct JsonlSessionRepo {
    sessions_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExactSessionIdResolution {
    Existing(JsonlSessionMetadata),
    New {
        id: String,
        cwd: PathBuf,
        path: PathBuf,
    },
}

impl JsonlSessionRepo {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
        }
    }

    /// Resolves Pi's project-scoped `--session-id` contract without writing
    /// a directory or session file for a missing ID.
    pub fn resolve_exact_id(
        &self,
        cwd: impl AsRef<Path>,
        id: &str,
    ) -> Result<ExactSessionIdResolution, SessionError> {
        validate_session_id(id)?;
        let cwd = absolute_path(cwd.as_ref())?;
        if let Some(metadata) = list_jsonl_session_metadata(&self.sessions_root, Some(&cwd))?
            .into_iter()
            .find(|metadata| metadata.id == id)
        {
            return Ok(ExactSessionIdResolution::Existing(metadata));
        }

        let directory = self.session_directory(&cwd)?;
        let path = directory.join(format!("{}_{}.jsonl", session_timestamp(now_ms()), id));
        Ok(ExactSessionIdResolution::New {
            id: id.to_string(),
            cwd,
            path,
        })
    }

    fn session_directory(&self, cwd: &Path) -> Result<PathBuf, SessionError> {
        Ok(absolute_path(&self.sessions_root)?.join(jsonl_session_directory_name(cwd)))
    }
}

fn list_jsonl_session_metadata(
    sessions_root: &Path,
    cwd: Option<&Path>,
) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
    let root = absolute_path(sessions_root)?;
    let directories = match cwd {
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

pub(crate) fn validate_session_id(id: &str) -> Result<(), SessionError> {
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
    use super::*;
    use crate::SessionLog;

    #[test]
    fn exact_id_resolution_is_project_scoped_and_does_not_materialize_new_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepo::new(directory.path().join("sessions"));
        let first_project = directory.path().join("workspace/first");
        let second_project = directory.path().join("workspace/second");

        let new = repo
            .resolve_exact_id(&first_project, "botmux-session")
            .unwrap();
        let ExactSessionIdResolution::New { id, cwd, path } = new else {
            panic!("missing id should resolve to a deferred new session");
        };
        assert_eq!(id, "botmux-session");
        assert_eq!(cwd, std::path::absolute(&first_project).unwrap());
        assert!(path.to_string_lossy().ends_with("_botmux-session.jsonl"));
        assert!(!path.exists());
        assert!(!directory.path().join("sessions").exists());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let existing_path = path.clone();
        SessionLog::create(
            &existing_path,
            SessionHeader::new(
                "botmux-session",
                std::path::absolute(&first_project).unwrap(),
            ),
        )
        .unwrap();
        assert!(matches!(
            repo.resolve_exact_id(&first_project, "botmux-session")
                .unwrap(),
            ExactSessionIdResolution::Existing(metadata) if metadata.path == existing_path
        ));
        assert!(matches!(
            repo.resolve_exact_id(&second_project, "botmux-session")
                .unwrap(),
            ExactSessionIdResolution::New { .. }
        ));
    }

    #[test]
    fn timestamp_matches_javascript_iso_filename_shape() {
        assert_eq!(
            session_timestamp(1_767_225_600_000),
            "2026-01-01T00-00-00-000Z"
        );
    }
}
