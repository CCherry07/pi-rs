use super::*;

struct Catalog(Vec<ToolSpec>);
impl pi_plugin::SessionContextAccess for Catalog {
    fn all_tools(&self) -> pi_plugin::PluginContextResult<Vec<ToolSpec>> {
        Ok(self.0.clone())
    }
}
impl pi_plugin::ModelsContextAccess for Catalog {}
impl pi_plugin::UiContextAccess for Catalog {}

fn host_context(pool: &McpToolSet) -> (pi_plugin::PluginContextEpoch, ToolContext) {
    let mut specs = pool
        .inner
        .tools
        .iter()
        .map(|tool| tool.spec())
        .collect::<Vec<_>>();
    specs.extend(pool.inner.resource_tools.iter().map(|tool| tool.spec()));
    let epoch = pi_plugin::PluginContextEpoch::new(Arc::new(Catalog(specs)));
    let (_, signal) = pi_core::AbortHandle::new();
    let context = ToolContext::with_plugin_context(PathBuf::from("/tmp"), signal, epoch.context())
        .with_run_id("frozen-run".into());
    (epoch, context)
}

#[tokio::test]
async fn nested_execution_remains_valid_with_empty_nested_catalog() {
    let fixture = Fixture::new(false).await;
    fixture.replies(&[Reply::Status(404)]);
    let (owner, _source, pool) = fixture.prepare().await;
    let (_epoch, context) = host_context(&pool);
    assert!(context.nested_tools().is_empty());
    let original = pool
        .inner
        .tools
        .iter()
        .find(|tool| tool.remote_name == "echo")
        .unwrap();
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let scoped =
        registry.scope_tool_execution(&[original.spec().name.clone()], "nested frozen admission");
    let tool = scoped.tool(&original.spec().name).unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    assert!(
        tool.execute(context, "nested-call".into(), json!({}), updates)
            .await
            .is_ok()
    );
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    assert_eq!(fixture.executed(), 1);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn retired_host_context_cannot_rebind_a_newly_granted_same_spec() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    let (epoch, retired) = host_context(&pool);
    epoch.retire();
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let original = pool
        .inner
        .tools
        .iter()
        .find(|tool| tool.remote_name == "echo")
        .unwrap();
    let scoped = registry.scope_tool_execution(&[original.spec().name.clone()], "new grant");
    let tool = scoped.tool(&original.spec().name).unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    let error = tool
        .execute(retired, "retired".into(), json!({}), updates)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("retired"));
    assert_eq!(fixture.calls(), 0);
    assert_eq!(fixture.starts(), 1);
    let (_new_epoch, active) = host_context(&pool);
    let (updates, _) = ToolUpdateSink::channel();
    assert!(
        tool.execute(active, "fresh".into(), json!({}), updates)
            .await
            .is_ok()
    );
    assert_eq!(fixture.calls(), 1);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn retired_resource_context_is_rejected_without_wire_or_recovery() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    let (epoch, context) = host_context(&pool);
    epoch.retire();
    fixture.resources(&[Reply::Status(503)]);
    let tool = pool
        .inner
        .resource_tools
        .iter()
        .find(|tool| tool.spec().name == "read_mcp_resource")
        .unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    let error = tool
        .execute(
            context,
            "retired-resource".into(),
            json!({"server":"fixture","uri":"fixture://read"}),
            updates,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("retired"));
    assert!(fixture.state.pages.lock().unwrap().is_empty());
    assert_eq!(fixture.starts(), 1);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn explicit_closed_connection_keeps_existing_disconnect_diagnostic() {
    let fixture = Fixture::new(false).await;
    let pool = McpToolSet::connect(vec![fixture.config.clone()])
        .await
        .unwrap();
    pool.inner.servers[0].connection.close().await.unwrap();
    let error = invoke(&pool, "echo", context()).await.unwrap_err();
    assert!(
        error.to_string().contains("Connection closed"),
        "explicit path diagnostic: {error}"
    );
    assert_eq!(fixture.calls(), 0);
    assert_eq!(
        fixture.starts(),
        1,
        "no lazy reconnect for a plain explicit close"
    );
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn ephemeral_context_uses_scoped_grants_without_parent_capabilities() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    let epoch = pi_plugin::PluginContextEpoch::unavailable();
    let context = ToolContext::with_plugin_context(
        PathBuf::from("/tmp"),
        pi_core::AbortHandle::new().1,
        epoch.context(),
    )
    .with_run_id("ephemeral-run".into());
    assert!(matches!(
        context.session.tools(),
        Err(pi_plugin::PluginContextError::Unbound)
    ));
    let name = pool
        .inner
        .tools
        .iter()
        .find(|tool| tool.remote_name == "echo")
        .unwrap()
        .spec()
        .name;
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let denied = registry.scope_tool_execution(&[], "ephemeral deny");
    // The descriptor remains identical, but never grants preparation or execution.
    let denied = denied.tool(&name).unwrap();
    assert!(denied.prepare_arguments(&context, json!({})).await.is_err());
    let (updates, _) = ToolUpdateSink::channel();
    assert!(
        denied
            .execute(context.clone(), "denied".into(), json!({}), updates)
            .await
            .unwrap_err()
            .to_string()
            .contains("denied tool")
    );
    assert_eq!(fixture.calls(), 0);
    assert_eq!(fixture.starts(), 1);

    fixture.replies(&[Reply::Status(404)]);
    let allowed = registry.scope_tool_execution(std::slice::from_ref(&name), "ephemeral allow");
    let allowed = allowed.tool(&name).unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    let result = allowed
        .execute(context.clone(), "allowed".into(), json!({}), updates)
        .await;
    assert!(
        result.is_ok(),
        "live unbound capability is not retired: {result:?}"
    );
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    assert_eq!(fixture.executed(), 1);

    // The exception for an unbound capability must not swallow actual epoch retirement.
    epoch.retire();
    let (updates, _) = ToolUpdateSink::channel();
    let error = allowed
        .execute(context, "retired".into(), json!({}), updates)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("retired"));
    assert_eq!(fixture.calls(), 2);
    assert_eq!(fixture.starts(), 2);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn ephemeral_context_cannot_bypass_retired_generation_or_missing_host_inventory() {
    let fixture = Fixture::new(false).await;
    let (owner, source, pool) = fixture.prepare().await;
    let (_next_source, next_pool) = fixture.publish(&owner).await;
    drop(source);
    let context = context().with_run_id("ephemeral-run".into());
    let error = invoke(&pool, "echo", context.clone()).await.unwrap_err();
    assert!(error.to_string().contains("retired"));
    assert_eq!(fixture.calls(), 0);
    let empty = pi_plugin::PluginContextEpoch::new(Arc::new(Catalog(vec![])));
    let missing = ToolContext::with_plugin_context(
        PathBuf::from("/tmp"),
        pi_core::AbortHandle::new().1,
        empty.context(),
    )
    .with_run_id("host-run".into());
    let error = invoke(&next_pool, "echo", missing).await.unwrap_err();
    assert!(error.to_string().contains("inventory changed"));
    assert_eq!(fixture.calls(), 0);
    assert!(invoke(&next_pool, "echo", context).await.is_ok());
    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.starts(), 1);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn authoritative_reconnect_cancels_detached_and_current_admitted_transports() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    let old_pool = pool.clone();
    let old = tokio::spawn(async move { invoke(&old_pool, "held-sse", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    fixture.replies(&[Reply::Status(404)]);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    let current_pool = pool.clone();
    let current = tokio::spawn(async move { invoke(&current_pool, "held-json", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    owner.reconnect("fixture").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), old)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), current)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    fixture.state.held_release.add_permits(2);
    let previous_calls = fixture.calls();
    let error = invoke(&pool, "echo", context()).await.unwrap_err();
    assert!(
        error.to_string().contains("retired"),
        "old exact pin must not bind the new slot: {error}"
    );
    assert_eq!(fixture.calls(), previous_calls);
    let (_new_source, next_pool) = fixture.publish(&owner).await;
    assert!(invoke(&next_pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 3);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn generation_retirement_wakes_admitted_requests_without_closing_newer_lease() {
    for shutdown in [false, true] {
        let fixture = Fixture::new(false).await;
        let (owner, source, pool) = fixture.prepare().await;
        let old_weak = source.downgrade();
        let json_pool = pool.clone();
        let json = tokio::spawn(async move { invoke(&json_pool, "held-json", context()).await });
        let sse_pool = pool.clone();
        let sse = tokio::spawn(async move { invoke(&sse_pool, "held-sse", context()).await });
        entered(&fixture.state.held_entered, 2).await;
        let (_next_source, next_pool) = fixture.publish(&owner).await;
        let _retained_source = if shutdown {
            old_weak.shutdown(true).await.unwrap();
            Some(source)
        } else {
            drop(source);
            None
        };
        assert!(
            old_weak.is_retired(),
            "waiting requests must not retain their generation lease"
        );
        for pending in [json, sse] {
            let error = tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().contains("retired"), "{error}");
        }
        assert!(!next_pool.inner.servers[0].connection.is_closed());
        assert!(invoke(&next_pool, "echo", context()).await.is_ok());
        assert_eq!(
            fixture.starts(),
            1,
            "new lease keeps the same raw transport"
        );
        assert_eq!(fixture.calls(), 3);
        fixture.state.held_release.add_permits(2);
        owner.close().await.unwrap();
    }
}
