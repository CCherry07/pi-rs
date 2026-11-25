//! Real Streamable HTTP notification -> frozen library -> session idle admission.
//! This is exact public-library evidence, not a claim about observing the private
//! ProductSessionFactory readiness watch. Pi's runtime.ts refreshes on the same
//! wire notification; immutable next-idle publication is the deliberate Rust seam.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{AbortSignal, ModelId, PluginId, ProviderId, ToolSpec};
use pi_plugin::{
    Provider, ProviderCallContext, ProviderError, ProviderPlugin, ProviderRegisterContext,
    ProviderRequest, ProviderStream,
};
use pi_plugin_mcp::{McpLibrary, WeakMcpSupervisor};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, CreateSessionOptions, MultiSessionManager, PreparedSessionGeneration,
    SessionError, SessionGenerationRequest,
};
use pi_test_support::{ScriptedProvider, ScriptedTurn};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc, watch};

const TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_GUIDANCE: &str = "Initialize-only notification fixture guidance.";
const NEXT_GUIDANCE: &str = "Different guidance requires another initialize.";
const SESSION: &str = "notification-connection";

#[derive(Clone, Debug, Default)]
struct Trace {
    initializes: usize,
    subscriptions: usize,
    lists: Vec<String>,
    calls: usize,
    deletes: usize,
}

struct Remote {
    trace: watch::Sender<Trace>,
    tools: Mutex<Vec<Value>>,
    instructions: Mutex<String>,
    notifications: Mutex<Option<mpsc::UnboundedSender<()>>>,
    list_gate: Semaphore,
}

struct HttpFixture {
    url: String,
    remote: Arc<Remote>,
    task: tokio::task::JoinHandle<()>,
}

impl HttpFixture {
    async fn new() -> Self {
        let remote = Arc::new(Remote {
            trace: watch::channel(Trace::default()).0,
            tools: Mutex::new(vec![tool("retired", "string"), tool("edited", "string")]),
            instructions: Mutex::new(INITIAL_GUIDANCE.into()),
            notifications: Mutex::new(None),
            list_gate: Semaphore::new(1),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn({
            let remote = remote.clone();
            async move {
                // Dropping this JoinSet cancels accepted SSE connections too.
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        connection = listener.accept() => {
                            let (socket, _) = connection.unwrap();
                            let remote = remote.clone();
                            connections.spawn(async move { serve(socket, remote).await });
                        }
                        result = connections.join_next(), if !connections.is_empty() => {
                            result.unwrap().unwrap();
                        }
                    }
                }
            }
        });
        Self { url, remote, task }
    }

    async fn until(&self, predicate: impl Fn(&Trace) -> bool) {
        let mut receiver = self.remote.trace.subscribe();
        tokio::time::timeout(TIMEOUT, receiver.wait_for(predicate))
            .await
            .unwrap_or_else(|_| panic!("missing fixture event: {:?}", self.remote.trace.borrow()))
            .unwrap();
    }

    fn change_and_notify(&self) {
        *self.remote.tools.lock().unwrap() =
            vec![tool("added", "boolean"), tool("edited", "number")];
        *self.remote.instructions.lock().unwrap() = NEXT_GUIDANCE.into();
        // This sends bytes on the existing HTTP GET stream, never an in-process
        // MCP handler callback, readiness bump, or fake generation signal.
        self.remote
            .notifications
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(())
            .unwrap();
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn tool(name: &str, kind: &str) -> Value {
    json!({"name":name,"inputSchema":{"type":"object","properties":{"value":{"type":kind}},"required":["value"]}})
}

// Minimal loopback HTTP/1.1 fixture avoids adding a Stream/SSE dependency to
// Coding. Each JSON response closes its HTTP socket; the MCP session and standalone
// GET notification stream remain stable. Transport reuse is measured by initialize,
// exact session headers, and helper execution, not physical TCP connection pooling.
async fn serve(socket: TcpStream, remote: Arc<Remote>) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap() == 0 {
        return;
    }
    let method = line.split_whitespace().next().unwrap().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.trim().split_once(':').unwrap();
        headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map_or(0, |value| value.parse().unwrap());
    assert!(length < 128 * 1024);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    let mut socket = reader.into_inner();
    assert_eq!(
        headers.get("x-open-counter").map(String::as_str),
        Some("fixture")
    );
    if method == "GET" {
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION)
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        *remote.notifications.lock().unwrap() = Some(tx);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: ready\n\n").await.unwrap();
        remote.trace.send_modify(|trace| trace.subscriptions += 1);
        while rx.recv().await.is_some() {
            socket.write_all(b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n").await.unwrap();
        }
        return;
    }
    if method == "DELETE" {
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION)
        );
        remote.trace.send_modify(|trace| trace.deletes += 1);
        remote.notifications.lock().unwrap().take();
        respond(&mut socket, "204 No Content", None, "").await;
        return;
    }
    assert_eq!(method, "POST");
    let request: Value = serde_json::from_slice(&body).unwrap();
    let id = &request["id"];
    let method = request["method"].as_str().unwrap();
    if !matches!(method, "initialize" | "server/discover") {
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION)
        );
    }
    if id.is_null() {
        respond(&mut socket, "202 Accepted", None, "").await;
        return;
    }
    let mut session_header = None;
    let result = match method {
        "server/discover" => {
            let error =
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Use initialize"}});
            respond(&mut socket, "200 OK", None, &error.to_string()).await;
            return;
        }
        "initialize" => {
            remote.trace.send_modify(|trace| trace.initializes += 1);
            session_header = Some(SESSION);
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"notification-fixture","version":"1"},"instructions":remote.instructions.lock().unwrap().clone()})
        }
        "tools/list" => {
            let tools = remote.tools.lock().unwrap().clone();
            remote
                .trace
                .send_modify(|trace| trace.lists.push(headers["mcp-session-id"].clone()));
            remote.list_gate.acquire().await.unwrap().forget();
            json!({"tools":tools})
        }
        "tools/call" => {
            remote.trace.send_modify(|trace| trace.calls += 1);
            panic!("notification regression must never execute an MCP tool");
        }
        other => panic!("unexpected MCP method {other}"),
    };
    let body = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
    respond(&mut socket, "200 OK", session_header, &body).await;
}

async fn respond(socket: &mut TcpStream, status: &str, session: Option<&str>, body: &str) {
    let session = session
        .map(|id| format!("Mcp-Session-Id: {id}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{session}Connection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

struct GatedProvider {
    inner: ScriptedProvider,
    requests: Mutex<Vec<ProviderRequest>>,
    entered: Semaphore,
    release: Semaphore,
}

#[async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let first = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len() == 1
        };
        if first {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        self.inner.stream(request, context, signal).await
    }
}

struct ProviderPluginFixture(Arc<GatedProvider>);

#[pi_plugin::provider_plugin]
impl ProviderPlugin for ProviderPluginFixture {
    fn id(&self) -> PluginId {
        PluginId::new("gated-notification-provider")
    }
    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(self.0.clone())
    }
}

fn assert_inventory(specs: &[ToolSpec], changed: bool) {
    let mut names = specs
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        if changed {
            vec!["mcp__fixture__added", "mcp__fixture__edited"]
        } else {
            vec!["mcp__fixture__edited", "mcp__fixture__retired"]
        }
    );
    let edited = specs
        .iter()
        .find(|tool| tool.name == "mcp__fixture__edited")
        .unwrap();
    assert_eq!(
        edited.parameters["properties"]["value"]["type"],
        if changed { "number" } else { "string" }
    );
}

#[tokio::test]
async fn real_notification_refresh_publishes_once_at_first_observed_next_idle() {
    let http = HttpFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let project = directory.path().join("project");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(agent.join("mcp.json"), json!({"mcpServers":{"fixture":{
        "url":http.url,"exposure":"direct","headers":{"X-Open-Counter":"!printf x >> notification-helper-count; printf fixture"}
    }}}).to_string()).unwrap();
    let provider = Arc::new(GatedProvider {
        inner: ScriptedProvider::new(
            ProviderId::new("scripted"),
            ModelId::new("test"),
            [
                ScriptedTurn::Text("active finished".into()),
                ScriptedTurn::Text("new snapshot".into()),
                ScriptedTurn::Text("same snapshot".into()),
            ],
        ),
        requests: Mutex::new(Vec::new()),
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let library = McpLibrary::new(&agent, Some(&project), false).with_command_values(true);
    let owner = Arc::new(Mutex::new(None::<WeakMcpSupervisor>));
    let generations = Arc::new(AtomicUsize::new(0));
    let manager = MultiSessionManager::new({
        let owner = owner.clone();
        let generations = generations.clone();
        let provider = provider.clone();
        move |request: SessionGenerationRequest| {
            let (owner, generations, provider, library) = (
                owner.clone(),
                generations.clone(),
                provider.clone(),
                library.clone(),
            );
            async move {
                // One session, initial generation and idle refresh only. The
                // factory cache is weak; only each session's freshness lease owns.
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
                assert!(
                    matches!(
                        prepared.server_readiness.get("fixture"),
                        Some(pi_plugin_mcp::McpServerReadiness::Ready)
                    ),
                    "direct fixture must be ready: {:?}",
                    prepared.server_readiness,
                );
                let readiness = prepared.readiness_watch.expect("supervised lease");
                let runtime = PiRuntime::builder()
                    .all_tools_active()
                    .plugin_arc(prepared.plugin)
                    .provider_plugin(ProviderPluginFixture(provider))
                    .agent_options(AgentOptions {
                        cwd: request.cwd,
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        ..AgentOptions::default()
                    })
                    .build()?;
                // Same user/direct-extension activation choice as Coding's
                // additional_active_tools, not a test-side setter or grant bypass.
                let options = AgentSessionOptions::default()
                    .additional_active_tools(runtime.active_tools())
                    .generation_freshness(Arc::new(move || Ok(readiness.changed())));
                generations.fetch_add(1, Ordering::AcqRel);
                Ok(PreparedSessionGeneration::new(runtime, options))
            }
        }
    });
    let session = manager
        .create_session(CreateSessionOptions::new(
            &project,
            agent.join("notification.jsonl"),
        ))
        .await
        .unwrap();
    http.until(|trace| trace.subscriptions == 1).await;
    let active = session.current();
    let original_specs = active.catalog().tool_specs();
    assert_inventory(&original_specs, false);
    assert_eq!(generations.load(Ordering::Acquire), 1);
    let weak = owner.lock().unwrap().as_ref().unwrap().clone();
    let baseline = weak.upgrade().unwrap().revision();
    let turn = tokio::spawn({
        let active = active.clone();
        async move { active.prompt("hold this active turn").await }
    });
    tokio::time::timeout(TIMEOUT, provider.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(!active.is_idle());
    assert_inventory(&provider.requests.lock().unwrap()[0].tools, false);
    http.change_and_notify();
    http.until(|trace| trace.lists.len() == 2).await;
    assert_eq!(active.catalog().tool_specs(), original_specs);
    http.remote.list_gate.add_permits(1);
    // Observe the real client's completed refresh before releasing the provider.
    // A JSON reply written by the fixture would not establish this fact.
    tokio::time::timeout(TIMEOUT, async {
        while weak.upgrade().unwrap().revision() == baseline {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!active.is_idle());
    assert!(Arc::ptr_eq(&active, &session.current()));
    assert_eq!(active.catalog().tool_specs(), original_specs);
    assert_eq!(generations.load(Ordering::Acquire), 1);
    provider.release.add_permits(1);
    tokio::time::timeout(TIMEOUT, turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(active.is_idle());
    assert!(
        Arc::ptr_eq(&active, &session.current()),
        "turn completion is not a publication boundary"
    );
    active.prompt("first next idle submission").await.unwrap();
    let published = session.current();
    assert!(!Arc::ptr_eq(&active, &published));
    assert_inventory(&published.catalog().tool_specs(), true);
    assert_eq!(active.catalog().tool_specs(), original_specs);
    assert_eq!(generations.load(Ordering::Acquire), 2);
    published
        .prompt("second idle must not rebuild")
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&published, &session.current()));
    assert_eq!(generations.load(Ordering::Acquire), 2);
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    for (index, request) in requests.iter().enumerate() {
        assert_inventory(&request.tools, index > 0);
        assert!(request.system_prompt.contains(INITIAL_GUIDANCE));
        assert!(!request.system_prompt.contains(NEXT_GUIDANCE));
    }
    let trace = http.remote.trace.borrow().clone();
    assert_eq!(trace.initializes, 1);
    assert_eq!(trace.subscriptions, 1);
    assert_eq!(trace.lists, [SESSION, SESSION]);
    assert_eq!(trace.calls, 0);
    assert_eq!(trace.deletes, 0);
    assert_eq!(
        std::fs::read(project.join("notification-helper-count")).unwrap(),
        b"x"
    );
    manager.shutdown().await.unwrap();
    http.until(|trace| trace.deletes == 1).await;
    assert!(weak.upgrade().is_none());
}
