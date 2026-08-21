use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

const TRUST_REQUIRING_PI_RESOURCES: &[&str] = &[
    "settings.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DefaultProjectTrust {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectTrustUpdate {
    pub path: PathBuf,
    pub decision: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectTrustOption {
    pub label: String,
    pub trusted: bool,
    pub updates: Vec<ProjectTrustUpdate>,
    pub saved_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectTrustEvaluation {
    Known(bool),
    Ask(Vec<ProjectTrustOption>),
}

#[derive(Debug)]
pub struct ProjectTrustPromptRequest {
    pub cwd: PathBuf,
    pub options: Vec<ProjectTrustOption>,
    pub response: oneshot::Sender<Option<usize>>,
}

#[derive(Debug, Error)]
pub enum ProjectTrustError {
    #[error("cannot access project path {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot access trust store {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid trust store {path}: {message}")]
    InvalidStore { path: PathBuf, message: String },
    #[error("project trust prompt is unavailable")]
    PromptUnavailable,
}

#[derive(Clone)]
pub struct ProjectTrustService {
    store: ProjectTrustStore,
    trust_override: Option<bool>,
    default_trust: DefaultProjectTrust,
    interactive: bool,
    decisions: Arc<Mutex<HashMap<PathBuf, bool>>>,
    prompt_sender: mpsc::UnboundedSender<ProjectTrustPromptRequest>,
}

impl ProjectTrustService {
    pub fn new(
        agent_dir: &Path,
        trust_override: Option<bool>,
        interactive: bool,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ProjectTrustPromptRequest>), ProjectTrustError> {
        let (prompt_sender, prompt_receiver) = mpsc::unbounded_channel();
        Ok((
            Self {
                store: ProjectTrustStore::new(agent_dir),
                trust_override,
                default_trust: load_default_project_trust(agent_dir)?,
                interactive,
                decisions: Arc::new(Mutex::new(HashMap::new())),
                prompt_sender,
            },
            prompt_receiver,
        ))
    }

    pub fn evaluate(&self, cwd: &Path) -> Result<ProjectTrustEvaluation, ProjectTrustError> {
        let cwd = normalize_path(cwd)?;
        if let Some(decision) = self.trust_override {
            return Ok(ProjectTrustEvaluation::Known(decision));
        }
        if let Some(decision) = self
            .decisions
            .lock()
            .expect("trust decisions poisoned")
            .get(&cwd)
        {
            return Ok(ProjectTrustEvaluation::Known(*decision));
        }
        if !has_trust_requiring_project_resources(&cwd)? {
            return Ok(ProjectTrustEvaluation::Known(true));
        }
        if let Some(entry) = self.store.get_entry(&cwd)? {
            return Ok(ProjectTrustEvaluation::Known(entry.decision));
        }
        match self.default_trust {
            DefaultProjectTrust::Always => Ok(ProjectTrustEvaluation::Known(true)),
            DefaultProjectTrust::Never => Ok(ProjectTrustEvaluation::Known(false)),
            DefaultProjectTrust::Ask if !self.interactive => {
                Ok(ProjectTrustEvaluation::Known(false))
            }
            DefaultProjectTrust::Ask => Ok(ProjectTrustEvaluation::Ask(project_trust_options(
                &cwd, true,
            )?)),
        }
    }

    pub fn remember(&self, cwd: &Path, trusted: bool) -> Result<(), ProjectTrustError> {
        let cwd = normalize_path(cwd)?;
        self.decisions
            .lock()
            .expect("trust decisions poisoned")
            .insert(cwd, trusted);
        Ok(())
    }

    pub fn apply_option(
        &self,
        cwd: &Path,
        option: &ProjectTrustOption,
    ) -> Result<bool, ProjectTrustError> {
        if !option.updates.is_empty() {
            self.store.set_many(&option.updates)?;
        }
        self.remember(cwd, option.trusted)?;
        Ok(option.trusted)
    }

    pub fn manual_options(&self, cwd: &Path) -> Result<Vec<ProjectTrustOption>, ProjectTrustError> {
        project_trust_options(cwd, false)
    }

    pub async fn resolve(&self, cwd: &Path) -> Result<bool, ProjectTrustError> {
        match self.evaluate(cwd)? {
            ProjectTrustEvaluation::Known(trusted) => {
                self.remember(cwd, trusted)?;
                Ok(trusted)
            }
            ProjectTrustEvaluation::Ask(options) => {
                let (response, receiver) = oneshot::channel();
                self.prompt_sender
                    .send(ProjectTrustPromptRequest {
                        cwd: normalize_path(cwd)?,
                        options: options.clone(),
                        response,
                    })
                    .map_err(|_| ProjectTrustError::PromptUnavailable)?;
                let selected = receiver
                    .await
                    .map_err(|_| ProjectTrustError::PromptUnavailable)?;
                let trusted = match selected.and_then(|index| options.get(index)) {
                    Some(option) => self.apply_option(cwd, option)?,
                    None => false,
                };
                self.remember(cwd, trusted)?;
                Ok(trusted)
            }
        }
    }
}

#[derive(Clone)]
struct ProjectTrustStore {
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectTrustStoreEntry {
    path: PathBuf,
    decision: bool,
}

impl ProjectTrustStore {
    fn new(agent_dir: &Path) -> Self {
        Self {
            path: agent_dir.join("trust.json"),
        }
    }

    fn get_entry(&self, cwd: &Path) -> Result<Option<ProjectTrustStoreEntry>, ProjectTrustError> {
        self.with_lock(|| {
            let data = read_trust_file(&self.path)?;
            let mut current = normalize_path(cwd)?;
            loop {
                if let Some(Some(decision)) = data.get(&current.to_string_lossy().to_string()) {
                    return Ok(Some(ProjectTrustStoreEntry {
                        path: current,
                        decision: *decision,
                    }));
                }
                if !current.pop() {
                    return Ok(None);
                }
            }
        })
    }

    fn set_many(&self, updates: &[ProjectTrustUpdate]) -> Result<(), ProjectTrustError> {
        self.with_lock(|| {
            let mut data = read_trust_file(&self.path)?;
            for update in updates {
                let key = normalize_path(&update.path)?.to_string_lossy().to_string();
                if let Some(decision) = update.decision {
                    data.insert(key, Some(decision));
                } else {
                    data.remove(&key);
                }
            }
            let json = serde_json::to_string_pretty(&data).map_err(|error| {
                ProjectTrustError::InvalidStore {
                    path: self.path.clone(),
                    message: error.to_string(),
                }
            })?;
            write_trust_file(&self.path, &json)?;
            Ok(())
        })
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, ProjectTrustError>,
    ) -> Result<T, ProjectTrustError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| ProjectTrustError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let lock_name = self.path.file_name().map_or_else(
            || "trust.json.lock".into(),
            |name| format!("{}.lock", name.to_string_lossy()),
        );
        let lock_path = parent.join(lock_name);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| ProjectTrustError::Io {
                path: lock_path.clone(),
                source,
            })?;
        file.lock_exclusive()
            .map_err(|source| ProjectTrustError::Io {
                path: lock_path.clone(),
                source,
            })?;
        let result = operation();
        let unlock_result = FileExt::unlock(&file).map_err(|source| ProjectTrustError::Io {
            path: lock_path,
            source,
        });
        let value = result?;
        unlock_result?;
        Ok(value)
    }
}

fn read_trust_file(path: &Path) -> Result<BTreeMap<String, Option<bool>>, ProjectTrustError> {
    let json = match fs::read_to_string(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(source) => {
            return Err(ProjectTrustError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if json.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(&json).map_err(|error| ProjectTrustError::InvalidStore {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn write_trust_file(path: &Path, json: &str) -> Result<(), ProjectTrustError> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|source| ProjectTrustError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(json.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .map_err(|source| ProjectTrustError::Io {
            path: path.to_path_buf(),
            source,
        })
}

pub fn has_trust_requiring_project_resources(cwd: &Path) -> Result<bool, ProjectTrustError> {
    let cwd = normalize_path(cwd)?;
    if TRUST_REQUIRING_PI_RESOURCES
        .iter()
        .any(|entry| cwd.join(".pi").join(entry).exists())
    {
        return Ok(true);
    }

    let user_skills = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| normalize_path(&home.join(".agents/skills")).ok());
    for ancestor in cwd.ancestors() {
        let skills = ancestor.join(".agents/skills");
        if skills.exists()
            && user_skills.as_ref().is_none_or(|user_skills| {
                normalize_path(&skills).ok().as_ref() != Some(user_skills)
            })
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn project_trust_options(
    cwd: &Path,
    include_session_only: bool,
) -> Result<Vec<ProjectTrustOption>, ProjectTrustError> {
    let cwd = normalize_path(cwd)?;
    let mut options = vec![ProjectTrustOption {
        label: "Trust".to_string(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: cwd.clone(),
            decision: Some(true),
        }],
        saved_path: Some(cwd.clone()),
    }];
    if let Some(parent) = cwd.parent() {
        options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({})", parent.display()),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent.to_path_buf(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: cwd.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent.to_path_buf()),
        });
    }
    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Trust (this session only)".to_string(),
            trusted: true,
            updates: Vec::new(),
            saved_path: None,
        });
    }
    options.push(ProjectTrustOption {
        label: "Do not trust".to_string(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: cwd.clone(),
            decision: Some(false),
        }],
        saved_path: Some(cwd.clone()),
    });
    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Do not trust (this session only)".to_string(),
            trusted: false,
            updates: Vec::new(),
            saved_path: None,
        });
    }
    Ok(options)
}

fn normalize_path(path: &Path) -> Result<PathBuf, ProjectTrustError> {
    fs::canonicalize(path).map_err(|source| ProjectTrustError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrustSettings {
    default_project_trust: Option<String>,
}

fn load_default_project_trust(agent_dir: &Path) -> Result<DefaultProjectTrust, ProjectTrustError> {
    let path = agent_dir.join("settings.json");
    let json = match fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DefaultProjectTrust::Ask);
        }
        Err(source) => return Err(ProjectTrustError::Io { path, source }),
    };
    let Ok(settings) = serde_json::from_str::<TrustSettings>(&json) else {
        return Ok(DefaultProjectTrust::Ask);
    };
    match settings.default_project_trust.as_deref().unwrap_or("ask") {
        "ask" => Ok(DefaultProjectTrust::Ask),
        "always" => Ok(DefaultProjectTrust::Always),
        "never" => Ok(DefaultProjectTrust::Never),
        _ => Ok(DefaultProjectTrust::Ask),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(root: &Path) -> ProjectTrustService {
        ProjectTrustService::new(&root.join("agent"), None, true)
            .unwrap()
            .0
    }

    #[test]
    fn nearest_saved_ancestor_decision_wins() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("project");
        let child = parent.join("packages/app");
        fs::create_dir_all(child.join(".pi/skills")).unwrap();
        let service = service(root.path());
        service
            .store
            .set_many(&[
                ProjectTrustUpdate {
                    path: parent.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: child.clone(),
                    decision: Some(false),
                },
            ])
            .unwrap();

        assert_eq!(
            service.evaluate(&child).unwrap(),
            ProjectTrustEvaluation::Known(false)
        );
        assert_eq!(
            service.evaluate(&parent).unwrap(),
            ProjectTrustEvaluation::Known(true)
        );
    }

    #[test]
    fn trust_parent_removes_a_child_override() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("project");
        let child = parent.join("app");
        fs::create_dir_all(child.join(".pi/skills")).unwrap();
        let service = service(root.path());
        service.store.set(&child, Some(false)).unwrap();
        let option = project_trust_options(&child, true)
            .unwrap()
            .into_iter()
            .find(|option| option.label.starts_with("Trust parent"))
            .unwrap();

        service.apply_option(&child, &option).unwrap();

        assert_eq!(
            service.store.get_entry(&child).unwrap().unwrap().path,
            fs::canonicalize(parent).unwrap()
        );
        assert!(service.store.get_entry(&child).unwrap().unwrap().decision);
    }

    #[test]
    fn bare_pi_directory_does_not_require_trust() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".pi")).unwrap();
        assert!(!has_trust_requiring_project_resources(root.path()).unwrap());
        fs::write(root.path().join(".pi/settings.json"), "{}").unwrap();
        assert!(has_trust_requiring_project_resources(root.path()).unwrap());
    }

    #[test]
    fn ancestor_agents_skills_require_trust() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("a/b");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(root.path().join(".agents/skills/example")).unwrap();
        assert!(has_trust_requiring_project_resources(&child).unwrap());
    }

    #[test]
    fn noninteractive_ask_defaults_to_untrusted() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("project/.pi/skills")).unwrap();
        let (service, _) =
            ProjectTrustService::new(&root.path().join("agent"), None, false).unwrap();

        assert_eq!(
            service.evaluate(&root.path().join("project")).unwrap(),
            ProjectTrustEvaluation::Known(false)
        );
    }

    #[test]
    fn global_default_project_trust_matches_pi_values() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let project = root.path().join("project");
        fs::create_dir_all(project.join(".pi/skills")).unwrap();
        fs::create_dir_all(&agent).unwrap();
        fs::write(
            agent.join("settings.json"),
            r#"{"defaultProjectTrust":"always"}"#,
        )
        .unwrap();

        let (service, _) = ProjectTrustService::new(&agent, None, true).unwrap();

        assert_eq!(
            service.evaluate(&project).unwrap(),
            ProjectTrustEvaluation::Known(true)
        );
    }

    #[test]
    fn null_store_entries_are_accepted_but_do_not_decide() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let project = root.path().join("project");
        fs::create_dir_all(project.join(".pi/skills")).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let project = fs::canonicalize(project).unwrap();
        fs::write(
            agent.join("trust.json"),
            format!("{{\n  {:?}: null\n}}\n", project.to_string_lossy()),
        )
        .unwrap();

        let (service, _) = ProjectTrustService::new(&agent, None, true).unwrap();

        assert!(matches!(
            service.evaluate(&project).unwrap(),
            ProjectTrustEvaluation::Ask(_)
        ));
    }

    #[tokio::test]
    async fn unresolved_runtime_cwd_waits_for_the_tui_decision_broker() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir_all(project.join(".pi/skills")).unwrap();
        let (service, mut requests) =
            ProjectTrustService::new(&root.path().join("agent"), None, true).unwrap();
        let resolver = service.clone();
        let resolved_project = project.clone();
        let resolution = tokio::spawn(async move { resolver.resolve(&resolved_project).await });
        let request = requests.recv().await.unwrap();
        assert_eq!(request.cwd, fs::canonicalize(&project).unwrap());

        request.response.send(Some(2)).unwrap();

        assert!(resolution.await.unwrap().unwrap());
        assert_eq!(
            service.evaluate(&project).unwrap(),
            ProjectTrustEvaluation::Known(true)
        );
        assert!(!root.path().join("agent/trust.json").exists());
    }

    impl ProjectTrustStore {
        fn set(&self, cwd: &Path, decision: Option<bool>) -> Result<(), ProjectTrustError> {
            self.set_many(&[ProjectTrustUpdate {
                path: cwd.to_path_buf(),
                decision,
            }])
        }
    }
}
