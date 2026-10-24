use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pi_core::{ContentBlock, Message, ModelId, ModelSpec, ProviderId, ThinkingLevel};
use pi_plugin::CommandSpec;
use pi_session::{
    aggregate_document_usage, current_session_context_tokens, AgentSession,
    AgentSessionReplacement, AgentSessionSnapshot, ExactSessionIdResolution, ForkPosition,
    JsonlSessionRepo, MultiSessionManager, PiSession, SessionDocument, SessionEntry,
    SessionObservation, SessionUpdate,
};

pub(crate) struct SessionScope {
    key: String,
    workspace: pi_core::WorkspaceSpec,
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
}
impl SessionScope {
    pub(crate) fn project(project: &pi_sdk::projects::Project) -> Result<Self, String> {
        Ok(Self {
            key: format!("project:{}", project.id),
            workspace: project.resolve()?,
            metadata: Some(project.session_metadata()),
        })
    }
    pub(crate) fn directory(path: &Path) -> Result<Self, String> {
        let path = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        Ok(Self {
            key: format!("cwd:{}", path.display()),
            workspace: pi_core::WorkspaceSpec::from_cwd(path),
            metadata: None,
        })
    }
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    manager: MultiSessionManager,
    agent_dir: PathBuf,
    sessions: Arc<RwLock<HashMap<String, PiSession>>>,
    drafts: Arc<tokio::sync::Mutex<HashMap<String, PiSession>>>,
    open_gate: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct StoredIsolatedSession {
    pub(crate) document: SessionDocument,
    pub(crate) parent_thread_id: String,
    pub(crate) agent: String,
    pub(crate) nickname: Option<String>,
}

pub(crate) struct LiveSession {
    observation: SessionObservation,
    pub(crate) snapshot: AgentSessionSnapshot,
}

impl LiveSession {
    pub(crate) async fn recv(&mut self) -> Option<SessionUpdate> {
        let update = self.observation.recv().await?;
        if let SessionUpdate::Reset { snapshot, .. } = &update {
            self.snapshot = (**snapshot).clone();
        }
        Some(update)
    }

    pub(crate) fn refresh_snapshot(&mut self) {
        // Keep queued lifecycle, error and notice events while refreshing message data.
        self.snapshot = self.session().snapshot();
    }

    pub(crate) fn session(&self) -> Arc<AgentSession> {
        self.observation.current()
    }

    pub(crate) fn cwd(&self) -> PathBuf {
        self.session().catalog().cwd().to_path_buf()
    }

    pub(crate) fn token_usage(&self) -> SessionTokenUsage {
        token_usage(&self.session())
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionTokenUsage {
    pub(crate) total_tokens: Option<u64>,
    pub(crate) context_tokens: Option<u64>,
    pub(crate) model_context_window: Option<u64>,
}

pub(crate) fn token_usage(session: &AgentSession) -> SessionTokenUsage {
    let model_context_window = session.active_context_window();
    let Ok(document) = session.history().load() else {
        return SessionTokenUsage {
            model_context_window,
            ..SessionTokenUsage::default()
        };
    };
    document_token_usage(&document, model_context_window)
}

pub(crate) fn document_token_usage(
    document: &SessionDocument,
    model_context_window: Option<u64>,
) -> SessionTokenUsage {
    let total_tokens = Some(aggregate_document_usage(document).total_tokens);
    let context_tokens = document.context().ok().and_then(|context| {
        let branch = document
            .branch()
            .ok()?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        current_session_context_tokens(&branch, &context.messages).map(|usage| usage.tokens)
    });
    SessionTokenUsage {
        total_tokens,
        context_tokens,
        model_context_window,
    }
}

pub(crate) struct SessionModelCatalog {
    pub(crate) models: Vec<ModelSpec>,
    pub(crate) selected_provider: ProviderId,
    pub(crate) selected_model: ModelId,
    pub(crate) selected_thinking: ThinkingLevel,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionSummary {
    pub(crate) project_id: Option<String>,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) cwd: PathBuf,
    pub(crate) updated_at_ms: i64,
    pub(crate) model: Option<String>,
    pub(crate) message_count: usize,
}

impl SessionStore {
    pub(crate) fn new(manager: MultiSessionManager, agent_dir: PathBuf) -> Self {
        Self {
            manager,
            agent_dir,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            drafts: Arc::default(),
            open_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) async fn create(&self, cwd: &Path) -> Result<Arc<AgentSession>, String> {
        self.create_scoped(SessionScope::directory(cwd)?).await
    }

    pub(crate) async fn create_scoped(
        &self,
        scope: SessionScope,
    ) -> Result<Arc<AgentSession>, String> {
        Ok(self.create_handle(&scope).await?.current())
    }

    #[cfg(test)]
    pub(crate) async fn prepare_thread(&self, cwd: &Path) -> Result<Arc<AgentSession>, String> {
        self.prepare_scoped(SessionScope::directory(cwd)?).await
    }

    pub(crate) async fn prepare_scoped(
        &self,
        scope: SessionScope,
    ) -> Result<Arc<AgentSession>, String> {
        let mut drafts = self.drafts.lock().await;
        self.prepare_locked(scope, &mut drafts).await
    }

    async fn prepare_locked(
        &self,
        scope: SessionScope,
        drafts: &mut HashMap<String, PiSession>,
    ) -> Result<Arc<AgentSession>, String> {
        if let Some(session) = drafts.get(&scope.key) {
            if session.current().catalog().workspace().spec() == &scope.workspace {
                return Ok(session.current());
            }
        }
        // Prepare first; failure retains the previous usable draft.
        let session = self.create_handle(&scope).await?;
        if let Some(previous) = drafts.insert(scope.key, session.clone()) {
            self.manager
                .close_session(&previous)
                .await
                .map_err(|error| error.to_string())?;
            self.sessions
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|_, handle| handle.id() != previous.id());
        }
        Ok(session.current())
    }

    pub(crate) async fn refresh_project_draft(
        &self,
        project: &pi_sdk::projects::Project,
    ) -> Result<(), String> {
        let key = format!("project:{}", project.id);
        if self.drafts.lock().await.contains_key(&key) {
            self.prepare_scoped(SessionScope::project(project)?).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn reload_prepared(&self, cwd: &Path) -> Result<Arc<AgentSession>, String> {
        self.reload_scoped(SessionScope::directory(cwd)?).await
    }

    pub(crate) async fn reload_scoped(
        &self,
        scope: SessionScope,
    ) -> Result<Arc<AgentSession>, String> {
        let current = self.prepare_scoped(scope).await?;
        self.reload(&current.history().header().id).await
    }

    pub(crate) async fn reload(&self, id: &str) -> Result<Arc<AgentSession>, String> {
        self.open(id).await?;
        let (_, session) = self
            .handle(id)
            .ok_or_else(|| format!("unknown Pi session: {id}"))?;
        session.reload().await.map_err(|error| error.to_string())?;
        Ok(session.current())
    }

    #[cfg(test)]
    pub(crate) async fn start_thread(&self, cwd: &Path) -> Result<Arc<AgentSession>, String> {
        self.start_scoped(SessionScope::directory(cwd)?).await
    }

    pub(crate) async fn start_scoped(
        &self,
        scope: SessionScope,
    ) -> Result<Arc<AgentSession>, String> {
        let mut drafts = self.drafts.lock().await;
        let current = self.prepare_locked(scope, &mut drafts).await?;
        drafts.retain(|_, session| session.id() != current.history().header().id);
        Ok(current)
    }

    async fn create_handle(&self, scope: &SessionScope) -> Result<PiSession, String> {
        let session_id = uuid::Uuid::now_v7().to_string();
        let path = match JsonlSessionRepo::new(self.agent_dir.join("sessions"))
            .resolve_exact_id(scope.workspace.cwd(), &session_id)
            .map_err(|error| error.to_string())?
        {
            ExactSessionIdResolution::New { path, .. } => path,
            ExactSessionIdResolution::Existing(_) => {
                return Err(format!("generated duplicate Pi session id: {session_id}"));
            }
        };
        let session = self
            .manager
            .create_session(pi_session::CreateSessionOptions {
                workspace: scope.workspace.clone(),
                path,
                session_id: Some(session_id),
                metadata: scope.metadata.clone(),
                generation_overlay: Default::default(),
            })
            .await
            .map_err(|error| error.to_string())?;
        self.sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(uuid::Uuid::now_v7().to_string(), session.clone());
        Ok(session)
    }

    pub(crate) async fn command_catalog(&self, id: &str) -> Result<Vec<CommandSpec>, String> {
        Ok(self.open(id).await?.catalog().command_specs())
    }

    // Keys are stable frontend-handle identities, not replaceable JSONL IDs.
    pub(super) fn handle(&self, id: &str) -> Option<(String, PiSession)> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(_, session)| session.id() == id)
            .map(|(key, session)| (key.clone(), session.clone()))
    }

    /// Read an existing handle only; unlike open(), never resumes or constructs a session.
    pub(crate) fn existing_native_plugin_ids(
        &self,
        cwd: &Path,
        id: &str,
    ) -> Result<Vec<String>, String> {
        let (_, handle) = self
            .handle(id)
            .ok_or_else(|| format!("Pi session is not open: {id}"))?;
        let current = handle.current();
        if current.history().header().id != id {
            return Err(format!("Pi session was replaced: {id}"));
        }
        let cwd = std::fs::canonicalize(cwd).map_err(|error| error.to_string())?;
        let session_cwd =
            std::fs::canonicalize(current.catalog().cwd()).map_err(|error| error.to_string())?;
        if cwd != session_cwd {
            return Err("Selected thread does not belong to the selected workspace".into());
        }
        Ok(current
            .runtime_inventory()
            .configured_native_plugins()
            .iter()
            .map(ToString::to_string)
            .collect())
    }

    pub(crate) fn forwarder_key(&self, id: &str) -> Result<String, String> {
        self.handle(id)
            .map(|(key, _)| key)
            .ok_or_else(|| format!("unknown Pi session: {id}"))
    }

    #[cfg(test)]
    pub(crate) async fn model_catalog(
        &self,
        cwd: &Path,
        thread_id: Option<&str>,
    ) -> Result<SessionModelCatalog, String> {
        self.model_catalog_scoped(SessionScope::directory(cwd)?, thread_id)
            .await
    }

    pub(crate) async fn model_catalog_scoped(
        &self,
        scope: SessionScope,
        thread_id: Option<&str>,
    ) -> Result<SessionModelCatalog, String> {
        let session = match thread_id {
            Some(id) => {
                // Wait for an already requested resume, without opening a session
                // merely to populate a model selector.
                let _open = self.open_gate.lock().await;
                let (_, handle) = self
                    .handle(id)
                    .ok_or_else(|| format!("Pi session is not open: {id}"))?;
                let session = handle.current();
                let header = session.history().header();
                let project_id = pi_sdk::projects::session_project_id(header.metadata.as_ref());
                let expected = pi_sdk::projects::session_project_id(scope.metadata.as_ref());
                let matches_scope = match (project_id, expected) {
                    (Some(actual), Some(expected)) => actual == expected,
                    _ => session.catalog().cwd() == scope.workspace.cwd(),
                };
                if header.id != id || !matches_scope {
                    return Err("Selected thread does not belong to the selected workspace".into());
                }
                session
            }
            None => self.prepare_scoped(scope).await?,
        };
        let state = session.snapshot().agent;
        let catalog = SessionModelCatalog {
            models: session.catalog().available_models(),
            selected_provider: state.provider_id,
            selected_model: state.model_id,
            selected_thinking: state.thinking_level,
        };
        Ok(catalog)
    }

    pub(crate) async fn open(&self, id: &str) -> Result<Arc<AgentSession>, String> {
        let _open = self.open_gate.lock().await;
        if let Some((_, session)) = self.handle(id) {
            return Ok(session.current());
        }
        let path = self.find_path(id)?;
        let session = self
            .manager
            .open_session(path, Default::default())
            .await
            .map_err(|error| error.to_string())?;
        let current = session.current();
        self.sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(uuid::Uuid::now_v7().to_string(), session);
        Ok(current)
    }

    pub(crate) async fn abort(&self, id: &str) -> Result<(), String> {
        self.open(id).await?.abort();
        Ok(())
    }

    pub(crate) async fn fork(
        &self,
        id: &str,
        entry_id: impl Into<String>,
    ) -> Result<Arc<AgentSession>, String> {
        let _ = self.open(id).await?;
        let (_, session) = self
            .handle(id)
            .ok_or_else(|| format!("unknown Pi session: {id}"))?;
        let replacement = session
            .fork_session(entry_id, ForkPosition::At)
            .await
            .map_err(|error| error.to_string())?;
        if replacement == AgentSessionReplacement::Cancelled {
            return Err("Pi session fork was cancelled by a session plugin".to_string());
        }
        Ok(session.current())
    }

    pub(crate) async fn rename(&self, id: &str, name: String) -> Result<(), String> {
        if let Ok(session) = self.open(id).await {
            return session
                .set_name(Some(name))
                .await
                .map_err(|error| error.to_string());
        }
        let path = self.find_path(id)?;
        let (log, _) = pi_session::SessionStore::open(path).map_err(|error| error.to_string())?;
        log.set_name(Some(name)).map_err(|error| error.to_string())
    }

    pub(crate) async fn archive(&self, id: &str) -> Result<(), String> {
        let path = self.close_and_resolve_path(id, false).await?;
        let sessions_dir = self.agent_dir.join("sessions");
        let relative = path.strip_prefix(&sessions_dir).map_err(|_| {
            format!(
                "session path {} is outside {}",
                path.display(),
                sessions_dir.display()
            )
        })?;
        let destination = sessions_dir.join("archived").join(relative);
        let destination_dir = destination
            .parent()
            .ok_or_else(|| format!("invalid archive path: {}", destination.display()))?;
        std::fs::create_dir_all(destination_dir).map_err(|error| {
            format!(
                "cannot create archive {}: {error}",
                destination_dir.display()
            )
        })?;
        move_session_with_companion(&path, &destination, "archive")
    }

    pub(crate) async fn unarchive(&self, id: &str) -> Result<(), String> {
        let path = self.find_archived_path(id)?;
        let sessions_dir = self.agent_dir.join("sessions");
        let archive_dir = sessions_dir.join("archived");
        let relative = path.strip_prefix(&archive_dir).map_err(|_| {
            format!(
                "archived session path {} is outside {}",
                path.display(),
                archive_dir.display()
            )
        })?;
        let destination = sessions_dir.join(relative);
        if destination.exists() {
            return Err(format!(
                "cannot restore archived session {} because {} already exists",
                path.display(),
                destination.display()
            ));
        }
        let destination_dir = destination
            .parent()
            .ok_or_else(|| format!("invalid restored session path: {}", destination.display()))?;
        std::fs::create_dir_all(destination_dir).map_err(|error| {
            format!(
                "cannot create restored session directory {}: {error}",
                destination_dir.display()
            )
        })?;
        move_session_with_companion(&path, &destination, "restore archived")
    }

    pub(crate) async fn delete(&self, id: &str) -> Result<(), String> {
        let path = self.close_and_resolve_path(id, true).await?;
        let companion = session_companion_directory(&path);
        if companion.exists() {
            std::fs::remove_dir_all(&companion).map_err(|error| {
                format!(
                    "cannot delete child sessions {}: {error}",
                    companion.display()
                )
            })?;
        }
        std::fs::remove_file(&path)
            .map_err(|error| format!("cannot delete session {}: {error}", path.display()))
    }

    async fn close_and_resolve_path(
        &self,
        id: &str,
        include_archived: bool,
    ) -> Result<PathBuf, String> {
        let key = self.handle(id).map(|(key, _)| key);
        let active = key.and_then(|key| {
            self.sessions
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key)
        });
        let path = active
            .as_ref()
            .map(|session| session.current().history().path().to_path_buf())
            .map_or_else(|| self.find_path_with_archived(id, include_archived), Ok)?;
        if let Some(session) = active {
            self.manager
                .close_session(&session)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(path)
    }

    pub(crate) fn subscribe(&self, id: &str) -> Result<LiveSession, String> {
        let (_, handle) = self
            .handle(id)
            .ok_or_else(|| format!("unknown Pi session: {id}"))?;
        let observation = handle.observe();
        Ok(LiveSession {
            snapshot: observation.snapshot().clone(),
            observation,
        })
    }

    pub(crate) fn read_isolated(&self, id: &str) -> Result<StoredIsolatedSession, String> {
        let path = all_session_files(&self.agent_dir.join("sessions"))?
            .into_iter()
            .find(|path| path_is_isolated(path) && session_id_matches(path, id))
            .ok_or_else(|| format!("unknown isolated Pi session: {id}"))?;
        let document = pi_session::SessionStore::read(&path).map_err(|error| error.to_string())?;
        let owner_path = isolated_owner_path(&path)
            .ok_or_else(|| format!("invalid isolated session path: {}", path.display()))?;
        let owner = pi_session::SessionStore::read(&owner_path).ok();
        let parent_thread_id = document
            .isolated_parent_session_id()
            .map_err(|error| error.to_string())?
            .or_else(|| owner.as_ref().map(|owner| owner.header.id.clone()))
            .ok_or_else(|| {
                format!(
                    "cannot resolve isolated session owner {}",
                    owner_path.display()
                )
            })?;
        Ok(StoredIsolatedSession {
            document,
            parent_thread_id,
            agent: "agent".to_string(),
            nickname: None,
        })
    }

    /// Read the current branch without creating or promoting a managed session.
    pub(crate) fn document(&self, id: &str) -> Result<SessionDocument, String> {
        if let Some((_, session)) = self.handle(id) {
            return session
                .current()
                .history()
                .load()
                .map_err(|error| error.to_string());
        }
        if let Ok(stored) = self.read_isolated(id) {
            return Ok(stored.document);
        }
        pi_session::SessionStore::read(self.find_path(id)?).map_err(|error| error.to_string())
    }

    pub(crate) fn list_project(
        &self,
        project: &pi_sdk::projects::Project,
        projects: &[pi_sdk::projects::Project],
        archived: bool,
    ) -> Result<Vec<SessionSummary>, String> {
        let mut summaries = Vec::new();
        for path in session_files(&self.agent_dir.join("sessions"))? {
            if path_is_archived(&path) != archived {
                continue;
            }
            let Ok(summary) = session_summary(&path) else {
                continue;
            };
            let belongs = match &summary.project_id {
                Some(id) => id == &project.id,
                None => pi_sdk::projects::project_for_legacy_cwd(projects, &summary.cwd)
                    .is_some_and(|candidate| candidate.id == project.id),
            };
            if belongs {
                summaries.push(summary);
            }
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at_ms));
        Ok(summaries)
    }

    #[cfg(test)]
    pub(crate) fn list(&self, cwd: &Path) -> Result<Vec<SessionSummary>, String> {
        self.list_by_archive_state(cwd, false)
    }

    #[cfg(test)]
    pub(crate) fn list_archived(&self, cwd: &Path) -> Result<Vec<SessionSummary>, String> {
        self.list_by_archive_state(cwd, true)
    }

    pub(crate) fn summary(&self, id: &str) -> Result<SessionSummary, String> {
        let path = self.find_path(id)?;
        session_summary(&path)
    }

    #[cfg(test)]
    fn list_by_archive_state(
        &self,
        cwd: &Path,
        archived: bool,
    ) -> Result<Vec<SessionSummary>, String> {
        let cwd = std::fs::canonicalize(cwd)
            .map_err(|error| format!("cannot access workspace {}: {error}", cwd.display()))?;
        let mut summaries = session_files(&self.agent_dir.join("sessions"))?
            .into_iter()
            .filter_map(|path| {
                if path_is_archived(&path) != archived {
                    return None;
                }
                let summary = session_summary(&path).ok()?;
                if summary.cwd != cwd {
                    return None;
                }
                Some(summary)
            })
            .collect::<Vec<_>>();
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at_ms));
        Ok(summaries)
    }

    fn find_path(&self, id: &str) -> Result<PathBuf, String> {
        self.find_path_with_archived(id, false)
    }

    fn find_archived_path(&self, id: &str) -> Result<PathBuf, String> {
        session_files(&self.agent_dir.join("sessions"))?
            .into_iter()
            .find(|path| path_is_archived(path) && session_id_matches(path, id))
            .ok_or_else(|| format!("unknown archived Pi session: {id}"))
    }

    fn find_path_with_archived(&self, id: &str, include_archived: bool) -> Result<PathBuf, String> {
        session_files(&self.agent_dir.join("sessions"))?
            .into_iter()
            .find(|path| {
                (include_archived || !path_is_archived(path)) && session_id_matches(path, id)
            })
            .ok_or_else(|| format!("unknown Pi session: {id}"))
    }
}

fn session_summary(path: &Path) -> Result<SessionSummary, String> {
    let document = pi_session::SessionStore::read(path).map_err(|error| error.to_string())?;
    let id = document.header.id.clone();
    let updated_at_ms = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default();
    let model = document.entries.iter().rev().find_map(|record| {
        let SessionEntry::Message(entry) = &record.entry else {
            return None;
        };
        match entry.message.as_standard()? {
            Message::Assistant(message) => Some(message.model.as_str().to_string()),
            _ => None,
        }
    });
    let messages = document
        .entries
        .iter()
        .filter_map(|record| {
            let SessionEntry::Message(entry) = &record.entry else {
                return None;
            };
            entry.message.as_standard().map(|_| &entry.message)
        })
        .collect::<Vec<_>>();
    let title = document.name.unwrap_or_else(|| first_user_title(&messages));
    let message_count = messages.len();
    Ok(SessionSummary {
        project_id: pi_sdk::projects::session_project_id(document.header.metadata.as_ref())
            .map(str::to_string),
        id,
        title,
        cwd: document.header.cwd,
        updated_at_ms,
        model,
        message_count,
    })
}

fn path_is_archived(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == "archived")
}

fn path_is_isolated(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == "isolated")
}

fn isolated_owner_path(path: &Path) -> Option<PathBuf> {
    let isolated = path.parent()?;
    if isolated.file_name()? != "isolated" {
        return None;
    }
    Some(isolated.parent()?.with_extension("jsonl"))
}

fn move_session_with_companion(
    source: &Path,
    destination: &Path,
    operation: &str,
) -> Result<(), String> {
    let source_companion = session_companion_directory(source);
    let destination_companion = session_companion_directory(destination);
    let moved_companion = source_companion.exists();
    if moved_companion {
        if destination_companion.exists() {
            return Err(format!(
                "cannot {operation} child sessions because {} already exists",
                destination_companion.display()
            ));
        }
        std::fs::rename(&source_companion, &destination_companion).map_err(|error| {
            format!(
                "cannot {operation} child sessions {} to {}: {error}",
                source_companion.display(),
                destination_companion.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(source, destination) {
        if moved_companion {
            let _ = std::fs::rename(&destination_companion, &source_companion);
        }
        return Err(format!(
            "cannot {operation} session {} to {}: {error}",
            source.display(),
            destination.display()
        ));
    }
    Ok(())
}

fn session_companion_directory(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("session"));
    path.parent().unwrap_or_else(|| Path::new(".")).join(stem)
}

fn session_id_matches(path: &Path, id: &str) -> bool {
    pi_session::SessionStore::read(path).is_ok_and(|document| document.header.id == id)
}

fn session_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    collect_session_files(root, false)
}

fn all_session_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    collect_session_files(root, true)
}

fn collect_session_files(root: &Path, include_isolated: bool) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot read sessions {}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                if !include_isolated && path.file_name().is_some_and(|name| name == "isolated") {
                    continue;
                }
                pending.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn first_user_title(messages: &[&pi_session::AgentMessage]) -> String {
    let text = messages
        .iter()
        .find_map(|message| match message.as_standard() {
            Some(Message::User(user)) => Some(
                message
                    .display_text()
                    .map(str::to_string)
                    .unwrap_or_else(|| content_text(&user.content)),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "Untitled session".to_string();
    }
    let mut chars = collapsed.chars();
    let title = chars.by_ref().take(80).collect::<String>();
    if chars.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

pub(crate) fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            ContentBlock::Thinking(thinking) => Some(thinking.thinking.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
