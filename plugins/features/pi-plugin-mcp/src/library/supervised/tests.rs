use super::*;
mod auth;
mod runtime;
use axum::response::IntoResponse;
use futures::FutureExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

struct Fixture {
    url: String,
    starts: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(blocked: bool, status: u16, duplicate: bool) -> Self {
        let starts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Semaphore::new(usize::from(!blocked)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = axum::Router::new().route("/mcp", axum::routing::post({
            let starts = starts.clone();
            let gate = gate.clone();
            move |axum::Json(request): axum::Json<Value>| {
                let starts = starts.clone();
                let gate = gate.clone();
                async move {
                    if status == 401 {
                        return (http::StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer realm=fixture")]).into_response();
                    }
                    let id = &request["id"];
                    if id.is_null() { return http::StatusCode::ACCEPTED.into_response(); }
                    let result = match request["method"].as_str().unwrap() {
                        "server/discover" => return axum::Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"legacy fixture"}})).into_response(),
                        "initialize" => {
                            let attempt = starts.fetch_add(1, Ordering::AcqRel);
                            if attempt == 0 { gate.acquire().await.unwrap().forget(); }
                            if status != 200 { return http::StatusCode::from_u16(status).unwrap().into_response(); }
                            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"supervised-library-fixture","version":"1"}})
                        }
                        "tools/list" => {
                            let mut tools = vec![json!({"name":"echo","inputSchema":{"type":"object"}})];
                            if duplicate { tools.push(tools[0].clone()); }
                            json!({"tools":tools})
                        }
                        "tools/call" => json!({"content":[{"type":"text","text":"still connected"}]}),
                        other => panic!("unexpected protocol request: {other}"),
                    };
                    axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
                }
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            starts,
            gate,
            task,
        }
    }

    fn entry(&self, exposure: &str) -> Value {
        json!({"url":self.url,"exposure":exposure})
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn configure(library: &McpLibrary, servers: Value) {
    std::fs::write(
        library.path(McpScope::Global).unwrap(),
        json!({"mcpServers":servers}).to_string(),
    )
    .unwrap();
}

async fn prepare(library: &McpLibrary, supervisor: &McpSupervisor) -> PreparedMcpGeneration {
    library
        .prepare_supervised_with_budget(supervisor.clone(), BTreeMap::new(), Duration::from_secs(3))
        .await
        .unwrap()
}

async fn inventory(
    supervisor: &McpSupervisor,
    name: &str,
) -> Arc<crate::supervisor::ConnectionInventory> {
    let snapshot = supervisor
        .wait_for(&[name.into()], Duration::from_secs(3))
        .await;
    let ConnectionReadiness::Ready(inventory) = &snapshot.servers[name] else {
        panic!("{name} not ready")
    };
    inventory.clone()
}

#[tokio::test]
async fn indirect_only_is_immediately_ready_without_waiting_and_empty_has_lease() {
    let fixture = Fixture::new(true, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"slow":fixture.entry("codemode")}));
    let supervisor = McpSupervisor::new();
    let prepared = library
        .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
        .now_or_never()
        .expect("indirect preparation must never wait")
        .unwrap();
    assert!(matches!(
        prepared.server_readiness["slow"],
        McpServerReadiness::Pending
    ));
    let watch = prepared.readiness_watch.as_ref().unwrap();
    assert!(watch.downgrade().active_supervisor().is_none());
    fixture.gate.add_permits(1);
    inventory(&supervisor, "slow").await;
    assert!(watch.changed());
    assert!(matches!(
        prepared.server_readiness["slow"],
        McpServerReadiness::Pending
    ));
    let next = prepare(&library, &supervisor).await;
    assert!(matches!(
        next.server_readiness["slow"],
        McpServerReadiness::Ready
    ));
    assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    configure(&library, json!({}));
    let empty = prepare(&library, &supervisor).await;
    assert!(empty.readiness_watch.is_some());
    assert!(empty.server_readiness.is_empty());
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn ready_direct_does_not_wait_for_slow_indirect_and_reports_mixed_states() {
    let slow = Fixture::new(true, 200, false).await;
    let healthy = Fixture::new(false, 200, false).await;
    let auth = Fixture::new(false, 401, false).await;
    let failed = Fixture::new(false, 400, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(
        &library,
        json!({
            "slow":slow.entry("codemode"), "ready":healthy.entry("direct"),
            "auth":auth.entry("direct"), "failed":failed.entry("direct"),
            "resolution":{"url":"$PI_MCP_UNSET_SUPERVISED_FIXTURE_URL_84701","exposure":"direct"},
            "disabled":{"enabled":false}
        }),
    );
    assert!(library.credential_watch().is_err());
    assert!(library.supervised_credential_watch().is_ok());
    let supervisor = McpSupervisor::new();
    let prepared = tokio::time::timeout(Duration::from_secs(1), prepare(&library, &supervisor))
        .await
        .expect("slow indirect must not consume the three-second direct wait budget");
    assert!(matches!(
        prepared.server_readiness["slow"],
        McpServerReadiness::Pending
    ));
    assert!(matches!(
        prepared.server_readiness["ready"],
        McpServerReadiness::Ready
    ));
    assert!(matches!(
        prepared.server_readiness["auth"],
        McpServerReadiness::NeedsAuth(_)
    ));
    assert!(matches!(
        prepared.server_readiness["failed"],
        McpServerReadiness::Failed(_)
    ));
    assert!(matches!(
        prepared.server_readiness["resolution"],
        McpServerReadiness::Failed(_)
    ));
    assert!(matches!(
        prepared.server_readiness["disabled"],
        McpServerReadiness::Disabled
    ));
    assert_eq!(prepared.authorization_required[0].server, "auth");
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![prepared.plugin])
        .unwrap();
    assert!(
        registry
            .tool_infos()
            .iter()
            .any(|tool| tool.spec.name == "mcp__ready__echo")
    );
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn multiple_direct_servers_share_one_startup_bound() {
    let fixtures = [
        Fixture::new(true, 200, false).await,
        Fixture::new(true, 200, false).await,
        Fixture::new(true, 200, false).await,
    ];
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(
        &library,
        json!({"a":fixtures[0].entry("direct"),"b":fixtures[1].entry("direct"),"c":fixtures[2].entry("direct")}),
    );
    let supervisor = McpSupervisor::new();
    let prepared = tokio::time::timeout(
        Duration::from_millis(200),
        library.prepare_supervised_with_budget(
            supervisor.clone(),
            BTreeMap::new(),
            Duration::from_millis(100),
        ),
    )
    .await
    .expect("must not spend one budget per direct server")
    .unwrap();
    for name in ["a", "b", "c"] {
        assert!(matches!(
            prepared.server_readiness[name],
            McpServerReadiness::Pending
        ));
    }
    for fixture in &fixtures {
        assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    }
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn direct_override_reclassifies_existing_attempt_without_reconnecting() {
    let fixture = Fixture::new(true, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"server":fixture.entry("hidden")}));
    let supervisor = McpSupervisor::new();
    let first = prepare(&library, &supervisor).await;
    let mut entry = fixture.entry("hidden");
    entry["toolExposure"] = json!({"echo":"direct"});
    configure(&library, json!({"server":entry}));
    let waiting = library.prepare_supervised_with_budget(
        supervisor.clone(),
        BTreeMap::new(),
        Duration::from_secs(3),
    );
    tokio::pin!(waiting);
    assert!(
        futures::poll!(&mut waiting).is_pending(),
        "direct override must wait"
    );
    fixture.gate.add_permits(1);
    let second = waiting.await.unwrap();
    assert!(matches!(
        first.server_readiness["server"],
        McpServerReadiness::Pending
    ));
    assert!(matches!(
        second.server_readiness["server"],
        McpServerReadiness::Ready
    ));
    let original = inventory(&supervisor, "server").await;
    let mut entry = fixture.entry("deferred");
    entry["description"] = json!("policy only");
    entry["timeout"] = json!(7);
    configure(&library, json!({"server":entry}));
    let third = prepare(&library, &supervisor).await;
    assert!(Arc::ptr_eq(
        &original.connection,
        &inventory(&supervisor, "server").await.connection
    ));
    assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![third.plugin])
        .unwrap();
    assert_eq!(
        registry
            .tool_infos()
            .iter()
            .find(|tool| tool.spec.name == "mcp__server__echo")
            .unwrap()
            .discovery
            .exposure,
        pi_plugin::ToolExposure::Deferred
    );
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn candidate_projection_failure_and_name_collision_keep_healthy_transport() {
    let healthy = Fixture::new(false, 200, false).await;
    let duplicate = Fixture::new(false, 200, true).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"healthy":healthy.entry("direct")}));
    let supervisor = McpSupervisor::new();
    let old = prepare(&library, &supervisor).await;
    // Model the host's completed publication, not another uncommitted candidate.
    old.readiness_watch.as_ref().unwrap().downgrade().publish();
    let original = inventory(&supervisor, "healthy").await;
    configure(
        &library,
        json!({"healthy":healthy.entry("direct"),"duplicate":duplicate.entry("direct")}),
    );
    assert!(
        library
            .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
            .await
            .is_err()
    );
    assert!(!original.connection.is_closed());
    assert!(Arc::ptr_eq(
        &original.connection,
        &inventory(&supervisor, "healthy").await.connection
    ));
    configure(
        &library,
        json!({"healthy":healthy.entry("direct"),"server-a":duplicate.entry("hidden"),"server_a":{"url":"$PI_MCP_UNSET_SUPERVISED_FIXTURE_URL_84701"}}),
    );
    assert!(
        library
            .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
            .await
            .is_err()
    );
    assert!(!original.connection.is_closed());
    assert_eq!(healthy.starts.load(Ordering::Acquire), 1);
    drop(old);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn plugin_registry_and_library_never_keep_supervisor_alive() {
    let fixture = Fixture::new(false, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"server":fixture.entry("direct")}));
    let supervisor = McpSupervisor::new();
    let weak = supervisor.downgrade();
    let prepared = prepare(&library, &supervisor).await;
    let connection = inventory(&supervisor, "server").await.connection.clone();
    let watch = prepared.readiness_watch.unwrap();
    let lease = watch.downgrade();
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![prepared.plugin.clone()])
        .unwrap();
    drop(supervisor);
    assert!(weak.upgrade().is_some());
    drop(watch);
    assert!(weak.upgrade().is_none());
    assert!(lease.is_retired());
    tokio::time::timeout(Duration::from_secs(3), async {
        while !connection.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!registry.tool_infos().is_empty());
    drop(prepared.plugin);
}

#[tokio::test]
async fn invalidate_during_direct_wait_rejects_stale_candidate() {
    let fixture = Fixture::new(true, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"server":fixture.entry("direct")}));
    let supervisor = McpSupervisor::new();
    let pending = library.prepare_supervised_generation(supervisor.clone(), BTreeMap::new());
    tokio::pin!(pending);
    assert!(futures::poll!(&mut pending).is_pending());
    supervisor.invalidate("server");
    assert!(pending.await.is_err());
    assert!(supervisor.snapshot().servers.is_empty());
    supervisor.close().await.unwrap();
}
