//! Hermes skill-library maintenance. Policy and durable state stay in this package.
//! Baseline: NousResearch/hermes-agent e629c900, agent/curator.py and tools/skill_usage.py.
mod archive;
pub(crate) mod metadata;
pub(crate) mod runtime;
pub(crate) mod scope;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use metadata::{Metadata, State, atomic_json, content_lock};
pub(crate) use runtime::{Worker, register, start_worker};

const DAY: i64 = 86_400_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Config {
    pub enabled: bool,
    pub interval_hours: u64,
    pub min_idle_hours: u64,
    pub stale_after_days: u64,
    pub archive_after_days: u64,
    pub consolidate: bool,
    pub max_input_tokens: u64,
    pub timeout_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: 168,
            min_idle_hours: 2,
            stale_after_days: 30,
            archive_after_days: 90,
            consolidate: false,
            max_input_tokens: 200_000,
            timeout_seconds: 120,
        }
    }
}

impl Config {
    pub fn validate(&self) -> io::Result<()> {
        if self.interval_hours == 0
            || self.stale_after_days == 0
            || self.archive_after_days < self.stale_after_days
            || self.max_input_tokens == 0
            || self.timeout_seconds == 0
            || [
                self.interval_hours,
                self.min_idle_hours,
                self.archive_after_days,
            ]
            .iter()
            .any(|v| *v > 1_000_000)
        {
            return Err(io::Error::other(
                "Invalid curator intervals or execution budget",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct SchedulerState {
    pub last_run_at: Option<i64>,
    pub last_activity_at: Option<i64>,
    pub paused: bool,
    pub run_count: u64,
    pub last_report: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Row {
    pub name: String,
    pub metadata: Option<Metadata>,
    pub protected_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    MarkStale,
    Reactivate,
    Archive,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlannedAction {
    pub name: String,
    pub action: Action,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct Report {
    pub dry_run: bool,
    pub planned: Vec<PlannedAction>,
    pub completed: Vec<PlannedAction>,
    pub skipped: Vec<String>,
    pub backup: Option<String>,
    pub review: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Curator {
    pub root: PathBuf,
    pub config: Config,
}

impl Curator {
    pub fn new(root: PathBuf, config: Config) -> Self {
        Self { root, config }
    }

    pub fn rows(&self) -> io::Result<Vec<Row>> {
        metadata::check_path(&self.root)?;
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if !entry.path().join("SKILL.md").exists() {
                continue;
            }
            let metadata = Metadata::read(&entry.path()).ok();
            let reason = match &metadata {
                None => Some("No valid provenance"),
                Some(m) if !m.curator_managed => Some("User-managed"),
                Some(m) if m.pinned => Some("Pinned"),
                Some(m) if !m.unchanged(&entry.path()) => {
                    Some("Externally changed or unsafe package")
                }
                Some(_) => None,
            };
            rows.push(Row {
                name,
                metadata,
                protected_reason: reason.map(str::to_string),
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    pub fn plan(&self, now: i64) -> io::Result<Report> {
        self.config.validate()?;
        let mut report = Report::default();
        for row in self.rows()? {
            if let Some(reason) = row.protected_reason {
                report.skipped.push(format!("{}: {reason}", row.name));
                continue;
            }
            let m = row.metadata.expect("eligible row");
            let age = now.saturating_sub(m.last_activity_at.max(m.created_at));
            let action = if age >= self.config.archive_after_days as i64 * DAY {
                Some(Action::Archive)
            } else if age >= self.config.stale_after_days as i64 * DAY && m.state == State::Active {
                Some(Action::MarkStale)
            } else if age < self.config.stale_after_days as i64 * DAY && m.state == State::Stale {
                Some(Action::Reactivate)
            } else {
                None
            };
            if let Some(action) = action {
                report.planned.push(PlannedAction {
                    name: row.name,
                    action,
                });
            }
        }
        Ok(report)
    }

    pub fn prune(&self, now: i64, dry_run: bool) -> io::Result<Report> {
        if dry_run {
            return Ok(Report {
                dry_run: true,
                ..self.plan(now)?
            });
        }
        let _content = content_lock(&self.root)?;
        let _store = metadata::lock(&self.root, ".skill-store.lock", true)?;
        let mut report = self.plan(now)?;
        if !report.planned.is_empty() {
            report.backup = Some(self.backup_locked()?);
        }
        for planned in &report.planned {
            let directory = self.root.join(&planned.name);
            let mut m = Metadata::read(&directory)?;
            if !m.writable(&directory) {
                report
                    .skipped
                    .push(format!("{}: changed since planning", planned.name));
                continue;
            }
            match planned.action {
                Action::Archive => {
                    self.archive_locked(&planned.name, "inactivity", None)?;
                }
                Action::MarkStale => {
                    m.state = State::Stale;
                    m.save(&directory)?;
                }
                Action::Reactivate => {
                    m.state = State::Active;
                    m.save(&directory)?;
                }
            }
            report.completed.push(PlannedAction {
                name: planned.name.clone(),
                action: planned.action,
            });
        }
        Ok(report)
    }

    pub fn state(&self) -> io::Result<SchedulerState> {
        let path = self.root.join(".curator-state.json");
        metadata::check_path(&path)?;
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SchedulerState::default()),
            Err(e) => Err(e),
        }
    }

    pub fn update_state(&self, f: impl FnOnce(&mut SchedulerState)) -> io::Result<()> {
        let _lock = metadata::lock(&self.root, ".curator-state.lock", true)?;
        let mut state = self.state()?;
        f(&mut state);
        atomic_json(&self.root.join(".curator-state.json"), &state)
    }

    pub fn due(&self, now: i64) -> io::Result<bool> {
        self.config.validate()?;
        if !self.config.enabled {
            return Ok(false);
        }
        let _lock = metadata::lock(&self.root, ".curator-state.lock", true)?;
        let mut state = self.state()?;
        if state.paused {
            return Ok(false);
        }
        let Some(last) = state.last_run_at else {
            state.last_run_at = Some(now);
            state.last_activity_at.get_or_insert(now);
            atomic_json(&self.root.join(".curator-state.json"), &state)?;
            return Ok(false);
        };
        Ok(
            now.saturating_sub(last) >= self.config.interval_hours as i64 * 3_600_000
                && now.saturating_sub(state.last_activity_at.unwrap_or(now))
                    >= self.config.min_idle_hours as i64 * 3_600_000,
        )
    }

    pub fn finish(&self, report: &Report, now: i64) -> io::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        atomic_json(
            &self.root.join(".reports").join(format!("{id}.json")),
            report,
        )?;
        self.update_state(|s| {
            s.last_run_at = Some(now);
            s.run_count = s.run_count.saturating_add(1);
            s.last_report = Some(id);
        })
    }

    pub fn adopt(&self, skill: &str) -> io::Result<()> {
        let _lock = content_lock(&self.root)?;
        let directory = self.root.join(metadata::name(skill)?);
        let mut m = match Metadata::read(&directory) {
            Ok(m) => m,
            Err(_) => Metadata::new(
                &directory,
                false,
                "user",
                chrono::Utc::now().timestamp_millis(),
            )?,
        };
        if m.pinned {
            return Err(io::Error::other(
                "Unpin the Skill before adopting a changed version",
            ));
        }
        m.curator_managed = true;
        m.files = metadata::fingerprint(&directory)?;
        m.last_activity_at = chrono::Utc::now().timestamp_millis();
        m.state = State::Active;
        m.save(&directory)
    }

    pub fn pin(&self, skill: &str, value: bool) -> io::Result<()> {
        let _lock = content_lock(&self.root)?;
        let directory = self.root.join(metadata::name(skill)?);
        let mut m = Metadata::read(&directory)?;
        m.pinned = value;
        m.save(&directory)
    }
}

/// A thin CLI adapter can finish local commands without a model or a session.
/// None means the registered /curator command must execute a model-backed run.
pub fn execute_local(
    agent_dir: &Path,
    cwd: &Path,
    project_trusted: bool,
    arguments: &[String],
) -> Result<Option<String>, String> {
    let config = crate::config::HermesMemoryConfig::load_profile(agent_dir);
    let targets = scope::Target::discover(
        config.global_dir(agent_dir).join("skills"),
        cwd,
        project_trusted,
    );
    let selected = scope::select(&targets, arguments.iter().map(String::as_str))?;
    if selected
        .iter()
        .any(|(_, command)| command.needs_model(&config.curator))
    {
        return Ok(None);
    }
    let results = selected
        .into_iter()
        .map(|(target, command)| {
            runtime::local(&target, &config.curator, &command)
                .map(|result| (target.key, result))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    scope::output(results).map(Some)
}

/// SDK adapter: explicit invocation is reported by the catalog owner; the
/// provider owns persistence. Neither the catalog nor memory-loader knows policy.
pub fn activity_observer(
    roots: Vec<PathBuf>,
) -> std::sync::Arc<dyn pi_plugin_skills::SkillActivityObserver> {
    std::sync::Arc::new(ActivityObserver(roots))
}

struct ActivityObserver(Vec<PathBuf>);
fn owns_path(root: &Path, directory: &Path) -> bool {
    let Some(root) = root.canonicalize().ok() else {
        return false;
    };
    directory
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .as_ref()
        == Some(&root)
}
pub(crate) fn observe_read(root: &Path, path: &Path) {
    if path.file_name().is_none_or(|name| name != "SKILL.md") {
        return;
    }
    let Some(directory) = path.parent() else {
        return;
    };
    if !owns_path(root, directory) {
        return;
    }
    let _ = (|| -> io::Result<()> {
        let _lock = content_lock(root)?;
        let mut m = Metadata::read(directory)?;
        m.view_count = m.view_count.saturating_add(1);
        m.last_activity_at = chrono::Utc::now().timestamp_millis();
        m.save(directory)
    })();
}

impl pi_plugin_skills::SkillActivityObserver for ActivityObserver {
    fn used(&self, skill: &pi_plugin_skills::SkillInfo) {
        let Some(directory) = skill.file_path.parent() else {
            return;
        };
        let Some(root) = self.0.iter().find(|root| owns_path(root, directory)) else {
            return;
        };
        let _ = (|| -> io::Result<()> {
            let _lock = content_lock(root)?;
            let mut m = Metadata::read(directory)?;
            m.use_count = m.use_count.saturating_add(1);
            m.last_activity_at = chrono::Utc::now().timestamp_millis();
            m.save(directory)
        })();
    }
}
