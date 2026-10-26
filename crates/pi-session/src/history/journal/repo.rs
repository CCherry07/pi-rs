use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::history::{
    HistoryError, JsonlSessionMetadata, SESSION_SCHEMA_VERSION, SessionHeader, now_ms,
};

use super::jsonl::{validate_header, validate_header_json_shape};

const PARALLEL_LOOKUP_MIN_FILES: usize = 128;
const MAX_LOOKUP_WORKERS: usize = 4;

#[derive(Clone)]
pub struct JsonlSessionRepo {
    sessions_root: PathBuf,
}

/// A fresh UUID and its Pi-compatible location; allocation performs no writes
/// or history enumeration. The session still materializes its file lazily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionTarget {
    pub id: String,
    pub cwd: PathBuf,
    pub path: PathBuf,
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

    pub fn allocate_new(&self, cwd: impl AsRef<Path>) -> Result<NewSessionTarget, HistoryError> {
        self.new_target(
            absolute_path(cwd.as_ref())?,
            uuid::Uuid::now_v7().to_string(),
        )
    }

    /// Resolves Pi's project-scoped `--session-id` contract without writing
    /// a directory or session file for a missing ID.
    pub fn resolve_exact_id(
        &self,
        cwd: impl AsRef<Path>,
        id: &str,
    ) -> Result<ExactSessionIdResolution, HistoryError> {
        validate_session_id(id)?;
        let cwd = absolute_path(cwd.as_ref())?;
        let directory = self.session_directory(&cwd)?;
        if let Some(metadata) = find_exact_metadata(&directory, id)? {
            return Ok(ExactSessionIdResolution::Existing(metadata));
        }
        let NewSessionTarget { id, cwd, path } = self.new_target(cwd, id.to_string())?;
        Ok(ExactSessionIdResolution::New { id, cwd, path })
    }

    fn session_directory(&self, cwd: &Path) -> Result<PathBuf, HistoryError> {
        Ok(absolute_path(&self.sessions_root)?.join(jsonl_session_directory_name(cwd)))
    }

    fn new_target(&self, cwd: PathBuf, id: String) -> Result<NewSessionTarget, HistoryError> {
        let directory = self.session_directory(&cwd)?;
        let path = directory.join(format!("{}_{}.jsonl", session_timestamp(now_ms()), id));
        Ok(NewSessionTarget { id, cwd, path })
    }
}

fn find_exact_metadata(
    directory: &Path,
    id: &str,
) -> Result<Option<JsonlSessionMetadata>, HistoryError> {
    if !directory.exists() {
        return Ok(None);
    }
    let entries = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension() == Some(OsStr::new("jsonl")))
        .collect::<Vec<_>>();
    let workers = if entries.len() < PARALLEL_LOOKUP_MIN_FILES {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_LOOKUP_WORKERS)
    };
    latest_matching_entry(&entries, id, workers)
}

fn latest_matching_entry(
    entries: &[std::fs::DirEntry],
    id: &str,
    workers: usize,
) -> Result<Option<JsonlSessionMetadata>, HistoryError> {
    if entries.is_empty() {
        return Ok(None);
    }
    if workers <= 1 {
        return Ok(latest_in_chunk(entries, id));
    }
    std::thread::scope(|scope| {
        let mut chunks = entries.chunks(entries.len().div_ceil(workers));
        let first = chunks.next().expect("nonempty directory entries");
        let pending = chunks
            .map(|chunk| scope.spawn(move || latest_in_chunk(chunk, id)))
            .collect::<Vec<_>>();
        let mut latest = latest_in_chunk(first, id);
        // Join in directory order, not completion order, to preserve stable ties.
        for worker in pending {
            let candidate = worker.join().map_err(|_| {
                HistoryError::Storage("session header lookup worker panicked".into())
            })?;
            select_newer(&mut latest, candidate);
        }
        Ok(latest)
    })
}

fn latest_in_chunk(entries: &[std::fs::DirEntry], id: &str) -> Option<JsonlSessionMetadata> {
    let mut latest = None;
    for entry in entries {
        if !entry.file_type().is_ok_and(|kind| !kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        let Some(header) = read_header_for_listing(&path).filter(|header| header.id == id) else {
            continue;
        };
        let modified_at = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |duration| duration.as_secs_f64() * 1_000.0);
        select_newer(
            &mut latest,
            Some(JsonlSessionMetadata {
                id: header.id,
                created_at: header.created_at,
                cwd: header.cwd,
                path,
                modified_at,
                source_format: SESSION_SCHEMA_VERSION,
                parent_session_id: header.parent_session_id,
                metadata: header.metadata,
            }),
        );
    }
    latest
}

fn select_newer(
    latest: &mut Option<JsonlSessionMetadata>,
    candidate: Option<JsonlSessionMetadata>,
) {
    if let Some(candidate) = candidate
        && latest
            .as_ref()
            .is_none_or(|previous| candidate.modified_at > previous.modified_at)
    {
        *latest = Some(candidate);
    }
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

pub fn validate_session_id(id: &str) -> Result<(), HistoryError> {
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
        Err(HistoryError::InvalidPayload(
            "session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character"
                .to_string(),
        ))
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, HistoryError> {
    std::path::absolute(path).map_err(HistoryError::from)
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
    use crate::history::SessionStore;

    #[test]
    fn allocating_new_id_is_pure_and_uses_the_existing_project_layout() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        let repo = JsonlSessionRepo::new(&root);
        let cwd = directory.path().join("workspace");
        let first = repo.allocate_new(&cwd).unwrap();
        let second = repo.allocate_new(&cwd).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            uuid::Uuid::parse_str(&first.id).unwrap().get_version_num(),
            7
        );
        assert_eq!(first.cwd, std::path::absolute(cwd).unwrap());
        assert_eq!(first.path.parent(), second.path.parent());
        assert!(
            first
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(&format!("_{}.jsonl", first.id))
        );
        assert!(!root.exists());
    }

    #[test]
    fn serial_and_parallel_lookup_preserve_header_matching_and_stable_ties() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, UNIX_EPOCH};

        let directory = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepo::new(directory.path().join("sessions"));
        let cwd = directory.path().join("workspace");
        let root = repo.session_directory(&cwd).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let mut matching = Vec::new();
        for index in 0..132 {
            let path = root.join(format!("renamed-{index}.jsonl"));
            let id = if index == 5 || index == 130 {
                "wanted"
            } else {
                "other"
            };
            drop(SessionStore::create(&path, SessionHeader::new(id, &cwd)).unwrap());
            File::open(&path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(100)))
                .unwrap();
            if id == "wanted" {
                matching.push(path);
            }
        }
        std::fs::write(root.join("broken.jsonl"), "not JSON\n").unwrap();
        std::fs::create_dir(root.join("folder.jsonl")).unwrap();
        let entries = std::fs::read_dir(&root)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let first = entries
            .iter()
            .find(|entry| matching.contains(&entry.path()))
            .unwrap()
            .path();
        for workers in [1, 4] {
            assert_eq!(
                latest_matching_entry(&entries, "wanted", workers)
                    .unwrap()
                    .unwrap()
                    .path,
                first
            );
            assert!(
                latest_matching_entry(&entries, "missing", workers)
                    .unwrap()
                    .is_none()
            );
        }
        File::open(&matching[1])
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(200)))
            .unwrap();
        assert!(matches!(repo.resolve_exact_id(&cwd, "wanted").unwrap(),
            ExactSessionIdResolution::Existing(metadata) if metadata.path == matching[1]));
        std::fs::remove_file(&matching[1]).unwrap();
        for workers in [1, 4] {
            assert_eq!(
                latest_matching_entry(&entries, "wanted", workers)
                    .unwrap()
                    .unwrap()
                    .path,
                matching[0]
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn lookup_and_allocation_work_in_a_readonly_directory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepo::new(directory.path().join("sessions"));
        let target = repo.allocate_new(directory.path()).unwrap();
        drop(
            SessionStore::create(&target.path, SessionHeader::new(&target.id, &target.cwd))
                .unwrap(),
        );
        let parent = target.path.parent().unwrap();
        std::fs::set_permissions(&target.path, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(
            matches!(repo.resolve_exact_id(&target.cwd, &target.id).unwrap(),
            ExactSessionIdResolution::Existing(metadata) if metadata.path == target.path)
        );
        let fresh = repo.allocate_new(&target.cwd).unwrap();
        assert!(!fresh.path.exists());
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

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
        SessionStore::create(
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
