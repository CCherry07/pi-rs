use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use pi_core::IsolatedSessionOptions;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schedule::Schedule;
use crate::{Error, Result};

const MAX_JOBS: usize = 100;
const MAX_HISTORY: usize = 500;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Job {
    pub id: String,
    pub name: String,
    pub cwd: PathBuf,
    pub prompt: String,
    pub schedule: Schedule,
    pub options: IsolatedSessionOptions,
    pub enabled: bool,
    pub next_run_at: Option<i64>,
    pub manual_requested: bool,
    pub timeout_seconds: u64,
    pub max_runs: Option<u64>,
    pub runs: u64,
    pub notify: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Running,
    Completed,
    Failed,
    Aborted,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Run {
    pub id: String,
    pub job_id: String,
    pub cwd: PathBuf,
    pub scheduled_at: Option<i64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: Status,
    pub session_id: Option<String>,
    pub output: String,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Database {
    version: u32,
    jobs: Vec<Job>,
    runs: Vec<Run>,
}

impl Default for Database {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: Vec::new(),
            runs: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Store {
    root: PathBuf,
}

/// The OS lock is held for the *whole* attempt, independently of metadata
/// transactions. A crashed owner releases it automatically. Lock files are
/// never unlinked: another process may still have the original inode open.
pub(crate) struct Claim {
    pub job: Job,
    pub run: Run,
    _lock: File,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn list(&self, cwd: &Path) -> Result<Vec<Job>> {
        Ok(self
            .read()?
            .jobs
            .into_iter()
            .filter(|job| job.cwd == cwd)
            .collect())
    }

    pub fn history(&self, cwd: &Path, id: Option<&str>) -> Result<Vec<Run>> {
        Ok(self
            .read()?
            .runs
            .into_iter()
            .rev()
            .filter(|run| run.cwd == cwd && id.is_none_or(|id| run.job_id == id))
            .take(50)
            .collect())
    }

    pub fn create(&self, job: Job) -> Result<Job> {
        self.transaction(|db| {
            if db.jobs.len() >= MAX_JOBS {
                return Err(Error::Invalid(format!(
                    "schedule store is limited to {MAX_JOBS} jobs"
                )));
            }
            db.jobs.push(job.clone());
            Ok(job)
        })
    }

    pub fn change(&self, cwd: &Path, id: &str, action: &str, now: i64) -> Result<Job> {
        self.transaction(|db| {
            let index = db
                .jobs
                .iter()
                .position(|job| job.id == id && job.cwd == cwd)
                .ok_or_else(|| {
                    Error::Invalid(format!("unknown job in this working directory: {id}"))
                })?;
            let job = &mut db.jobs[index];
            match action {
                "pause" => {
                    job.enabled = false;
                    job.manual_requested = false;
                }
                "resume" => {
                    if job.max_runs.is_some_and(|max| job.runs >= max)
                        || (!job.schedule.recurring() && job.runs > 0)
                    {
                        return Err(Error::Invalid(
                            "job is exhausted; create a new schedule".into(),
                        ));
                    }
                    job.enabled = true;
                    job.next_run_at = Some(job.schedule.next(now)?);
                }
                "run_now" => {
                    if db
                        .runs
                        .iter()
                        .any(|run| run.job_id == id && run.status == Status::Running)
                    {
                        return Err(Error::Invalid(
                            "job already has an active attempt; inspect history".into(),
                        ));
                    }
                    job.manual_requested = true;
                }
                "remove" => return Ok(db.jobs.remove(index)),
                _ => return Err(Error::Invalid("unknown schedule action".into())),
            }
            Ok(job.clone())
        })
    }

    pub fn claim(&self, cwd: &Path, now: i64) -> Result<Option<Claim>> {
        if !self.root.join("jobs.json").exists() {
            return Ok(None);
        }
        self.transaction(|db| {
            // Recover only attempts whose OS lock is available. Never infer
            // death from a timestamp or lease timeout while a tool may run.
            for run in db
                .runs
                .iter_mut()
                .filter(|run| run.cwd == cwd && run.status == Status::Running)
            {
                if self.run_lock(&run.job_id)?.is_some() {
                    run.status = Status::Unknown;
                    run.finished_at = Some(now);
                    run.error = Some(
                        "execution owner stopped without a terminal record; not replayed".into(),
                    );
                }
            }
            let mut candidates = (0..db.jobs.len())
                .filter(|index| db.jobs[*index].cwd == cwd)
                .collect::<Vec<_>>();
            candidates.sort_by_key(|index| {
                let job = &db.jobs[*index];
                (
                    if job.manual_requested {
                        i64::MIN
                    } else {
                        job.next_run_at.unwrap_or(i64::MAX)
                    },
                    job.runs,
                )
            });
            for index in candidates {
                let job = &mut db.jobs[index];
                let due = job.enabled && job.next_run_at.is_some_and(|at| at <= now);
                if !due && !job.manual_requested {
                    continue;
                }
                let Some(lock) = self.run_lock(&job.id)? else {
                    continue;
                };
                let run = Run {
                    id: Uuid::now_v7().to_string(),
                    job_id: job.id.clone(),
                    cwd: job.cwd.clone(),
                    scheduled_at: if job.manual_requested {
                        None
                    } else {
                        job.next_run_at
                    },
                    started_at: now,
                    finished_at: None,
                    status: Status::Running,
                    session_id: None,
                    output: String::new(),
                    error: None,
                };
                job.manual_requested = false;
                job.runs = job
                    .runs
                    .checked_add(1)
                    .ok_or_else(|| Error::Invalid("run counter overflow".into()))?;
                // Persist consumption *before* launching a provider. Missed
                // windows coalesce; neither restart nor failure replays one.
                if !job.schedule.recurring() || job.max_runs.is_some_and(|max| job.runs >= max) {
                    job.enabled = false;
                    job.next_run_at = None;
                } else if due {
                    job.next_run_at = Some(job.schedule.next(now)?);
                }
                db.runs.push(run.clone());
                return Ok(Some(Claim {
                    job: job.clone(),
                    run,
                    _lock: lock,
                }));
            }
            Ok(None)
        })
    }

    pub fn finish(&self, mut run: Run) -> Result<()> {
        truncate(&mut run.output, MAX_OUTPUT_BYTES);
        if let Some(error) = &mut run.error {
            truncate(error, 4096);
        }
        self.transaction(|db| {
            // Unknown live outcomes can still have an unwinding child. Pause
            // atomically with the ledger update; removal during a run is fine.
            if run.status == Status::Unknown
                && let Some(job) = db.jobs.iter_mut().find(|job| job.id == run.job_id)
            {
                job.enabled = false;
                job.manual_requested = false;
            }
            let saved = db
                .runs
                .iter_mut()
                .find(|saved| saved.id == run.id)
                .ok_or_else(|| Error::Invalid("execution record disappeared".into()))?;
            if saved.status != Status::Running {
                return Err(Error::Invalid(
                    "execution already has a terminal record".into(),
                ));
            }
            *saved = run;
            Ok(())
        })
    }

    fn run_lock(&self, id: &str) -> Result<Option<File>> {
        Uuid::parse_str(id).map_err(|_| Error::Invalid("invalid job identity".into()))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(format!("{id}.lock")))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(file)),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn read(&self) -> Result<Database> {
        let bytes = match std::fs::read(self.root.join("jobs.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Database::default());
            }
            Err(error) => return Err(error.into()),
        };
        let db: Database = serde_json::from_slice(&bytes)?;
        if db.version != 1 {
            return Err(Error::Invalid(format!(
                "unsupported schedule store version {}",
                db.version
            )));
        }
        Ok(db)
    }

    fn transaction<T>(&self, operation: impl FnOnce(&mut Database) -> Result<T>) -> Result<T> {
        std::fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("store.lock"))?;
        // Short metadata transactions never hold this lock during agent work.
        lock.lock_exclusive()?;
        let mut db = self.read()?;
        let previous = serde_json::to_vec(&db)?;
        let result = operation(&mut db)?;
        let terminal = db
            .runs
            .iter()
            .filter(|run| run.status != Status::Running)
            .count();
        let mut to_remove = terminal.saturating_sub(MAX_HISTORY);
        db.runs.retain(|run| {
            if to_remove > 0 && run.status != Status::Running {
                to_remove -= 1;
                false
            } else {
                true
            }
        });
        let bytes = serde_json::to_vec(&db)?;
        if bytes == previous {
            return Ok(result);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(&bytes)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join("jobs.json"))
            .map_err(|error| Error::Io(error.error))?;
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(result)
    }
}

fn truncate(value: &mut String, maximum: usize) {
    if value.len() > maximum {
        let mut end = maximum;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push_str("\n[output truncated]");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(cwd: &Path) -> Job {
        Job {
            id: Uuid::now_v7().to_string(),
            name: "check".into(),
            cwd: cwd.into(),
            prompt: "check".into(),
            schedule: Schedule::Interval { milliseconds: 1000 },
            options: IsolatedSessionOptions::default(),
            enabled: true,
            next_run_at: Some(1000),
            manual_requested: false,
            timeout_seconds: 60,
            max_runs: None,
            runs: 0,
            notify: true,
        }
    }

    #[test]
    fn multiple_instances_claim_once_and_crash_does_not_replay() {
        let directory = tempfile::tempdir().unwrap();
        let a = Store::new(directory.path().join("schedule"));
        let b = a.clone();
        a.create(job(directory.path())).unwrap();
        let claim = a.claim(directory.path(), 1000).unwrap().unwrap();
        assert!(b.claim(directory.path(), 1000).unwrap().is_none());
        assert!(b.claim(directory.path(), 5000).unwrap().is_none());
        assert_eq!(
            b.history(directory.path(), None).unwrap()[0].status,
            Status::Running
        );
        drop(claim);
        let next = b.claim(directory.path(), 5000).unwrap().unwrap();
        assert_eq!(next.run.scheduled_at, Some(2000));
        assert_eq!(
            b.history(directory.path(), None).unwrap()[1].status,
            Status::Unknown
        );
        let mut run = next.run.clone();
        run.status = Status::Completed;
        run.finished_at = Some(5001);
        b.finish(run).unwrap();
        drop(next);
        assert!(a.claim(directory.path(), 5001).unwrap().is_none());
    }

    #[test]
    fn one_shot_is_consumed_before_dispatch_and_history_survives_removal() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().join("schedule"));
        assert!(store.list(directory.path()).unwrap().is_empty());
        assert!(!store.root.exists());
        let mut once = job(directory.path());
        once.schedule = Schedule::Once { at: 1000 };
        let id = once.id.clone();
        store.create(once).unwrap();
        let claim = store.claim(directory.path(), 9000).unwrap().unwrap();
        assert!(!store.list(directory.path()).unwrap()[0].enabled);
        drop(claim);
        assert!(store.claim(directory.path(), 20_000).unwrap().is_none());
        assert_eq!(
            store.history(directory.path(), None).unwrap()[0].status,
            Status::Unknown
        );
        store
            .change(directory.path(), &id, "remove", 20_001)
            .unwrap();
        assert_eq!(store.history(directory.path(), Some(&id)).unwrap().len(), 1);
    }

    #[test]
    fn pause_resume_manual_run_and_cwd_isolation() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().join("schedule"));
        let job = store.create(job(directory.path())).unwrap();
        store
            .change(directory.path(), &job.id, "pause", 1000)
            .unwrap();
        assert!(store.claim(directory.path(), 3000).unwrap().is_none());
        store
            .change(directory.path(), &job.id, "run_now", 3000)
            .unwrap();
        assert!(
            store
                .claim(&directory.path().join("other"), 3000)
                .unwrap()
                .is_none()
        );
        let claim = store.claim(directory.path(), 3000).unwrap().unwrap();
        assert_eq!(claim.run.scheduled_at, None);
        assert!(!claim.job.enabled);
        drop(claim);
        store.claim(directory.path(), 4000).unwrap();
        let resumed = store
            .change(directory.path(), &job.id, "resume", 4000)
            .unwrap();
        assert_eq!(resumed.next_run_at, Some(5000));
    }
}
