#![forbid(unsafe_code)]

//! ACP stable-v1 frontend for Pi sessions.
//!
//! The adapter owns ACP connection/session policy and delegates durable
//! conversation state to [`pi_session`]. Per-session MCP servers are adapted
//! through [`pi_mcp`] and injected with a transient generation overlay.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_client_protocol::schema::{ProtocolVersion, v1 as acp};
use agent_client_protocol::{
    Agent, ConnectTo, ConnectionTo, Responder, Stdio, on_receive_notification, on_receive_request,
};
use pi_agent::AgentLoopStop;
use pi_core::{
    AgentEvent, AssistantMessageEvent, ContentBlock as PiContentBlock, Message, ModelId,
    ProviderId, TextContent as PiTextContent, ThinkingLevel, UserMessage,
};
use pi_mcp::{McpServerConfig, McpToolSet};
use pi_session::{
    AgentSessionEvent, MultiSessionManager, PiSession, SessionEntry, SessionGenerationOverlay,
    SessionHeader,
};
use tokio::sync::Mutex;

const MODEL_CONFIG_ID: &str = "model";
const THINKING_CONFIG_ID: &str = "thought_level";
const LIST_PAGE_SIZE: usize = 100;

/// Process-level ACP configuration.
#[derive(Debug, Clone)]
pub struct AcpOptions {
    pub sessions_dir: PathBuf,
    pub agent_name: String,
    pub agent_version: String,
}

impl AcpOptions {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
            agent_name: "pi-rs".to_string(),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    pub fn agent_version(mut self, version: impl Into<String>) -> Self {
        self.agent_version = version.into();
        self
    }
}

/// Errors returned while serving an ACP connection.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("ACP connection failed: {0}")]
    Protocol(#[from] agent_client_protocol::Error),
    #[error("ACP session cleanup failed: {0}")]
    Cleanup(String),
}

/// ACP server backed by a protocol-neutral Pi multi-session manager.
#[derive(Clone)]
pub struct AcpServer {
    state: Arc<AcpState>,
}

impl AcpServer {
    pub fn new(manager: MultiSessionManager, options: AcpOptions) -> Self {
        Self {
            state: Arc::new(AcpState {
                manager,
                options,
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Serves one ACP connection over an arbitrary SDK transport.
    pub async fn serve(self, transport: impl ConnectTo<Agent>) -> Result<(), AcpError> {
        let state = Arc::clone(&self.state);
        let initialize = Arc::clone(&state);
        let create = Arc::clone(&state);
        let load = Arc::clone(&state);
        let resume = Arc::clone(&state);
        let list = Arc::clone(&state);
        let close = Arc::clone(&state);
        let configure = Arc::clone(&state);
        let prompt = Arc::clone(&state);
        let cancel = Arc::clone(&state);

        let result = Agent
            .builder()
            .name(&state.options.agent_name)
            .on_receive_request(
                async move |request: acp::InitializeRequest, responder, _connection| {
                    responder.respond(initialize.initialize(request))
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::NewSessionRequest, responder, _connection| {
                    responder.respond_with_result(create.new_session(request).await)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::LoadSessionRequest, responder, connection| {
                    responder.respond_with_result(load.load_session(request, &connection).await)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::ResumeSessionRequest, responder, _connection| {
                    responder.respond_with_result(resume.resume_session(request).await)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::ListSessionsRequest, responder, _connection| {
                    responder.respond_with_result(list.list_sessions(request).await)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::CloseSessionRequest, responder, _connection| {
                    responder.respond_with_result(close.close_session(request).await)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::SetSessionConfigOptionRequest, responder, _connection| {
                    responder.respond_with_result(configure.set_config_option(request).await)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |request: acp::PromptRequest, responder, connection| {
                    prompt.spawn_prompt(request, responder, connection)
                },
                on_receive_request!(),
            )
            .on_receive_notification(
                async move |notification: acp::CancelNotification, _connection| {
                    cancel.cancel(notification).await;
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_to(transport)
            .await;

        let cleanup = state.shutdown().await;
        result.map_err(AcpError::from)?;
        cleanup
    }
}

/// Serves ACP stable v1 on stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    manager: MultiSessionManager,
    options: AcpOptions,
) -> Result<(), AcpError> {
    AcpServer::new(manager, options).serve(Stdio::new()).await
}

struct AcpState {
    manager: MultiSessionManager,
    options: AcpOptions,
    sessions: Mutex<HashMap<String, Arc<ManagedSession>>>,
}

struct ManagedSession {
    session: PiSession,
    mcp: Option<McpToolSet>,
    prompt_gate: Mutex<()>,
}

#[derive(Debug, Clone)]
struct StoredSession {
    header: SessionHeader,
    path: PathBuf,
}

impl AcpState {
    fn initialize(&self, _request: acp::InitializeRequest) -> acp::InitializeResponse {
        let capabilities = acp::AgentCapabilities::new()
            .load_session(true)
            .prompt_capabilities(
                acp::PromptCapabilities::new()
                    .image(true)
                    .embedded_context(true),
            )
            // Stdio MCP is ACP baseline behavior; HTTP and SSE remain false.
            .mcp_capabilities(acp::McpCapabilities::new())
            .session_capabilities(
                acp::SessionCapabilities::new()
                    .list(acp::SessionListCapabilities::new())
                    .resume(acp::SessionResumeCapabilities::new())
                    .close(acp::SessionCloseCapabilities::new()),
            );
        acp::InitializeResponse::new(ProtocolVersion::V1)
            .agent_capabilities(capabilities)
            .agent_info(acp::Implementation::new(
                self.options.agent_name.clone(),
                self.options.agent_version.clone(),
            ))
    }

    async fn new_session(
        &self,
        request: acp::NewSessionRequest,
    ) -> agent_client_protocol::Result<acp::NewSessionResponse> {
        reject_additional_directories(&request.additional_directories)?;
        let cwd = canonical_session_cwd(&request.cwd)?;
        let (overlay, mcp) = generation_overlay(request.mcp_servers, &cwd).await?;
        let path = self
            .options
            .sessions_dir
            .join(format!("{}.jsonl", uuid::Uuid::now_v7()));
        let session = match self
            .manager
            .create_session_with_overlay(&cwd, path, overlay)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                shutdown_mcp(&mcp).await;
                return Err(internal_error(error));
            }
        };
        let id = session.id();
        let response = acp::NewSessionResponse::new(id.clone())
            .config_options(session_config_options(&session));
        self.sessions.lock().await.insert(
            id,
            Arc::new(ManagedSession {
                session,
                mcp,
                prompt_gate: Mutex::new(()),
            }),
        );
        Ok(response)
    }

    async fn load_session(
        &self,
        request: acp::LoadSessionRequest,
        connection: &ConnectionTo<agent_client_protocol::Client>,
    ) -> agent_client_protocol::Result<acp::LoadSessionResponse> {
        reject_additional_directories(&request.additional_directories)?;
        let session_id = request.session_id.0.to_string();
        let managed = self
            .open_session(&session_id, request.cwd, request.mcp_servers)
            .await?;
        replay_transcript(&managed.session, connection)?;
        Ok(
            acp::LoadSessionResponse::new()
                .config_options(session_config_options(&managed.session)),
        )
    }

    async fn resume_session(
        &self,
        request: acp::ResumeSessionRequest,
    ) -> agent_client_protocol::Result<acp::ResumeSessionResponse> {
        reject_additional_directories(&request.additional_directories)?;
        let session_id = request.session_id.0.to_string();
        let managed = self
            .open_session(&session_id, request.cwd, request.mcp_servers)
            .await?;
        Ok(acp::ResumeSessionResponse::new()
            .config_options(session_config_options(&managed.session)))
    }

    async fn open_session(
        &self,
        session_id: &str,
        cwd: PathBuf,
        mcp_servers: Vec<acp::McpServer>,
    ) -> agent_client_protocol::Result<Arc<ManagedSession>> {
        let cwd = canonical_session_cwd(&cwd)?;
        if let Some(active) = self.sessions.lock().await.get(session_id).cloned() {
            if active.session.cwd() != cwd {
                return Err(invalid_params(
                    "session cwd does not match the stored session",
                ));
            }
            if !mcp_servers.is_empty() {
                return Err(invalid_params(
                    "MCP servers cannot be changed while a session is active",
                ));
            }
            return Ok(active);
        }

        let stored = self
            .find_stored_session(session_id)
            .await?
            .ok_or_else(|| invalid_params(format!("unknown session: {session_id}")))?;
        if comparable_path(&stored.header.cwd) != comparable_path(&cwd) {
            return Err(invalid_params(
                "session cwd does not match the stored session",
            ));
        }
        let (overlay, mcp) = generation_overlay(mcp_servers, &cwd).await?;
        let session = match self
            .manager
            .open_session_with_overlay(&stored.path, overlay)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                shutdown_mcp(&mcp).await;
                return Err(internal_error(error));
            }
        };
        let managed = Arc::new(ManagedSession {
            session,
            mcp,
            prompt_gate: Mutex::new(()),
        });
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), Arc::clone(&managed));
        Ok(managed)
    }

    async fn list_sessions(
        &self,
        request: acp::ListSessionsRequest,
    ) -> agent_client_protocol::Result<acp::ListSessionsResponse> {
        let cwd = request
            .cwd
            .as_deref()
            .map(canonical_session_cwd)
            .transpose()?;
        let offset = match request.cursor {
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| invalid_params("invalid session/list cursor"))?,
            None => 0,
        };
        let mut sessions = self.scan_sessions().await?;
        let active = self.sessions.lock().await;
        for managed in active.values() {
            let header = managed.session.current().log().header();
            sessions.entry(header.id.clone()).or_insert(StoredSession {
                header,
                path: managed.session.path(),
            });
        }
        let mut sessions = sessions.into_values().collect::<Vec<_>>();
        sessions.sort_by_key(|stored| std::cmp::Reverse(stored.header.created_at));
        if let Some(cwd) = cwd {
            sessions.retain(|stored| comparable_path(&stored.header.cwd) == comparable_path(&cwd));
        }
        let next_offset = offset.saturating_add(LIST_PAGE_SIZE);
        let page = sessions
            .iter()
            .skip(offset)
            .take(LIST_PAGE_SIZE)
            .map(|stored| {
                let title = active
                    .get(&stored.header.id)
                    .and_then(|managed| managed.session.current().snapshot().name);
                acp::SessionInfo::new(stored.header.id.clone(), stored.header.cwd.clone())
                    .title(title)
            })
            .collect::<Vec<_>>();
        let mut response = acp::ListSessionsResponse::new(page);
        if next_offset < sessions.len() {
            response = response.next_cursor(next_offset.to_string());
        }
        Ok(response)
    }

    async fn close_session(
        &self,
        request: acp::CloseSessionRequest,
    ) -> agent_client_protocol::Result<acp::CloseSessionResponse> {
        let session_id = request.session_id.0.to_string();
        let managed = self
            .sessions
            .lock()
            .await
            .remove(&session_id)
            .ok_or_else(|| invalid_params(format!("unknown active session: {session_id}")))?;
        managed.session.abort();
        let close = self.manager.close_session(&managed.session).await;
        let mcp_close = if let Some(mcp) = &managed.mcp {
            mcp.shutdown().await.map_err(|error| error.to_string())
        } else {
            Ok(())
        };
        if let Err(error) = close {
            return Err(internal_error(match mcp_close {
                Ok(()) => error.to_string(),
                Err(mcp_error) => format!("{error}; MCP cleanup also failed: {mcp_error}"),
            }));
        }
        mcp_close.map_err(internal_error)?;
        Ok(acp::CloseSessionResponse::new())
    }

    async fn set_config_option(
        &self,
        request: acp::SetSessionConfigOptionRequest,
    ) -> agent_client_protocol::Result<acp::SetSessionConfigOptionResponse> {
        let managed = self.active_session(&request.session_id).await?;
        let value = request
            .value
            .as_value_id()
            .ok_or_else(|| invalid_params("configuration option requires a value ID"))?
            .0
            .as_ref();
        match request.config_id.0.as_ref() {
            MODEL_CONFIG_ID => {
                let (provider, model) = decode_model_value(value)?;
                managed
                    .session
                    .current()
                    .set_model(ProviderId::new(provider), ModelId::new(model))
                    .map_err(internal_error)?;
            }
            THINKING_CONFIG_ID => {
                let level = parse_thinking_level(value)?;
                managed
                    .session
                    .current()
                    .set_thinking_level(level)
                    .map_err(internal_error)?;
            }
            id => {
                return Err(invalid_params(format!(
                    "unknown configuration option: {id}"
                )));
            }
        }
        Ok(acp::SetSessionConfigOptionResponse::new(
            session_config_options(&managed.session),
        ))
    }

    fn spawn_prompt(
        self: &Arc<Self>,
        request: acp::PromptRequest,
        responder: Responder<acp::PromptResponse>,
        connection: ConnectionTo<agent_client_protocol::Client>,
    ) -> agent_client_protocol::Result<()> {
        let state = Arc::clone(self);
        connection.spawn({
            let task_connection = connection.clone();
            async move {
                let response = state.run_prompt(request, &task_connection).await;
                responder.respond_with_result(response)
            }
        })?;
        Ok(())
    }

    async fn run_prompt(
        &self,
        request: acp::PromptRequest,
        connection: &ConnectionTo<agent_client_protocol::Client>,
    ) -> agent_client_protocol::Result<acp::PromptResponse> {
        let managed = self.active_session(&request.session_id).await?;
        let _prompt = managed.prompt_gate.lock().await;
        let messages = vec![Message::User(UserMessage {
            content: request
                .prompt
                .into_iter()
                .map(acp_prompt_block_to_pi)
                .collect::<agent_client_protocol::Result<Vec<_>>>()?,
            timestamp_ms: now_ms(),
        })];
        let agent = managed.session.current();
        let mut subscription = agent.subscribe();
        let initial_revision = subscription.snapshot.revision;
        let prompt = agent.prompt_messages(messages);
        tokio::pin!(prompt);
        loop {
            tokio::select! {
                result = &mut prompt => {
                    let outcome = result.map_err(internal_error)?;
                    while let Ok(event) = subscription.events.try_recv() {
                        if event.revision > initial_revision {
                            send_projected_event(connection, &request.session_id, &event.event)?;
                        }
                    }
                    return Ok(acp::PromptResponse::new(match outcome.stop {
                        AgentLoopStop::Completed | AgentLoopStop::TerminatedByTools => acp::StopReason::EndTurn,
                        AgentLoopStop::Aborted => acp::StopReason::Cancelled,
                        AgentLoopStop::ProviderError => acp::StopReason::Refusal,
                        AgentLoopStop::MaxToolIterations => acp::StopReason::MaxTurnRequests,
                    }));
                }
                event = subscription.events.recv() => {
                    match event {
                        Ok(event) if event.revision > initial_revision => {
                            send_projected_event(connection, &request.session_id, &event.event)?;
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            return Err(internal_error("ACP event consumer lagged behind the Pi session"));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err(internal_error("Pi session event stream closed during prompt"));
                        }
                    }
                }
            }
        }
    }

    async fn cancel(&self, notification: acp::CancelNotification) {
        if let Some(session) = self
            .sessions
            .lock()
            .await
            .get(notification.session_id.0.as_ref())
            .cloned()
        {
            session.session.abort();
        }
    }

    async fn active_session(
        &self,
        session_id: &acp::SessionId,
    ) -> agent_client_protocol::Result<Arc<ManagedSession>> {
        self.sessions
            .lock()
            .await
            .get(session_id.0.as_ref())
            .cloned()
            .ok_or_else(|| invalid_params(format!("unknown active session: {session_id}")))
    }

    async fn find_stored_session(
        &self,
        session_id: &str,
    ) -> agent_client_protocol::Result<Option<StoredSession>> {
        Ok(self.scan_sessions().await?.remove(session_id))
    }

    async fn scan_sessions(&self) -> agent_client_protocol::Result<HashMap<String, StoredSession>> {
        let root = self.options.sessions_dir.clone();
        tokio::task::spawn_blocking(move || scan_session_dir(&root))
            .await
            .map_err(internal_error)?
            .map_err(internal_error)
    }

    async fn shutdown(&self) -> Result<(), AcpError> {
        let sessions = self
            .sessions
            .lock()
            .await
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>();
        let mut first_error = None;
        for managed in sessions {
            managed.session.abort();
            if let Err(error) = self.manager.close_session(&managed.session).await {
                first_error.get_or_insert_with(|| error.to_string());
            }
            if let Some(mcp) = &managed.mcp
                && let Err(error) = mcp.shutdown().await
            {
                first_error.get_or_insert_with(|| error.to_string());
            }
        }
        first_error.map_or(Ok(()), |error| Err(AcpError::Cleanup(error)))
    }
}

async fn generation_overlay(
    servers: Vec<acp::McpServer>,
    cwd: &Path,
) -> agent_client_protocol::Result<(SessionGenerationOverlay, Option<McpToolSet>)> {
    if servers.is_empty() {
        return Ok((SessionGenerationOverlay::new(), None));
    }
    let mut configs = Vec::with_capacity(servers.len());
    for server in servers {
        match server {
            acp::McpServer::Stdio(server) => {
                if !server.command.is_absolute() {
                    return Err(invalid_params(format!(
                        "MCP command for {} must be an absolute path",
                        server.name
                    )));
                }
                let env = server
                    .env
                    .into_iter()
                    .map(|variable| (variable.name, variable.value))
                    .collect::<BTreeMap<_, _>>();
                configs.push(
                    McpServerConfig::stdio(server.name, server.command.to_string_lossy())
                        .args(server.args)
                        .env(env)
                        .cwd(cwd),
                );
            }
            acp::McpServer::Http(_) | acp::McpServer::Sse(_) => {
                return Err(invalid_params(
                    "this ACP endpoint supports stdio MCP servers only",
                ));
            }
            _ => return Err(invalid_params("unsupported MCP transport")),
        }
    }
    let tools = McpToolSet::connect(configs).await.map_err(internal_error)?;
    let overlay_tools = tools.clone();
    let overlay = SessionGenerationOverlay::new().with_agent_plugin(move || overlay_tools.plugin());
    Ok((overlay, Some(tools)))
}

fn reject_additional_directories(paths: &[PathBuf]) -> agent_client_protocol::Result<()> {
    if paths.is_empty() {
        Ok(())
    } else {
        Err(invalid_params(
            "additionalDirectories is not supported by this ACP endpoint",
        ))
    }
}

fn canonical_session_cwd(path: &Path) -> agent_client_protocol::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid_params("session cwd must be an absolute path"));
    }
    let cwd = path.canonicalize().map_err(|error| {
        invalid_params(format!(
            "cannot access session cwd {}: {error}",
            path.display()
        ))
    })?;
    if !cwd.is_dir() {
        return Err(invalid_params(format!(
            "session cwd is not a directory: {}",
            cwd.display()
        )));
    }
    Ok(cwd)
}

fn scan_session_dir(root: &Path) -> std::io::Result<HashMap<String, StoredSession>> {
    let mut sessions = HashMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            let path = entry.path();
            if !file_type.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
            {
                continue;
            }
            let Some(header) = read_session_header(&path) else {
                continue;
            };
            sessions.insert(header.id.clone(), StoredSession { header, path });
        }
    }
    Ok(sessions)
}

fn read_session_header(path: &Path) -> Option<SessionHeader> {
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn replay_transcript(
    session: &PiSession,
    connection: &ConnectionTo<agent_client_protocol::Client>,
) -> agent_client_protocol::Result<()> {
    let id = session.id();
    let document = session.current().log().load().map_err(internal_error)?;
    for record in document.branch().map_err(internal_error)? {
        let SessionEntry::Message(entry) = &record.entry else {
            continue;
        };
        let Some(message) = entry.message.as_standard() else {
            continue;
        };
        match message {
            Message::User(message) => {
                for content in &message.content {
                    if let Some(content) = pi_content_to_acp(content) {
                        connection.send_notification(acp::SessionNotification::new(
                            id.clone(),
                            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(content)),
                        ))?;
                    }
                }
            }
            Message::Assistant(message) => {
                for content in &message.content {
                    let update = match content {
                        PiContentBlock::Text(text) => Some(acp::SessionUpdate::AgentMessageChunk(
                            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                                text.text.clone(),
                            ))),
                        )),
                        PiContentBlock::Thinking(thinking) => {
                            Some(acp::SessionUpdate::AgentThoughtChunk(
                                acp::ContentChunk::new(acp::ContentBlock::Text(
                                    acp::TextContent::new(thinking.thinking.clone()),
                                )),
                            ))
                        }
                        PiContentBlock::Image(_) | PiContentBlock::ToolCall(_) => None,
                    };
                    if let Some(update) = update {
                        connection
                            .send_notification(acp::SessionNotification::new(id.clone(), update))?;
                    }
                }
            }
            Message::ToolResult(_) | Message::Custom(_) => {}
        }
    }
    Ok(())
}

fn acp_prompt_block_to_pi(
    content: acp::ContentBlock,
) -> agent_client_protocol::Result<PiContentBlock> {
    match content {
        acp::ContentBlock::Text(text) => Ok(PiContentBlock::Text(PiTextContent::new(text.text))),
        acp::ContentBlock::Image(image) => Ok(PiContentBlock::Image(pi_core::ImageContent {
            data: image.data,
            mime_type: image.mime_type,
        })),
        acp::ContentBlock::ResourceLink(resource) => Ok(PiContentBlock::Text(PiTextContent::new(
            format!("[Resource: {}]({})", resource.name, resource.uri),
        ))),
        acp::ContentBlock::Resource(resource) => match resource.resource {
            acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                Ok(PiContentBlock::Text(PiTextContent::new(format!(
                    "<resource uri=\"{}\">\n{}\n</resource>",
                    resource.uri, resource.text
                ))))
            }
            acp::EmbeddedResourceResource::BlobResourceContents(resource) => {
                Ok(PiContentBlock::Text(PiTextContent::new(format!(
                    "[Embedded binary resource: {} ({})]",
                    resource.uri,
                    resource
                        .mime_type
                        .as_deref()
                        .unwrap_or("application/octet-stream")
                ))))
            }
            _ => Err(invalid_params("unsupported embedded resource")),
        },
        acp::ContentBlock::Audio(_) => Err(invalid_params("audio prompts are not supported")),
        _ => Err(invalid_params("unsupported ACP prompt content")),
    }
}

fn pi_content_to_acp(content: &PiContentBlock) -> Option<acp::ContentBlock> {
    match content {
        PiContentBlock::Text(text) => Some(acp::ContentBlock::Text(acp::TextContent::new(
            text.text.clone(),
        ))),
        PiContentBlock::Image(image) => Some(acp::ContentBlock::Image(acp::ImageContent::new(
            image.data.clone(),
            image.mime_type.clone(),
        ))),
        PiContentBlock::Thinking(_) | PiContentBlock::ToolCall(_) => None,
    }
}

fn project_event(event: &AgentSessionEvent) -> Option<acp::SessionUpdate> {
    match event {
        AgentSessionEvent::Agent(event) => match event.as_ref() {
            AgentEvent::MessageUpdate { event, .. } => match event {
                AssistantMessageEvent::TextDelta { delta, .. } => Some(
                    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                        acp::ContentBlock::Text(acp::TextContent::new(delta.clone())),
                    )),
                ),
                AssistantMessageEvent::ThinkingDelta { delta, .. } => Some(
                    acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk::new(
                        acp::ContentBlock::Text(acp::TextContent::new(delta.clone())),
                    )),
                ),
                _ => None,
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => Some(acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(tool_call_id.as_str().to_string(), tool_name.clone())
                    .kind(tool_kind(tool_name))
                    .status(acp::ToolCallStatus::InProgress)
                    .raw_input(args.clone()),
            )),
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => Some(acp::SessionUpdate::ToolCallUpdate(
                acp::ToolCallUpdate::new(
                    tool_call_id.as_str().to_string(),
                    acp::ToolCallUpdateFields::new()
                        .status(acp::ToolCallStatus::InProgress)
                        .content(tool_result_content(&partial_result.content))
                        .raw_output(tool_result_raw(partial_result)),
                ),
            )),
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => Some(acp::SessionUpdate::ToolCallUpdate(
                acp::ToolCallUpdate::new(
                    tool_call_id.as_str().to_string(),
                    acp::ToolCallUpdateFields::new()
                        .status(if *is_error {
                            acp::ToolCallStatus::Failed
                        } else {
                            acp::ToolCallStatus::Completed
                        })
                        .content(tool_result_content(&result.content))
                        .raw_output(tool_result_raw(result)),
                ),
            )),
            _ => None,
        },
        AgentSessionEvent::SessionInfoChanged { name } => {
            Some(acp::SessionUpdate::SessionInfoUpdate(
                acp::SessionInfoUpdate::new().title(name.clone()),
            ))
        }
        _ => None,
    }
}

fn send_projected_event(
    connection: &ConnectionTo<agent_client_protocol::Client>,
    session_id: &acp::SessionId,
    event: &AgentSessionEvent,
) -> agent_client_protocol::Result<()> {
    if let Some(update) = project_event(event) {
        connection.send_notification(acp::SessionNotification::new(session_id.clone(), update))?;
    }
    Ok(())
}

fn tool_result_content(content: &[PiContentBlock]) -> Vec<acp::ToolCallContent> {
    content
        .iter()
        .filter_map(pi_content_to_acp)
        .map(acp::ToolCallContent::from)
        .collect()
}

fn tool_result_raw(result: &pi_core::ToolResult) -> serde_json::Value {
    serde_json::json!({
        "content": result.content,
        "details": result.details,
        "isError": result.is_error,
        "terminate": result.terminate,
    })
}

fn tool_kind(name: &str) -> acp::ToolKind {
    let name = name.to_ascii_lowercase();
    if name.contains("read") || name.ends_with("ls") {
        acp::ToolKind::Read
    } else if name.contains("write") || name.contains("edit") {
        acp::ToolKind::Edit
    } else if name.contains("grep") || name.contains("find") || name.contains("search") {
        acp::ToolKind::Search
    } else if name.contains("bash") || name.contains("shell") || name.contains("exec") {
        acp::ToolKind::Execute
    } else if name.starts_with("mcp__") {
        acp::ToolKind::Fetch
    } else {
        acp::ToolKind::Other
    }
}

fn session_config_options(session: &PiSession) -> Vec<acp::SessionConfigOption> {
    let current = session.current();
    let state = current.runtime().agent().state();
    let mut grouped = BTreeMap::<String, Vec<acp::SessionConfigSelectOption>>::new();
    let mut current_model_is_listed = false;
    for model in current.runtime().available_models() {
        current_model_is_listed |=
            model.provider == state.provider_id && model.id == state.model_id;
        grouped.entry(model.provider.to_string()).or_default().push(
            acp::SessionConfigSelectOption::new(
                encode_model_value(&model.provider, &model.id),
                model.name,
            ),
        );
    }
    if !current_model_is_listed {
        grouped
            .entry(state.provider_id.to_string())
            .or_default()
            .push(acp::SessionConfigSelectOption::new(
                encode_model_value(&state.provider_id, &state.model_id),
                state.model_id.to_string(),
            ));
    }
    let groups = grouped
        .into_iter()
        .map(|(provider, options)| {
            acp::SessionConfigSelectGroup::new(provider.clone(), provider, options)
        })
        .collect::<Vec<_>>();
    let model = acp::SessionConfigOption::select(
        MODEL_CONFIG_ID,
        "Model",
        encode_model_value(&state.provider_id, &state.model_id),
        groups,
    )
    .category(acp::SessionConfigOptionCategory::Model);
    let thinking_options = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::XHigh,
        ThinkingLevel::Max,
    ]
    .into_iter()
    .map(|level| acp::SessionConfigSelectOption::new(level.as_str(), thinking_level_name(level)))
    .collect::<Vec<_>>();
    let thinking = acp::SessionConfigOption::select(
        THINKING_CONFIG_ID,
        "Thinking",
        state.thinking_level.as_str(),
        thinking_options,
    )
    .category(acp::SessionConfigOptionCategory::ThoughtLevel);
    vec![model, thinking]
}

fn encode_model_value(provider: &ProviderId, model: &ModelId) -> String {
    serde_json::to_string(&(provider.as_str(), model.as_str()))
        .expect("model identifiers always serialize")
}

fn decode_model_value(value: &str) -> agent_client_protocol::Result<(String, String)> {
    serde_json::from_str::<(String, String)>(value)
        .map_err(|_| invalid_params("invalid model configuration value"))
}

fn parse_thinking_level(value: &str) -> agent_client_protocol::Result<ThinkingLevel> {
    match value {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        "max" => Ok(ThinkingLevel::Max),
        _ => Err(invalid_params(format!("invalid thinking level: {value}"))),
    }
}

const fn thinking_level_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "Off",
        ThinkingLevel::Minimal => "Minimal",
        ThinkingLevel::Low => "Low",
        ThinkingLevel::Medium => "Medium",
        ThinkingLevel::High => "High",
        ThinkingLevel::XHigh => "Extra high",
        ThinkingLevel::Max => "Maximum",
    }
}

async fn shutdown_mcp(mcp: &Option<McpToolSet>) {
    if let Some(mcp) = mcp {
        let _ = mcp.shutdown().await;
    }
}

fn invalid_params(message: impl ToString) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(message.to_string())
}

fn internal_error(error: impl ToString) -> agent_client_protocol::Error {
    agent_client_protocol::util::internal_error(error.to_string())
}

fn comparable_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use agent_client_protocol::{ByteStreams, Client};
    use pi_agent::AgentOptions;
    use pi_runtime::PiRuntime;
    use pi_session::{
        AgentSession, AgentSessionOptions, AgentSessionRuntimeRequest, AgentSessionRuntimeTarget,
        SessionLog,
    };
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    use super::*;

    fn test_manager(turn: ScriptedTurn) -> MultiSessionManager {
        MultiSessionManager::new(move |request: AgentSessionRuntimeRequest| {
            let turn = turn.clone();
            async move {
                let AgentSessionRuntimeRequest {
                    target,
                    generation_overlay,
                    ..
                } = request;
                let (cwd, path, create, reused_log) = match target {
                    AgentSessionRuntimeTarget::Create { cwd, path, .. } => (cwd, path, true, None),
                    AgentSessionRuntimeTarget::Open { path } => {
                        let (_, document) = SessionLog::open(&path)?;
                        (document.header.cwd, path, false, None)
                    }
                    AgentSessionRuntimeTarget::Reuse { log } => {
                        let document = log.load()?;
                        (
                            document.header.cwd,
                            log.path().to_path_buf(),
                            false,
                            Some(log),
                        )
                    }
                };
                let mut builder = PiRuntime::builder()
                    .provider_plugin(ScriptedProviderPlugin::scripted([turn]))
                    .agent_options(AgentOptions {
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        cwd,
                        ..AgentOptions::default()
                    });
                builder = generation_overlay.apply_to(builder);
                let runtime = builder.build()?;
                if create {
                    AgentSession::prepare_create_with_options(
                        runtime,
                        path,
                        AgentSessionOptions::default(),
                    )
                    .await
                } else if let Some(log) = reused_log {
                    AgentSession::prepare_reuse_with_options(
                        runtime,
                        log,
                        AgentSessionOptions::default(),
                    )
                    .await
                } else {
                    AgentSession::prepare_open_with_options(
                        runtime,
                        path,
                        AgentSessionOptions::default(),
                    )
                    .await
                }
            }
        })
    }

    #[tokio::test]
    async fn stable_v1_round_trip_covers_prompt_config_and_session_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager(ScriptedTurn::Text("hello from pi".to_string()));
        let server = AcpServer::new(manager.clone(), AcpOptions::new(directory.path()));
        let updates = Arc::new(StdMutex::new(Vec::<acp::SessionUpdate>::new()));
        let received = Arc::clone(&updates);

        let (client_writer, server_reader) = tokio::io::duplex(64 * 1024);
        let (server_writer, client_reader) = tokio::io::duplex(64 * 1024);
        let server_transport =
            ByteStreams::new(server_writer.compat_write(), server_reader.compat());
        let client_transport =
            ByteStreams::new(client_writer.compat_write(), client_reader.compat());
        let server_task = tokio::spawn(server.serve(server_transport));

        Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _connection| {
                    received
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(notification.update);
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_transport, async |connection| {
                let initialized = connection
                    .send_request(acp::InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
                assert!(initialized.agent_capabilities.load_session);
                assert!(
                    initialized
                        .agent_capabilities
                        .session_capabilities
                        .list
                        .is_some()
                );

                let cwd = directory.path().canonicalize().unwrap();
                let created = connection
                    .send_request(acp::NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await?;
                assert!(created.config_options.is_some());
                let session_id = created.session_id.clone();

                let configured = connection
                    .send_request(acp::SetSessionConfigOptionRequest::new(
                        session_id.clone(),
                        THINKING_CONFIG_ID,
                        "minimal",
                    ))
                    .block_task()
                    .await?;
                assert!(configured.config_options.iter().any(|option| {
                    option.id.0.as_ref() == THINKING_CONFIG_ID
                        && matches!(
                            &option.kind,
                            acp::SessionConfigKind::Select(select)
                                if select.current_value.0.as_ref() == "minimal"
                        )
                }));

                let prompted = connection
                    .send_request(acp::PromptRequest::new(
                        session_id.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new("hello"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(prompted.stop_reason, acp::StopReason::EndTurn);

                connection
                    .send_request(acp::CloseSessionRequest::new(session_id.clone()))
                    .block_task()
                    .await?;

                let listed = connection
                    .send_request(acp::ListSessionsRequest::new().cwd(cwd.clone()))
                    .block_task()
                    .await?;
                assert!(
                    listed
                        .sessions
                        .iter()
                        .any(|session| session.session_id == session_id)
                );

                connection
                    .send_request(acp::ResumeSessionRequest::new(
                        session_id.clone(),
                        cwd.clone(),
                    ))
                    .block_task()
                    .await?;
                connection
                    .send_request(acp::CloseSessionRequest::new(session_id.clone()))
                    .block_task()
                    .await?;

                connection
                    .send_request(acp::LoadSessionRequest::new(session_id.clone(), cwd))
                    .block_task()
                    .await?;
                connection
                    .send_request(acp::CloseSessionRequest::new(session_id))
                    .block_task()
                    .await?;
                Ok(())
            })
            .await
            .unwrap();

        server_task.await.unwrap().unwrap();
        let updates = updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(updates.iter().any(|update| matches!(
            update,
            acp::SessionUpdate::AgentMessageChunk(chunk)
                if matches!(&chunk.content, acp::ContentBlock::Text(text) if text.text == "hello from pi")
        )));
        assert!(manager.sessions().is_empty());
    }

    #[tokio::test]
    async fn cancel_notification_interrupts_an_in_flight_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager(ScriptedTurn::WaitForAbort);
        let observed_manager = manager.clone();
        let server = AcpServer::new(manager.clone(), AcpOptions::new(directory.path()));

        let (client_writer, server_reader) = tokio::io::duplex(64 * 1024);
        let (server_writer, client_reader) = tokio::io::duplex(64 * 1024);
        let server_transport =
            ByteStreams::new(server_writer.compat_write(), server_reader.compat());
        let client_transport =
            ByteStreams::new(client_writer.compat_write(), client_reader.compat());
        let server_task = tokio::spawn(server.serve(server_transport));

        Client
            .connect_with(client_transport, async |connection| {
                connection
                    .send_request(acp::InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let created = connection
                    .send_request(acp::NewSessionRequest::new(
                        directory.path().canonicalize().unwrap(),
                    ))
                    .block_task()
                    .await?;
                let prompt = connection
                    .send_request(acp::PromptRequest::new(
                        created.session_id.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new("wait"))],
                    ))
                    .block_task();
                let mut started = false;
                for _ in 0..1_000 {
                    started = observed_manager
                        .sessions()
                        .iter()
                        .any(|session| session.current().runtime().agent().state().is_running);
                    if started {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(started, "the ACP prompt did not reach the Pi agent loop");
                connection
                    .send_notification(acp::CancelNotification::new(created.session_id.clone()))?;
                let response = prompt.await?;
                assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
                connection
                    .send_request(acp::CloseSessionRequest::new(created.session_id))
                    .block_task()
                    .await?;
                Ok(())
            })
            .await
            .unwrap();

        server_task.await.unwrap().unwrap();
        assert!(manager.sessions().is_empty());
    }
}
