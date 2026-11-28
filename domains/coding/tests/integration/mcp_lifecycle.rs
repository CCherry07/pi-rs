//! Cross-layer ownership checks for the deliberately immutable Rust MCP generations.
//! Stateful local HTTP sessions make connection reuse and shutdown observable without
//! live credentials, child-process timing, sleeps, or private runtime getters.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pi_agent::AgentOptions;
use pi_coding::{Config, Features, ProductSessionFactory, ProjectTrustService};
use pi_core::{AbortHandle, ContentBlock, Message, ModelId, PluginId, ProviderId, TextContent};
use pi_plugin::{
    ContextParts, EphemeralSessionRequest, Plugin, PluginContextScope, ToolContext, ToolUpdateSink,
};
use pi_plugin_mcp::{McpLibrary, McpSupervisor};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionEvent, AgentSessionOptions, CreateSessionOptions, ForkPosition,
    MultiSessionManager, PiSession, PreparedSessionGeneration, SessionError,
    SessionGenerationFactory, SessionGenerationOverlay, SessionGenerationRequest, SessionHeader,
    SessionStore,
};
use pi_settings::{DefaultProjectTrust, SettingsManager};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};

mod authorization;
mod selection;

const TIMEOUT: Duration = Duration::from_secs(30);
const TOOL: &str = "mcp__fixture__echo";

#[derive(Clone, Debug, Default)]
struct Trace {
    initialized: Vec<String>,
    deleted: Vec<String>,
    calls: Vec<String>,
    rejected_sessions: Vec<String>,
    provider_requests: usize,
    provider_tools: Vec<Vec<String>>,
    remote_tools: Option<Vec<Value>>,
    slow_tools: Option<Vec<Value>>,
    slow_listing: bool,
    slow_replied: bool,
}

struct HttpFixture {
    base: String,
    trace: watch::Sender<Trace>,
    slow_gate: Arc<Semaphore>,
    provider_gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HttpFixture {
    async fn new() -> Self {
        let (trace, _) = watch::channel(Trace::default());
        let slow_gate = Arc::new(Semaphore::new(0));
        let provider_gate = Arc::new(Mutex::new(None::<Arc<Semaphore>>));
        let app = axum::Router::new()
            .route("/mcp", mcp_route(trace.clone(), None))
            .route("/slow", mcp_route(trace.clone(), Some(slow_gate.clone())))
            .route("/auth", axum::routing::post(|| async {
                (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer realm=lifecycle-fixture")])
            }))
            .route("/v1/chat/completions", axum::routing::post({
                let trace = trace.clone();
                let provider_gate = provider_gate.clone();
                move |axum::Json(request): axum::Json<Value>| {
                    let trace = trace.clone();
                    let provider_gate = provider_gate.clone();
                    async move {
                        let gate = provider_gate.lock().unwrap().take();
                        trace.send_modify(|trace| {
                            trace.provider_requests += 1;
                            trace.provider_tools.push(
                                request["tools"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|tool| tool["function"]["name"].as_str())
                                    .map(str::to_owned)
                                    .collect(),
                            );
                        });
                        if let Some(gate) = gate {
                            gate.acquire().await.unwrap().forget();
                        }
                        let messages = request["messages"].as_array().unwrap();
                        let invoke = messages.iter().any(|message| message["content"] == "invoke fixture");
                        let completed = messages.iter().any(|message| message["role"] == "tool");
                        let (delta, reason) = if invoke && !completed {
                            (json!({"role":"assistant","tool_calls":[{
                                "index":0,"id":"fixture-call","type":"function",
                                "function":{"name":TOOL,"arguments":"{}"}
                            }]}), "tool_calls")
                        } else {
                            (json!({"role":"assistant","content":"done"}), "stop")
                        };
                        let chunk = json!({"id":"lifecycle","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                        let end = json!({"id":"lifecycle","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{},"finish_reason":reason}]});
                        ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            trace,
            slow_gate,
            provider_gate,
            task,
        }
    }

    fn snapshot(&self) -> Trace {
        self.trace.borrow().clone()
    }

    async fn until(&self, predicate: impl Fn(&Trace) -> bool) {
        let mut receiver = self.trace.subscribe();
        tokio::time::timeout(TIMEOUT, receiver.wait_for(predicate))
            .await
            .unwrap_or_else(|_| panic!("missing fixture event: {:?}", self.snapshot()))
            .unwrap();
    }

    async fn deleted(&self, expected: &[&str]) {
        self.until(|trace| {
            expected
                .iter()
                .all(|id| trace.deleted.iter().any(|deleted| deleted == id))
        })
        .await;
        let trace = self.snapshot();
        assert_eq!(
            trace.deleted.len(),
            expected.len(),
            "unexpected session shutdown: {trace:?}"
        );
    }

    fn entry(&self, path: &str) -> Value {
        json!({"url": format!("{}{path}", self.base), "exposure":"direct"})
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn mcp_route(
    trace: watch::Sender<Trace>,
    gate: Option<Arc<Semaphore>>,
) -> axum::routing::MethodRouter {
    axum::routing::post({
        let trace = trace.clone();
        move |headers: HeaderMap, axum::Json(request): axum::Json<Value>| {
            let trace = trace.clone();
            let gate = gate.clone();
            async move {
                let slow = gate.is_some();
                if request["method"] == "tools/list"
                    && let Some(gate) = gate
                {
                    trace.send_modify(|trace| trace.slow_listing = true);
                    gate.acquire().await.unwrap().forget();
                    // A server-side response signal is not a client readiness-commit signal.
                    trace.send_modify(|trace| trace.slow_replied = true);
                }
                protocol_response(&trace, &headers, &request, slow)
            }
        }
    })
    .delete(move |headers: HeaderMap| {
        let trace = trace.clone();
        async move {
            let id = session_id(&headers);
            trace.send_modify(|trace| trace.deleted.push(id));
            StatusCode::NO_CONTENT
        }
    })
}

fn session_id(headers: &HeaderMap) -> String {
    headers
        .get("mcp-session-id")
        .expect("stateful session header")
        .to_str()
        .unwrap()
        .to_owned()
}

fn protocol_response(
    trace: &watch::Sender<Trace>,
    headers: &HeaderMap,
    request: &Value,
    slow: bool,
) -> Response {
    let id = &request["id"];
    if id.is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let mut new_session = None;
    let result = match request["method"].as_str().unwrap() {
        "server/discover" => {
            return axum::Json(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Use initialize"}}),
            )
            .into_response();
        }
        "initialize" => {
            trace.send_modify(|trace| {
                let id = format!("connection-{}", trace.initialized.len() + 1);
                trace.initialized.push(id.clone());
                new_session = Some(id);
            });
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"lifecycle-fixture","version":"1"}})
        }
        "tools/list" => {
            let state = trace.borrow();
            let tools = if slow {
                state.slow_tools.as_ref()
            } else {
                None
            }
            .or(state.remote_tools.as_ref())
            .cloned()
            .unwrap_or_else(|| vec![json!({"name":"echo","inputSchema":{"type":"object"}})]);
            json!({"tools":tools})
        }
        "tools/call" => {
            let connection = session_id(headers);
            assert!(
                !trace.borrow().deleted.contains(&connection),
                "call on deleted connection"
            );
            trace.send_modify(|trace| trace.calls.push(connection.clone()));
            if trace.borrow().rejected_sessions.contains(&connection) {
                return (
                    StatusCode::FORBIDDEN,
                    [(
                        "www-authenticate",
                        "Bearer error=\"insufficient_scope\", scope=\"private-scope-marker\"",
                    )],
                )
                    .into_response();
            }
            json!({"content":[{"type":"text","text":connection}]})
        }
        method => panic!("unexpected MCP method {method}"),
    };
    let mut response = axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response();
    if let Some(id) = new_session {
        response
            .headers_mut()
            .insert("mcp-session-id", id.parse().unwrap());
    }
    response
}

struct ProductFixture {
    _directory: tempfile::TempDir,
    agent: PathBuf,
    project: PathBuf,
    factory: Arc<ProductSessionFactory>,
    http: HttpFixture,
}

impl ProductFixture {
    async fn new(auth_only: bool) -> Self {
        let http = HttpFixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            agent.join("memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        let servers = if auth_only {
            json!({"locked":http.entry("/auth")})
        } else {
            json!({"fixture":http.entry("/mcp")})
        };
        std::fs::write(
            agent.join("mcp.json"),
            json!({"mcpServers":servers}).to_string(),
        )
        .unwrap();
        std::fs::write(agent.join("models.json"), json!({"providers":{"fixture":{
            "baseUrl":format!("{}/v1", http.base),"api":"openai-completions","apiKey":"local-fixture",
            "models":[{"id":"test","contextWindow":100000,"maxTokens":1024}]
        }}}).to_string()).unwrap();
        let mut config = Config::new(project.clone(), agent.clone());
        config.provider = "fixture".into();
        config.model = Some("test".into());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        config.features = Features::none();
        let (trust, _) =
            ProjectTrustService::new(&agent, Some(false), false, DefaultProjectTrust::Never)
                .unwrap();
        let factory = Arc::new(ProductSessionFactory::new(
            config,
            trust,
            SettingsManager::new(&agent),
        ));
        Self {
            _directory: directory,
            agent,
            project,
            factory,
            http,
        }
    }

    fn manager(&self) -> MultiSessionManager {
        MultiSessionManager::new(SharedProductFactory(self.factory.clone()))
    }

    async fn create(&self, manager: &MultiSessionManager, name: &str) -> PiSession {
        manager
            .create_session(CreateSessionOptions::new(
                &self.project,
                self.agent.join(name),
            ))
            .await
            .unwrap()
    }
}

// Forward the publication callback too; a closure forwarding prepare alone would
// accidentally omit the real product's stable context binding.
struct SharedProductFactory(Arc<ProductSessionFactory>);

#[async_trait]
impl SessionGenerationFactory for SharedProductFactory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        self.0.prepare_generation(request).await
    }

    fn session_registered(&self, session: &PiSession) {
        self.0.session_registered(session);
    }
}

async fn invoke(session: &PiSession, expected_connection: &str) {
    let current = session.current();
    let context = ContextParts::new(current.plugin_context(PluginContextScope::Base));
    let outcome = context
        .session
        .run_ephemeral(
            EphemeralSessionRequest {
                system_prompt: Some("Call the fixture tool once.".into()),
                origin: "mcp-lifecycle-test".into(),
                inherit_history: false,
                history_tail: None,
                messages: vec![Message::User(pi_core::UserMessage::text(
                    "invoke fixture",
                    1,
                ))],
                tools: vec![TOOL.into()],
                plugins: Vec::new(),
                model: None,
                thinking_level: None,
                max_tool_iterations: 2,
                max_input_tokens: None,
                compaction: None,
                timeout: TIMEOUT,
            },
            AbortHandle::new().1,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        pi_plugin::EphemeralSessionStatus::Completed,
        "{outcome:?}"
    );
    let result = outcome
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("MCP tool result");
    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        result.content,
        vec![ContentBlock::Text(TextContent::new(expected_connection))]
    );
}

#[tokio::test]
async fn shared_product_factory_same_saved_path_has_independent_connections_and_shutdown() {
    let fixture = ProductFixture::new(false).await;
    let path = fixture.agent.join("shared.jsonl");
    SessionStore::create(&path, SessionHeader::new("shared", &fixture.project)).unwrap();
    let first_manager = fixture.manager();
    let second_manager = fixture.manager();
    let first = first_manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    let second = second_manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    assert_eq!(first.id(), second.id());
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);
    invoke(&first, "connection-1").await;
    invoke(&second, "connection-2").await;

    first_manager.shutdown().await.unwrap();
    fixture.http.deleted(&["connection-1"]).await;
    // Retain both the first session handle and the shared product factory here.
    invoke(&second, "connection-2").await;
    second.reload().await.unwrap();
    invoke(&second, "connection-2").await;
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);
    second_manager.shutdown().await.unwrap();
    fixture
        .http
        .deleted(&["connection-1", "connection-2"])
        .await;
}

#[tokio::test]
async fn reload_reuses_transport_but_new_same_path_resume_and_fork_replace_it() {
    let fixture = ProductFixture::new(false).await;
    let manager = fixture.manager();
    let session = fixture.create(&manager, "draft.jsonl").await;
    let retained_original = session.current();
    session.reload().await.unwrap();
    assert!(!Arc::ptr_eq(&retained_original, &session.current()));
    invoke(&session, "connection-1").await;
    assert!(
        !session.path().exists(),
        "ephemeral invocation must not materialize the journal"
    );
    assert!(fixture.http.snapshot().deleted.is_empty());

    let second_path = fixture.agent.join("second.jsonl");
    session
        .new_session(&fixture.project, &second_path)
        .await
        .unwrap();
    fixture.http.deleted(&["connection-1"]).await;
    invoke(&session, "connection-2").await;
    let id = session.id();
    session.resume_session(&second_path).await.unwrap();
    assert_eq!(session.id(), id);
    assert!(!second_path.exists());
    fixture
        .http
        .deleted(&["connection-1", "connection-2"])
        .await;
    invoke(&session, "connection-3").await;

    session
        .current()
        .prompt("persist a fork point")
        .await
        .unwrap();
    let user_id = session.current().history().load().unwrap().entries.into_iter().find(|record| {
        matches!(&record.entry, pi_session::SessionEntry::Message(message) if message.message.role() == "user")
    }).expect("persisted user entry").id;
    session
        .fork_session(user_id, ForkPosition::At)
        .await
        .unwrap();
    assert_ne!(session.path(), second_path);
    fixture
        .http
        .deleted(&["connection-1", "connection-2", "connection-3"])
        .await;
    invoke(&session, "connection-4").await;
    session.reload().await.unwrap();
    assert_eq!(fixture.http.snapshot().initialized.len(), 4);
    invoke(&session, "connection-4").await;
    manager.shutdown().await.unwrap();
    fixture
        .http
        .deleted(&[
            "connection-1",
            "connection-2",
            "connection-3",
            "connection-4",
        ])
        .await;
}

struct DuplicateCandidate(bool);

#[pi_plugin::plugin]
impl Plugin for DuplicateCandidate {
    fn id(&self) -> PluginId {
        PluginId::new(if self.0 { "mcp" } else { "lifecycle-overlay" })
    }
}

#[tokio::test]
async fn late_registration_failure_preserves_published_owner_and_prior_tool() {
    let fixture = ProductFixture::new(false).await;
    let manager = fixture.manager();
    let fail = Arc::new(AtomicBool::new(false));
    let overlay = SessionGenerationOverlay::new().with_plugin({
        let fail = fail.clone();
        move || Arc::new(DuplicateCandidate(fail.load(Ordering::Acquire)))
    });
    let path = fixture.agent.join("rollback.jsonl");
    let session = manager
        .create_session(CreateSessionOptions {
            generation_overlay: overlay,
            ..CreateSessionOptions::new(&fixture.project, &path)
        })
        .await
        .unwrap();
    let active = session.current();
    invoke(&session, "connection-1").await;
    fail.store(true, Ordering::Release);
    let error = session.reload().await.unwrap_err().to_string();
    assert!(
        error.contains("duplicate") && error.contains("mcp"),
        "{error}"
    );
    fail.store(false, Ordering::Release);
    assert!(Arc::ptr_eq(&active, &session.current()));
    assert!(fixture.http.snapshot().deleted.is_empty());
    invoke(&session, "connection-1").await;
    // A failed staged lease must not hide the still-published cache entry.
    session.reload().await.unwrap();
    invoke(&session, "connection-1").await;
    assert_eq!(fixture.http.snapshot().initialized.len(), 1);
    assert!(!path.exists());
    manager.shutdown().await.unwrap();
    fixture.http.deleted(&["connection-1"]).await;
}

#[tokio::test]
async fn changed_connection_candidate_failure_keeps_the_original_transport_usable() {
    let fixture = ProductFixture::new(false).await;
    let replacement_server = HttpFixture::new().await;
    let manager = fixture.manager();
    let fail = Arc::new(AtomicBool::new(false));
    let overlay = SessionGenerationOverlay::new().with_plugin({
        let fail = fail.clone();
        move || Arc::new(DuplicateCandidate(fail.load(Ordering::Acquire)))
    });
    let session = manager
        .create_session(CreateSessionOptions {
            generation_overlay: overlay,
            ..CreateSessionOptions::new(
                &fixture.project,
                fixture.agent.join("connection-rollback.jsonl"),
            )
        })
        .await
        .unwrap();
    let active = session.current();
    invoke(&session, "connection-1").await;
    // Replace the input *before* the late registry failure. Reusing the exact old
    // input would only exercise lease bookkeeping, not transport transactionality.
    std::fs::write(
        fixture.agent.join("mcp.json"),
        json!({
            "mcpServers":{"fixture":replacement_server.entry("/mcp")}
        })
        .to_string(),
    )
    .unwrap();
    fail.store(true, Ordering::Release);
    let error = session.reload().await.unwrap_err().to_string();
    fail.store(false, Ordering::Release);
    assert!(
        error.contains("duplicate") && error.contains("mcp"),
        "{error}"
    );
    assert!(Arc::ptr_eq(&active, &session.current()));
    assert_eq!(
        replacement_server.snapshot().initialized.len(),
        1,
        "failure must occur after candidate MCP preparation"
    );
    invoke(&session, "connection-1").await;
    assert_eq!(
        fixture.http.snapshot().calls.len(),
        2,
        "the prior tool must still reach the original endpoint"
    );
    assert!(
        fixture.http.snapshot().deleted.is_empty(),
        "failed preparation must not retire the active transport"
    );
    assert!(replacement_server.snapshot().calls.is_empty());
    replacement_server.deleted(&["connection-1"]).await;

    std::fs::write(
        fixture.agent.join("mcp.json"),
        json!({
            "mcpServers":{"fixture":fixture.http.entry("/mcp")}
        })
        .to_string(),
    )
    .unwrap();
    session.reload().await.unwrap();
    invoke(&session, "connection-1").await;
    assert_eq!(fixture.http.snapshot().initialized.len(), 1);
    manager.shutdown().await.unwrap();
    fixture.http.deleted(&["connection-1"]).await;
}

#[tokio::test]
async fn dropping_unsaved_product_manager_releases_owner_with_factory_and_context_retained() {
    let fixture = ProductFixture::new(false).await;
    let manager = fixture.manager();
    let session = fixture.create(&manager, "unsaved.jsonl").await;
    let path = session.path();
    let current = session.current();
    let weak_session = Arc::downgrade(&current);
    let context = current.plugin_context(PluginContextScope::Base);
    drop(current);
    drop(session);
    drop(manager);
    assert!(
        weak_session.upgrade().is_none(),
        "the product factory must not pin a session"
    );
    fixture.http.deleted(&["connection-1"]).await;
    assert!(!path.exists());
    drop(context);
    // Reuse the very same still-live ProductSessionFactory after abrupt destruction.
    let manager = fixture.manager();
    let session = fixture.create(&manager, "unsaved.jsonl").await;
    invoke(&session, "connection-2").await;
    manager.shutdown().await.unwrap();
    fixture
        .http
        .deleted(&["connection-1", "connection-2"])
        .await;
    assert!(!path.exists());
}

#[tokio::test]
async fn auth_only_fault_keeps_generation_and_control_plane_available() {
    let fixture = ProductFixture::new(true).await;
    let manager = fixture.manager();
    let session = fixture.create(&manager, "auth-only.jsonl").await;
    let current = session.current();
    assert!(
        current
            .catalog()
            .command_specs()
            .iter()
            .any(|command| command.name == "mcp")
    );
    assert!(
        !current
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name.starts_with("mcp__"))
    );
    let mut events = current.subscribe().events;
    current.prompt("/mcp status").await.unwrap();
    let mut notices = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AgentSessionEvent::PluginNotice { message, .. } = event.event {
            notices.push(message);
        }
    }
    let status = notices.join("\n");
    assert!(
        status.contains("locked") && status.contains("needs sign-in"),
        "{status}"
    );
    assert!(Arc::ptr_eq(&current, &session.current()));
    assert_eq!(fixture.http.snapshot().provider_requests, 0);
    assert!(!session.path().exists());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn freshness_owner_drop_cancels_transport_despite_current_executable_snapshots() {
    // Public library -> runtime -> session handoff, deliberately separate from
    // ProductSessionFactory: its private runtime cannot be extracted by an integration test.
    let fixture = ProductFixture::new(false).await;
    let library = McpLibrary::new(&fixture.agent, Some(&fixture.project), false);
    let retained = Arc::new(Mutex::new(None));
    let manager = MultiSessionManager::new({
        let retained = retained.clone();
        move |request: SessionGenerationRequest| {
            let library = library.clone();
            let retained = retained.clone();
            async move {
                let supervisor = McpSupervisor::new();
                let weak_owner = supervisor.downgrade();
                let prepared = library
                    .prepare_supervised_generation(supervisor, BTreeMap::new())
                    .await
                    .map_err(SessionError::Runtime)?;
                let readiness = prepared
                    .readiness_watch
                    .expect("supervised generation lease");
                let plugin = prepared.plugin;
                let runtime = PiRuntime::builder()
                    .plugin_arc(plugin.clone())
                    .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                        "unused".into(),
                    )]))
                    .agent_options(AgentOptions {
                        cwd: request.cwd,
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        ..AgentOptions::default()
                    })
                    .build()?;
                let registries = runtime.agent().runtime().registries().clone();
                let command = registries.command("mcp").unwrap();
                let tool = registries.tool(TOOL).unwrap();
                *retained.lock().unwrap() = Some((
                    runtime.clone(),
                    plugin,
                    command,
                    tool,
                    registries,
                    weak_owner,
                ));
                Ok(PreparedSessionGeneration::new(
                    runtime,
                    AgentSessionOptions::default()
                        .generation_freshness(Arc::new(move || Ok(readiness.changed()))),
                ))
            }
        }
    });
    let path = fixture.agent.join("executable-retention.jsonl");
    let session = manager
        .create_session(CreateSessionOptions::new(&fixture.project, &path))
        .await
        .unwrap();
    let (runtime, plugin, command, tool, registries, weak_owner) =
        retained.lock().unwrap().take().unwrap();
    let execute = || {
        let (updates, _) = ToolUpdateSink::channel();
        tool.execute(
            ToolContext::standalone(fixture.project.clone(), AbortHandle::new().1),
            "retained-call".into(),
            json!({}),
            updates,
        )
    };
    assert!(!execute().await.unwrap().is_error);
    drop(session);
    drop(manager);
    assert!(
        weak_owner.upgrade().is_none(),
        "executable snapshots cannot own the supervisor"
    );
    fixture.http.deleted(&["connection-1"]).await;
    let result = tokio::time::timeout(TIMEOUT, execute()).await.unwrap();
    assert!(
        result.is_err() || result.is_ok_and(|result| result.is_error),
        "retained tool must not execute after owner destruction"
    );
    assert_eq!(fixture.http.snapshot().calls, ["connection-1"]);
    assert!(!path.exists());
    // Keep the *current*, never-reloaded snapshots alive throughout the cancellation proof.
    drop((runtime, plugin, command, tool, registries));
}

const SLOW_TOOL: &str = "mcp__slow__echo";

fn has_slow_tool(session: &pi_session::AgentSession) -> bool {
    session
        .catalog()
        .tool_specs()
        .iter()
        .any(|tool| tool.name == SLOW_TOOL)
}

fn configure_slow_server(fixture: &ProductFixture, include_direct: bool) {
    let mut slow = fixture.http.entry("/slow");
    slow["exposure"] = json!("codemode");
    let mut servers = json!({"slow":slow});
    if include_direct {
        servers["fixture"] = fixture.http.entry("/mcp");
    }
    std::fs::write(
        fixture.agent.join("mcp.json"),
        json!({"mcpServers":servers}).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn slow_product_inventory_stays_frozen_midturn_and_publishes_at_idle_admission() {
    let fixture = ProductFixture::new(false).await;
    configure_slow_server(&fixture, true);
    let manager = fixture.manager();
    let session = fixture.create(&manager, "slow-product.jsonl").await;
    fixture.http.until(|trace| trace.slow_listing).await;
    assert!(!has_slow_tool(&session.current()));
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);
    // Reload must reuse the pending indirect attempt as well as the ready direct connection.
    session.reload().await.unwrap();
    let active = session.current();
    let tools = active.catalog().tool_specs();
    assert!(!has_slow_tool(&active));
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);

    let provider_gate = Arc::new(Semaphore::new(0));
    *fixture.http.provider_gate.lock().unwrap() = Some(provider_gate.clone());
    let before_requests = fixture.http.snapshot().provider_requests;
    let turn = tokio::spawn({
        let active = active.clone();
        async move { active.prompt("hold this turn while MCP completes").await }
    });
    fixture
        .http
        .until(|trace| trace.provider_requests > before_requests)
        .await;
    assert!(!active.is_idle());
    fixture.http.slow_gate.add_permits(1);
    fixture.http.until(|trace| trace.slow_replied).await;
    assert!(Arc::ptr_eq(&active, &session.current()));
    assert_eq!(active.catalog().tool_specs(), tools);
    provider_gate.add_permits(1);
    tokio::time::timeout(TIMEOUT, turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(active.is_idle());
    assert!(
        Arc::ptr_eq(&active, &session.current()),
        "completion itself must not publish a registry"
    );
    assert_eq!(active.catalog().tool_specs(), tools);

    // Product readiness is private. The server reply above does not prove the
    // client has committed it, so drive bounded *semantic idle admissions*, not
    // sleeps or a guessed number of scheduler yields, until the tool is published.
    tokio::time::timeout(TIMEOUT, async {
        while !has_slow_tool(&session.current()) {
            session
                .current()
                .prompt("next idle admission")
                .await
                .unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!Arc::ptr_eq(&active, &session.current()));
    assert_eq!(
        active.catalog().tool_specs(),
        tools,
        "the old generation stays immutable"
    );
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);
    assert!(fixture.http.snapshot().deleted.is_empty());
    manager.shutdown().await.unwrap();
    fixture
        .http
        .deleted(&["connection-1", "connection-2"])
        .await;
}

#[tokio::test]
async fn observed_library_readiness_publishes_on_the_first_next_idle_submission() {
    // Precise readiness-before-admission evidence at the public library/session
    // seam; this does not pretend to expose ProductSessionFactory's private watch.
    let fixture = ProductFixture::new(false).await;
    configure_slow_server(&fixture, false);
    let library = McpLibrary::new(&fixture.agent, Some(&fixture.project), false);
    let owner = Arc::new(Mutex::new(None::<pi_plugin_mcp::WeakMcpSupervisor>));
    let generations = Arc::new(AtomicUsize::new(0));
    let manager = MultiSessionManager::new({
        let owner = owner.clone();
        let generations = generations.clone();
        move |request: SessionGenerationRequest| {
            let owner = owner.clone();
            let generations = generations.clone();
            let library = library.clone();
            async move {
                // This one-session test factory has no persisted-path identity or
                // strong owner cache; only Reload/automatic refresh are exercised.
                let supervisor = owner
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|weak| weak.upgrade())
                    .unwrap_or_default();
                *owner.lock().unwrap() = Some(supervisor.downgrade());
                let prepared = library
                    .prepare_supervised_generation(supervisor, BTreeMap::new())
                    .await
                    .map_err(SessionError::Runtime)?;
                let readiness = prepared.readiness_watch.expect("supervised lease");
                let runtime = PiRuntime::builder()
                    .plugin_arc(prepared.plugin)
                    .provider_plugin(ScriptedProviderPlugin::scripted([
                        ScriptedTurn::Text("done".into()),
                        ScriptedTurn::Text("still current".into()),
                    ]))
                    .agent_options(AgentOptions {
                        cwd: request.cwd,
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        ..AgentOptions::default()
                    })
                    .build()?;
                generations.fetch_add(1, Ordering::AcqRel);
                Ok(PreparedSessionGeneration::new(
                    runtime,
                    AgentSessionOptions::default()
                        .generation_freshness(Arc::new(move || Ok(readiness.changed()))),
                ))
            }
        }
    });
    let session = manager
        .create_session(CreateSessionOptions::new(
            &fixture.project,
            fixture.agent.join("precise-idle.jsonl"),
        ))
        .await
        .unwrap();
    let active = session.current();
    fixture.http.until(|trace| trace.slow_listing).await;
    assert!(!has_slow_tool(&active));
    let weak = owner.lock().unwrap().as_ref().unwrap().clone();
    let baseline = weak.upgrade().unwrap().revision();
    fixture.http.slow_gate.add_permits(1);
    // Unlike response-sent, this public revision observes the supervisor's actual
    // readiness transition. No strong owner crosses an await or the final shutdown.
    tokio::time::timeout(TIMEOUT, async {
        while weak.upgrade().unwrap().revision() == baseline {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(Arc::ptr_eq(&active, &session.current()));
    assert!(!has_slow_tool(&active));
    assert_eq!(generations.load(Ordering::Acquire), 1);
    active.prompt("first next idle submission").await.unwrap();
    assert!(!Arc::ptr_eq(&active, &session.current()));
    assert!(has_slow_tool(&session.current()));
    assert!(!has_slow_tool(&active));
    assert_eq!(generations.load(Ordering::Acquire), 2);
    let published = session.current();
    published
        .prompt("no additional readiness change")
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&published, &session.current()));
    assert_eq!(
        generations.load(Ordering::Acquire),
        2,
        "publication must not stale its own snapshot"
    );
    assert_eq!(fixture.http.snapshot().initialized.len(), 1);
    assert!(fixture.http.snapshot().deleted.is_empty());
    manager.shutdown().await.unwrap();
    fixture.http.deleted(&["connection-1"]).await;
    assert!(weak.upgrade().is_none());
}
