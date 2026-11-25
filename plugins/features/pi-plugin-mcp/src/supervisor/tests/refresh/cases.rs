use super::*;

pub(super) async fn initialized(
    fixture: &ChangingFixture,
    supervisor: &McpSupervisor,
) -> Arc<ConnectionInventory> {
    fixture.state.gate.add_permits(1);
    reconcile(supervisor, vec![fixture.input()]);
    ready(supervisor, "server").await
}

pub(super) async fn received(connection: &McpConnection, revision: u64) {
    let (_, notifications) = connection.tool_changes().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while *notifications.borrow() < revision {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("notification not dispatched");
}

pub(super) async fn settled(supervisor: &McpSupervisor, revision: u64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let settled = supervisor.inner.state.lock().unwrap().entries["server"]
                .state
                .lock()
                .unwrap()
                .refresh_seen
                >= revision;
            if settled {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("inventory refresh not settled");
}

#[tokio::test]
async fn initial_discovery_notification_is_not_lost_before_worker_subscription() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input()]);
    fixture.listed(1).await;
    *fixture.state.tools.lock().unwrap() = vec![tool("after_initial_list_started", "number")];
    fixture.notify().await;
    // Protocol round-trip orders notification dispatch before releasing initial discovery.
    let peer = fixture.state.peer.lock().unwrap().clone().unwrap();
    peer.send_request(rmcp::model::ServerRequest::PingRequest(
        rmcp::model::PingRequest::default(),
    ))
    .await
    .unwrap();
    fixture.state.gate.add_permits(1);
    fixture.listed(2).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 1).await;
    assert_eq!(
        ready(&supervisor, "server").await.tools[0].name,
        "after_initial_list_started"
    );
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn burst_during_list_coalesces_to_one_follow_up_and_preserves_active_views() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let original = initialized(&fixture, &supervisor).await;
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    *fixture.state.tools.lock().unwrap() = vec![tool("first_update", "number")];
    fixture.notify().await;
    fixture.listed(2).await;
    *fixture.state.tools.lock().unwrap() = vec![tool("second_update", "boolean")];
    for _ in 0..16 {
        fixture.notify().await;
    }
    received(&original.connection, 17).await;
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 2);
    fixture.state.gate.add_permits(1);
    fixture.listed(3).await;
    assert_eq!(original.tools[0].name, "original");
    assert_eq!(
        ready(&supervisor, "server").await.tools[0].name,
        "first_update"
    );
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 17).await;
    assert_eq!(
        ready(&supervisor, "server").await.tools[0].name,
        "second_update"
    );
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 3);
    assert_eq!(fixture.state.peak.load(Ordering::Acquire), 1);
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    assert!(source.changed());
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn empty_and_reordered_inventory_are_no_ops_but_schema_changes_are_not() {
    for tools in [vec![], vec![tool("a", "string"), tool("b", "string")]] {
        let fixture = ChangingFixture::new().await;
        *fixture.state.tools.lock().unwrap() = tools;
        let supervisor = McpSupervisor::new();
        let original = initialized(&fixture, &supervisor).await;
        let source = generation(supervisor.clone());
        source.downgrade().publish();
        fixture.state.tools.lock().unwrap().reverse();
        fixture.notify().await;
        fixture.listed(2).await;
        fixture.state.gate.add_permits(1);
        settled(&supervisor, 1).await;
        assert!(!source.changed());
        assert!(Arc::ptr_eq(&original, &ready(&supervisor, "server").await));
        *fixture.state.tools.lock().unwrap() = vec![tool("a", "number")];
        fixture.notify().await;
        fixture.listed(3).await;
        fixture.state.gate.add_permits(1);
        settled(&supervisor, 2).await;
        assert!(source.changed());
        assert_eq!(
            ready(&supervisor, "server").await.tools[0].input_schema["properties"]["value"]["type"],
            "number"
        );
        supervisor.close().await.unwrap();
    }
}

#[tokio::test]
async fn refresh_uses_all_sdk_pages_before_replacing_inventory() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    *fixture.state.tools.lock().unwrap() = vec![tool("first", "number"), tool("last", "boolean")];
    fixture.state.paginated.store(true, Ordering::Release);
    fixture.notify().await;
    fixture.listed(2).await;
    fixture.state.gate.add_permits(1);
    fixture.listed(3).await;
    assert!(Arc::ptr_eq(&old, &ready(&supervisor, "server").await));
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 1).await;
    let refreshed = ready(&supervisor, "server").await;
    assert_eq!(
        refreshed
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        ["first", "last"]
    );
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn failed_refresh_retains_inventory_and_dirties_only_error_or_recovery_transitions() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    fixture.state.fail.store(true, Ordering::Release);
    fixture.notify().await;
    fixture.listed(2).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 1).await;
    let failed = ready(&supervisor, "server").await;
    assert!(
        failed
            .refresh_error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("last discovered tools retained")
    );
    assert!(
        !failed
            .refresh_error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("private-token")
    );
    assert_eq!(old.tools, failed.tools);
    assert!(!old.connection.is_closed());
    assert!(source.changed());
    let error_revision = supervisor.revision();
    fixture.notify().await;
    fixture.listed(3).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 2).await;
    assert_eq!(supervisor.revision(), error_revision);
    fixture.state.fail.store(false, Ordering::Release);
    fixture.notify().await;
    fixture.listed(4).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 3).await;
    assert!(ready(&supervisor, "server").await.refresh_error.is_none());
    assert_ne!(supervisor.revision(), error_revision);
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn final_source_drop_cancels_refresh_with_retained_executable_views() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    let pool = crate::McpToolSet::from_connections(
        vec![(
            old.connection.clone(),
            old.tools.clone(),
            crate::policy::McpServerPolicy::direct(),
        )],
        vec![],
    )
    .unwrap();
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    fixture.notify().await;
    fixture.listed(2).await;
    let weak = supervisor.downgrade();
    drop(supervisor);
    drop(source);
    assert!(weak.upgrade().is_none());
    closed(&old.connection).await;
    assert!(registry.tool("mcp__server__original").is_some());
}

#[tokio::test]
async fn candidate_drop_cancels_its_refresh_but_preserves_published_worker() {
    let original = ChangingFixture::new().await;
    let changed = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&original, &supervisor).await;
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    let discarded = initialized(&changed, &supervisor).await;
    let candidate = generation(supervisor.clone());
    changed.notify().await;
    changed.listed(2).await;
    drop(candidate);
    closed(&discarded.connection).await;
    assert!(Arc::ptr_eq(&old, &ready(&supervisor, "server").await));
    *original.state.tools.lock().unwrap() = vec![tool("continued", "number")];
    original.notify().await;
    original.listed(2).await;
    original.state.gate.add_permits(1);
    settled(&supervisor, 1).await;
    assert_eq!(
        ready(&supervisor, "server").await.tools[0].name,
        "continued"
    );
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn refresh_deadline_cancels_pending_request_without_closing_connection() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    let result = old.connection.refresh_tools(Duration::from_millis(5)).await;
    assert!(matches!(result, Err(McpError::ListTools { .. })));
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 2);
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.state.active.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed-out list must not remain alive on the server");
    assert!(!old.connection.is_closed());
    supervisor.close().await.unwrap();
}
