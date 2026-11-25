use super::cases::{initialized, settled};
use super::*;

#[tokio::test]
async fn aborting_refresh_cancels_only_that_request_and_transport_remains_usable() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let inventory = initialized(&fixture, &supervisor).await;
    let task = tokio::spawn({
        let connection = inventory.connection.clone();
        async move { connection.refresh_tools(Duration::from_secs(30)).await }
    });
    fixture.listed(2).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.state.cancelled_lists.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("server did not observe request cancellation");
    // The next list uses the same transport after control cleanup; no initialization replay.
    fixture.state.gate.add_permits(1);
    let tools = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match inventory
                .connection
                .refresh_tools(Duration::from_secs(3))
                .await
            {
                Ok(tools) => break tools,
                Err(error) => assert!(
                    error.to_string().contains("cancellation incomplete"),
                    "{error}"
                ),
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(tools, inventory.tools);
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    assert!(!inventory.connection.is_closed());
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_fences_old_notifications_and_both_epoch_and_connection_completion() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let original = initialized(&fixture, &supervisor).await;
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    let old_slot = supervisor.inner.state.lock().unwrap().entries["server"].clone();
    let old_epoch = old_slot.state.lock().unwrap().epoch;
    fixture.notify().await;
    fixture.listed(2).await;
    *fixture.state.tools.lock().unwrap() = vec![tool("after_reconnect", "number")];
    *fixture.state.instructions.lock().unwrap() = "new instructions".into();
    supervisor.reconnect("server").unwrap();
    closed(&original.connection).await;
    fixture.listed(3).await;
    // Release both retired server work and current discovery; the old response must be ignored.
    fixture.state.gate.add_permits(2);
    let replacement = ready(&supervisor, "server").await;
    assert_eq!(
        replacement
            .connection
            .metadata()
            .unwrap()
            .instructions
            .as_deref(),
        Some("new instructions")
    );
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 2);
    let new_slot = supervisor.inner.state.lock().unwrap().entries["server"].clone();
    let epoch = new_slot.state.lock().unwrap().epoch;
    let baseline = supervisor.revision();
    assert!(!old_slot.complete_refresh(
        old_epoch,
        &original.connection,
        99,
        Ok(vec![tool("stale", "string")])
    ));
    assert!(!new_slot.complete_refresh(
        epoch,
        &original.connection,
        99,
        Ok(vec![tool("wrong_connection", "string")])
    ));
    assert!(!new_slot.complete_refresh(
        epoch + 1,
        &replacement.connection,
        99,
        Ok(vec![tool("wrong_epoch", "string")])
    ));
    assert_eq!(supervisor.revision(), baseline);
    assert_eq!(
        ready(&supervisor, "server").await.tools[0].name,
        "after_reconnect"
    );
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn historical_refresh_can_conservatively_dirty_candidate_but_drain_stops_old_worker() {
    let old_fixture = ChangingFixture::new().await;
    let new_fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old_inventory = initialized(&old_fixture, &supervisor).await;
    let old_source = generation(supervisor.clone());
    old_source.downgrade().publish();
    let old_slot = Arc::downgrade(&supervisor.inner.state.lock().unwrap().entries["server"]);
    let new_inventory = initialized(&new_fixture, &supervisor).await;
    let candidate = generation(supervisor.clone());
    *old_fixture.state.tools.lock().unwrap() = vec![tool("historical_update", "number")];
    old_fixture.notify().await;
    old_fixture.listed(2).await;
    old_fixture.state.gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !old_slot
            .upgrade()
            .is_some_and(|slot| slot.state.lock().unwrap().refresh_seen == 1)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        candidate.changed(),
        "global revision conservatively includes published historical changes"
    );
    candidate.downgrade().publish();
    drop(old_source);
    closed(&old_inventory.connection).await;
    assert!(
        old_slot.upgrade().is_none(),
        "retained raw inventory cannot pin the old refresh worker"
    );
    let next_idle = generation(supervisor.clone());
    next_idle.downgrade().publish();
    drop(candidate);
    assert!(!next_idle.changed());
    assert!(!new_inventory.connection.is_closed());
    assert_eq!(old_fixture.state.lists.load(Ordering::Acquire), 2);
    assert_eq!(new_fixture.state.lists.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn projected_refresh_error_keeps_tools_and_reload_never_reruns_header_helper() {
    let fixture = ChangingFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let crate::McpTransport::Http { url, .. } = &fixture.config.transport else {
        panic!()
    };
    std::fs::write(directory.path().join("mcp.json"), json!({"mcpServers":{"server":{
        "url":url,"exposure":"direct","headers":{"Authorization":"!printf hit >> helper_calls; printf 'Bearer test'"}
    }}}).to_string()).unwrap();
    let library = crate::McpLibrary::new(directory.path(), None, false).with_command_values(true);
    let supervisor = McpSupervisor::new();
    fixture.state.gate.add_permits(1);
    let old = library
        .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
        .await
        .unwrap();
    old.readiness_watch.as_ref().unwrap().downgrade().publish();
    let (_, old_registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![old.plugin])
        .unwrap();
    let unchanged = library
        .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
        .await
        .unwrap();
    drop(unchanged);
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    fixture.state.fail.store(true, Ordering::Release);
    fixture.notify().await;
    fixture.listed(2).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 1).await;
    let failed = library
        .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
        .await
        .unwrap();
    assert!(matches!(
        failed.server_readiness["server"],
        crate::McpServerReadiness::ReadyWithRefreshError(_)
    ));
    assert_eq!(
        failed.server_readiness["server"].state(),
        crate::McpServerState::Connected
    );
    assert!(
        !failed.server_readiness["server"]
            .diagnostic()
            .unwrap()
            .contains("private-token")
    );
    let (_, failed_registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![failed.plugin])
        .unwrap();
    assert!(failed_registry.tool("mcp__server__original").is_some());
    fixture.state.fail.store(false, Ordering::Release);
    *fixture.state.tools.lock().unwrap() = vec![tool("replacement", "number")];
    fixture.notify().await;
    fixture.listed(3).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 2).await;
    assert!(old_registry.tool("mcp__server__original").is_some());
    assert!(old_registry.tool("mcp__server__replacement").is_none());
    let next = library
        .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
        .await
        .unwrap();
    let (_, next_registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![next.plugin])
        .unwrap();
    assert!(next_registry.tool("mcp__server__replacement").is_some());
    assert!(next_registry.tool("mcp__server__original").is_none());
    assert_eq!(
        std::fs::read(directory.path().join("helper_calls")).unwrap(),
        b"hit"
    );
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}
