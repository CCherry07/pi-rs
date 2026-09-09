use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pi_core::{
    CommandSpec, ContentBlock, IsolatedSessionId, Message, ModelId, ModelSpec, ProviderId,
    ThinkingLevel,
};
use pi_session::{
    aggregate_document_usage, current_session_context_tokens, AgentSession,
    AgentSessionReplacement, AgentSessionSnapshot, AgentSessionSubscription,
    ExactSessionIdResolution, ForkPosition, IsolatedSessionObservation, JsonlSessionRepo,
    MultiSessionManager, PiSession, SessionDocument, SessionEntry, SessionLog,
};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct SessionStore {
    manager: MultiSessionManager,
    agent_dir: PathBuf,
    sessions: Arc<RwLock<HashMap<String, PiSession>>>,
    observed_isolated: Arc<RwLock<HashMap<String, ObservedIsolatedSession>>>,
}

#[derive(Clone)]
pub(crate) struct ObservedIsolatedSession {
    pub(crate) observation: IsolatedSessionObservation,
    pub(crate) parent_thread_id: String,
    pub(crate) agent: String,
}

pub(crate) struct StoredIsolatedSession {
    pub(crate) document: SessionDocument,
    pub(crate) parent_thread_id: String,
    pub(crate) agent: String,
}

pub(crate) struct LiveSession {
    source: LiveSessionSource,
    pub(crate) subscription: AgentSessionSubscription,
}

enum LiveSessionSource {
    Primary(Arc<AgentSession>),
    Isolated(IsolatedSessionObservation),
}

impl LiveSession {
    pub(crate) fn snapshot(&self) -> AgentSessionSnapshot {
        match &self.source {
            LiveSessionSource::Primary(session) => session.snapshot(),
            LiveSessionSource::Isolated(observation) => observation.snapshot(),
        }
    }

    pub(crate) fn cwd(&self) -> PathBuf {
        match &self.source {
            LiveSessionSource::Primary(session) => session.runtime().cwd().to_path_buf(),
            LiveSessionSource::Isolated(observation) => observation.cwd(),
        }
    }

    pub(crate) fn token_usage(&self) -> SessionTokenUsage {
        match &self.source {
            LiveSessionSource::Primary(session) => token_usage(session),
            LiveSessionSource::Isolated(_) => SessionTokenUsage {
                total_tokens: None,
                context_tokens: None,
                model_context_window: None,
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionTokenUsage {
    pub(crate) total_tokens: Option<u64>,
    pub(crate) context_tokens: Option<u64>,
    pub(crate) model_context_window: Option<u64>,
}

pub(crate) fn token_usage(session: &AgentSession) -> SessionTokenUsage {
    let model_context_window = session.active_context_window();
    let document = session.log().load().ok();
    let total_tokens = document
        .as_ref()
        .map(|document| aggregate_document_usage(document).total_tokens);
    let context_tokens = document.as_ref().and_then(|document| {
        let branch = document
            .branch()
            .ok()?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let context = document.context().ok()?;
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
            observed_isolated: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) async fn create(&self, cwd: &Path) -> Result<Arc<AgentSession>, String> {
        let cwd = std::fs::canonicalize(cwd)
            .map_err(|error| format!("cannot access workspace {}: {error}", cwd.display()))?;
        if !cwd.is_dir() {
            return Err(format!("workspace is not a directory: {}", cwd.display()));
        }
        let session_id = uuid::Uuid::now_v7().to_string();
        let path = match JsonlSessionRepo::new(self.agent_dir.join("sessions"))
            .resolve_exact_id(&cwd, &session_id)
            .map_err(|error| error.to_string())?
        {
            ExactSessionIdResolution::New { path, .. } => path,
            ExactSessionIdResolution::Existing(_) => {
                return Err(format!("generated duplicate Pi session id: {session_id}"));
            }
        };
        let session = self
            .manager
            .create_session_with_id(cwd, path, session_id)
            .await
            .map_err(|error| error.to_string())?;
        let current = session.current();
        self.sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.id(), session);
        Ok(current)
    }

    pub(crate) async fn command_catalog(&self, cwd: &Path) -> Result<Vec<CommandSpec>, String> {
        let cwd = std::fs::canonicalize(cwd)
            .map_err(|error| format!("cannot access workspace {}: {error}", cwd.display()))?;
        let path = self
            .agent_dir
            .join("sessions")
            .join(format!("{}.jsonl", uuid::Uuid::now_v7()));
        let handle = self
            .manager
            .create_session(cwd, path)
            .await
            .map_err(|error| error.to_string())?;
        let commands = handle.current().runtime().command_specs();
        self.manager
            .close_session(&handle)
            .await
            .map_err(|error| error.to_string())?;
        Ok(commands)
    }

    pub(crate) async fn model_catalog(&self, cwd: &Path) -> Result<SessionModelCatalog, String> {
        let cwd = std::fs::canonicalize(cwd)
            .map_err(|error| format!("cannot access workspace {}: {error}", cwd.display()))?;
        let path = self
            .agent_dir
            .join("sessions")
            .join(format!("{}.jsonl", uuid::Uuid::now_v7()));
        let handle = self
            .manager
            .create_session(cwd, path)
            .await
            .map_err(|error| error.to_string())?;
        let session = handle.current();
        let state = session.runtime().agent().state();
        let catalog = SessionModelCatalog {
            models: session.runtime().available_models(),
            selected_provider: state.provider_id,
            selected_model: state.model_id,
            selected_thinking: state.thinking_level,
        };
        self.manager
            .close_session(&handle)
            .await
            .map_err(|error| error.to_string())?;
        Ok(catalog)
    }

    pub(crate) async fn open(&self, id: &str) -> Result<Arc<AgentSession>, String> {
        if let Some(session) = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
        {
            return Ok(session.current());
        }
        let path = self.find_path(id)?;
        let session = self
            .manager
            .open_session(path)
            .await
            .map_err(|error| error.to_string())?;
        let current = session.current();
        self.sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.id(), session);
        Ok(current)
    }

    pub(crate) async fn fork(&self, id: &str) -> Result<Arc<AgentSession>, String> {
        let _ = self.open(id).await?;
        let session = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown Pi session: {id}"))?;
        let document =
            SessionLog::read(session.current().log().path()).map_err(|error| error.to_string())?;
        let entry_id = document
            .entries
            .iter()
            .rev()
            .find(|record| {
                matches!(
                    &record.entry,
                    SessionEntry::Message(entry)
                        if matches!(
                            entry.message.as_standard(),
                            Some(Message::User(_) | Message::Assistant(_))
                        )
                )
            })
            .map(|record| record.id.clone())
            .ok_or_else(|| "the Pi session has no message to fork".to_string())?;
        let replacement = session
            .fork_session(entry_id, ForkPosition::At)
            .await
            .map_err(|error| error.to_string())?;
        if replacement == AgentSessionReplacement::Cancelled {
            return Err("Pi session fork was cancelled by a session plugin".to_string());
        }
        let new_id = session.id();
        let current = session.current();
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.remove(id);
        sessions.insert(new_id, session);
        Ok(current)
    }

    pub(crate) async fn rename(&self, id: &str, name: String) -> Result<(), String> {
        if let Ok(session) = self.open(id).await {
            return session
                .set_name(Some(name))
                .await
                .map_err(|error| error.to_string());
        }
        let path = self.find_path(id)?;
        let (log, _) = SessionLog::open(path).map_err(|error| error.to_string())?;
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
        let active = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        let path = active
            .as_ref()
            .map(|session| session.current().log().path().to_path_buf())
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
        let session = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown Pi session: {id}"))?
            .current();
        Ok(LiveSession {
            subscription: session.subscribe(),
            source: LiveSessionSource::Primary(session),
        })
    }

    pub(crate) fn subscribe_isolated(
        &self,
        owner_id: &str,
        isolated_id: &str,
        agent: &str,
    ) -> Result<(IsolatedSessionObservation, LiveSession), String> {
        let id = IsolatedSessionId::new(isolated_id.to_string());
        let observation = if let Some(owner) = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(owner_id)
            .cloned()
        {
            owner
                .observe_isolated_session(&id)
                .map_err(|error| error.to_string())?
        } else if let Some(owner) = self
            .observed_isolated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(owner_id)
            .cloned()
        {
            owner.observation.observe_isolated_session(&id)?
        } else {
            return Err(format!("unknown Pi session: {owner_id}"));
        };
        let session_id = observation.session_id();
        self.observed_isolated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session_id,
                ObservedIsolatedSession {
                    observation: observation.clone(),
                    parent_thread_id: owner_id.to_string(),
                    agent: agent.to_string(),
                },
            );
        let live = LiveSession {
            subscription: observation.subscribe(),
            source: LiveSessionSource::Isolated(observation.clone()),
        };
        Ok((observation, live))
    }

    pub(crate) fn observed_isolated(&self, id: &str) -> Option<ObservedIsolatedSession> {
        self.observed_isolated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    pub(crate) fn read_isolated(&self, id: &str) -> Result<StoredIsolatedSession, String> {
        let path = all_session_files(&self.agent_dir.join("sessions"))?
            .into_iter()
            .find(|path| path_is_isolated(path) && session_id_matches(path, id))
            .ok_or_else(|| format!("unknown isolated Pi session: {id}"))?;
        let document = SessionLog::read(&path).map_err(|error| error.to_string())?;
        let owner_path = isolated_owner_path(&path)
            .ok_or_else(|| format!("invalid isolated session path: {}", path.display()))?;
        let owner = SessionLog::read(&owner_path).map_err(|error| {
            format!(
                "cannot read isolated session owner {}: {error}",
                owner_path.display()
            )
        })?;
        let agent = subagent_role_for_child(&owner, id).unwrap_or_else(|| "subagent".to_string());
        Ok(StoredIsolatedSession {
            document,
            parent_thread_id: owner.header.id,
            agent,
        })
    }

    pub(crate) fn list(&self, cwd: &Path) -> Result<Vec<SessionSummary>, String> {
        self.list_by_archive_state(cwd, false)
    }

    pub(crate) fn list_archived(&self, cwd: &Path) -> Result<Vec<SessionSummary>, String> {
        self.list_by_archive_state(cwd, true)
    }

    pub(crate) fn summary(&self, id: &str) -> Result<SessionSummary, String> {
        let path = self.find_path(id)?;
        session_summary(&path)
    }

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
    let document = SessionLog::read(path).map_err(|error| error.to_string())?;
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
            entry.message.as_standard()
        })
        .collect::<Vec<_>>();
    let title = document.name.unwrap_or_else(|| first_user_title(&messages));
    let message_count = messages.len();
    Ok(SessionSummary {
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

fn subagent_role_for_child(document: &SessionDocument, child_id: &str) -> Option<String> {
    let mut pending = HashMap::<String, Value>::new();
    for record in &document.entries {
        let SessionEntry::Message(entry) = &record.entry else {
            continue;
        };
        let Some(message) = entry.message.as_standard() else {
            continue;
        };
        match message {
            Message::Assistant(message) => {
                for block in &message.content {
                    let ContentBlock::ToolCall(call) = block else {
                        continue;
                    };
                    if call.name == "subagent" {
                        pending.insert(call.id.to_string(), call.arguments.clone());
                    }
                }
            }
            Message::ToolResult(result) => {
                let matches_child = result
                    .details
                    .as_ref()
                    .and_then(|details| details.get("sessionId"))
                    .and_then(Value::as_str)
                    == Some(child_id);
                if matches_child {
                    return result
                        .details
                        .as_ref()
                        .and_then(|details| details.get("agent"))
                        .and_then(Value::as_str)
                        .or_else(|| {
                            pending
                                .get(result.tool_call_id.as_str())
                                .and_then(|args| args.get("agent"))
                                .and_then(Value::as_str)
                        })
                        .map(str::to_string);
                }
            }
            _ => {}
        }
    }
    None
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
    SessionLog::read(path).is_ok_and(|document| document.header.id == id)
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

fn first_user_title(messages: &[&Message]) -> String {
    let text = messages
        .iter()
        .find_map(|message| match message {
            Message::User(message) => Some(content_text(&message.content)),
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
