use super::*;
mod cases;
mod faults;
mod lifecycle;
mod modern;
use rmcp::model::{
    InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, Peer, RoleServer, ServerHandler};

#[derive(Clone)]
struct ChangingServer(Arc<ServerState>);

struct ServerState {
    tools: Mutex<Vec<RemoteTool>>,
    instructions: Mutex<String>,
    initializes: AtomicUsize,
    lists: AtomicUsize,
    fail: AtomicBool,
    paginated: AtomicBool,
    repeat_cursor: AtomicBool,
    active: AtomicUsize,
    cancelled_lists: AtomicUsize,
    peak: AtomicUsize,
    peer: Mutex<Option<Peer<RoleServer>>>,
    gate: Semaphore,
    modern: bool,
    subscription_allowed: AtomicBool,
    end_subscription: tokio::sync::Notify,
    discovers: AtomicUsize,
    subscriptions: AtomicUsize,
    active_subscriptions: AtomicUsize,
    sink: Mutex<Option<rmcp::service::SubscriptionSink>>,
}

impl ServerHandler for ChangingServer {
    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::DiscoverResult, ErrorData> {
        self.0.discovers.fetch_add(1, Ordering::AcqRel);
        if self.0.modern {
            Ok(rmcp::model::DiscoverResult::from_server_info(
                rmcp::model::ProtocolVersion::KNOWN_VERSIONS.to_vec(),
                self.get_info(),
            ))
        } else {
            Err(ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                "legacy fixture",
                None,
            ))
        }
    }

    fn accepted_subscription_filter(
        &self,
        requested: &rmcp::model::SubscriptionFilter,
    ) -> Option<rmcp::model::SubscriptionFilter> {
        self.0
            .subscription_allowed
            .load(Ordering::Acquire)
            .then(|| requested.supported_by(&self.get_info().capabilities))
    }

    async fn listen(&self, context: rmcp::service::SubscriptionContext) -> Result<(), ErrorData> {
        self.0.subscriptions.fetch_add(1, Ordering::AcqRel);
        self.0.active_subscriptions.fetch_add(1, Ordering::AcqRel);
        let _active = ActiveSubscription(self.0.clone());
        *self.0.sink.lock().unwrap() = Some(context.sink().clone());
        tokio::select! {
            () = context.cancelled() => (),
            () = self.0.end_subscription.notified() => (),
        }
        Ok(())
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_instructions(self.0.instructions.lock().unwrap().clone())
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        self.0.initializes.fetch_add(1, Ordering::AcqRel);
        *self.0.peer.lock().unwrap() = Some(context.peer.clone());
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = self.0.tools.lock().unwrap().clone();
        let fail = self.0.fail.load(Ordering::Acquire);
        let active = self.0.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.0.peak.fetch_max(active, Ordering::AcqRel);
        let _active = ActiveList(self.0.clone());
        self.0.lists.fetch_add(1, Ordering::AcqRel);
        tokio::select! {
            biased;
            () = context.ct.cancelled() => {
                self.0.cancelled_lists.fetch_add(1, Ordering::AcqRel);
                return Err(ErrorData::internal_error("fixture request cancelled", None));
            }
            permit = self.0.gate.acquire() => permit.unwrap().forget(),
        }
        if fail {
            return Err(ErrorData::internal_error(
                "private-token-do-not-expose",
                None,
            ));
        }
        let mut next_cursor = None;
        if self.0.paginated.load(Ordering::Acquire) && !tools.is_empty() {
            if request.and_then(|params| params.cursor).is_none() {
                tools.truncate(1);
                next_cursor = Some("rest".into());
            } else {
                tools.remove(0);
                if self.0.repeat_cursor.load(Ordering::Acquire) {
                    next_cursor = Some("rest".into());
                }
            }
        }
        Ok(ListToolsResult {
            tools,
            next_cursor,
            ..Default::default()
        })
    }
}

struct ActiveList(Arc<ServerState>);

impl Drop for ActiveList {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ActiveSubscription(Arc<ServerState>);
impl Drop for ActiveSubscription {
    fn drop(&mut self) {
        self.0.active_subscriptions.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ChangingFixture {
    state: Arc<ServerState>,
    config: McpServerConfig,
    task: JoinHandle<()>,
}

impl Drop for ChangingFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ChangingFixture {
    async fn new() -> Self {
        Self::with_protocol(false).await
    }

    async fn with_protocol(modern: bool) -> Self {
        let state = Arc::new(ServerState {
            tools: Mutex::new(vec![tool("original", "string")]),
            instructions: Mutex::new("initial instructions".into()),
            initializes: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            paginated: AtomicBool::new(false),
            repeat_cursor: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            cancelled_lists: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            peer: Mutex::new(None),
            gate: Semaphore::new(0),
            modern,
            subscription_allowed: AtomicBool::new(true),
            end_subscription: tokio::sync::Notify::new(),
            discovers: AtomicUsize::new(0),
            subscriptions: AtomicUsize::new(0),
            active_subscriptions: AtomicUsize::new(0),
            sink: Mutex::new(None),
        });
        let server = ChangingServer(state.clone());
        let service: StreamableHttpService<ChangingServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(server.clone()),
                Default::default(),
                StreamableHttpServerConfig::default(),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http(
            "server",
            format!("http://{}/mcp", listener.local_addr().unwrap()),
        )
        .headers(BTreeMap::from([(
            "Authorization".into(),
            "Bearer test".into(),
        )]));
        let app = axum::Router::new().nest_service("/mcp", service);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            config,
            task,
        }
    }

    fn input(&self) -> ConnectionInput {
        ConnectionInput::new(
            self.config.clone(),
            (&crate::policy::McpServerPolicy::direct()).into(),
            McpClientAuth::default(),
            [0; 32],
        )
        .unwrap()
    }

    async fn listed(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.state.lists.load(Ordering::Acquire) < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "tools/list #{count} did not start: listed={}, initialized={}",
                self.state.lists.load(Ordering::Acquire),
                self.state.initializes.load(Ordering::Acquire)
            )
        });
    }

    async fn notify(&self) {
        if self.state.modern {
            let sink = self.state.sink.lock().unwrap().clone().unwrap();
            sink.notify_tool_list_changed().await.unwrap();
        } else {
            let peer = self.state.peer.lock().unwrap().clone().unwrap();
            peer.notify_tool_list_changed().await.unwrap();
        }
    }
}

fn tool(name: &str, kind: &str) -> RemoteTool {
    serde_json::from_value(
        json!({"name":name, "inputSchema":{"type":"object","properties":{"value":{"type":kind}}}}),
    )
    .unwrap()
}

#[tokio::test]
async fn notification_refresh_uses_existing_transport_and_preserves_frozen_inventory() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input()]);
    fixture.listed(1).await;
    fixture.state.gate.add_permits(1);
    let original = ready(&supervisor, "server").await;
    let published = generation(supervisor.clone());
    published.downgrade().publish();
    *fixture.state.tools.lock().unwrap() = vec![tool("replacement", "number")];
    *fixture.state.instructions.lock().unwrap() = "new initialize instructions".into();
    fixture.notify().await;
    fixture.listed(2).await;
    fixture.state.gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !published.changed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let replacement = ready(&supervisor, "server").await;
    assert_eq!(replacement.tools[0].name, "replacement");
    assert_eq!(original.tools[0].name, "original");
    assert!(Arc::ptr_eq(&original.connection, &replacement.connection));
    assert_eq!(
        replacement
            .connection
            .metadata()
            .unwrap()
            .instructions
            .as_deref(),
        Some("initial instructions")
    );
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}
