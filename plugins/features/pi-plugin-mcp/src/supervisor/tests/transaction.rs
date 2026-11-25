use super::*;

async fn invoke(inventory: &ConnectionInventory) {
    let pool = crate::McpToolSet::from_connections(
        vec![(
            inventory.connection.clone(),
            inventory.tools.clone(),
            crate::policy::McpServerPolicy::direct(),
        )],
        vec![],
    )
    .unwrap();
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let tool = registry.tool("mcp__server__echo").unwrap();
    let result = tool
        .execute(
            pi_plugin::ToolContext::standalone("/tmp".into(), pi_core::AbortHandle::new().1),
            "call".into(),
            json!({}),
            pi_plugin::ToolUpdateSink::channel().0,
        )
        .await
        .unwrap();
    assert_eq!(
        result.content,
        vec![pi_core::ContentBlock::Text(pi_core::TextContent::new(
            "still connected"
        ))]
    );
}

#[tokio::test]
async fn changed_connection_abandoned_candidate_preserves_published_transport_until_reload() {
    let original = Fixture::new("server", false, 200).await;
    let changed = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![original.input(0)]);
    let old_inventory = ready(&supervisor, "server").await;
    let published = generation(supervisor.clone());
    let old_view = published.downgrade();
    old_view.publish();
    invoke(&old_inventory).await;

    reconcile(&supervisor, vec![changed.input(0)]);
    let new_inventory = ready(&supervisor, "server").await;
    let failed = generation(supervisor.clone());
    drop(failed); // A later, unrelated registry validator rejects the prepared candidate.
    assert!(!old_view.is_retired());
    invoke(&old_inventory).await;
    closed(&new_inventory.connection).await;
    reconcile(&supervisor, vec![original.input(0)]);
    assert!(Arc::ptr_eq(
        &old_inventory,
        &ready(&supervisor, "server").await
    ));

    reconcile(&supervisor, vec![changed.input(0)]);
    let replacement_inventory = ready(&supervisor, "server").await;
    let replacement = generation(supervisor.clone());
    assert!(!replacement.changed());
    old_view.shutdown(true).await.unwrap();
    replacement.downgrade().publish();
    closed(&old_inventory.connection).await;
    assert!(
        !replacement.changed(),
        "draining old pins must not stale the replacement"
    );
    invoke(&replacement_inventory).await;
    assert_eq!(original.starts.load(Ordering::Acquire), 1);
    assert_eq!(changed.starts.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
}
