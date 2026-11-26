use super::*;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
mod authority;
mod fixture;
mod mixed;
mod sse;
use fixture::{Fixture, Reply, entered};

fn context() -> ToolContext {
    let (_, signal) = pi_core::AbortHandle::new();
    ToolContext::standalone(PathBuf::from("/tmp"), signal)
}

async fn invoke(
    pool: &McpToolSet,
    name: &str,
    context: ToolContext,
) -> Result<ToolResult, ToolError> {
    let tool = pool
        .inner
        .tools
        .iter()
        .find(|tool| tool.remote_name == name)
        .unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    tool.execute(context, "recovery".into(), json!({}), updates)
        .await
}

async fn resources(pool: &McpToolSet, name: &str) -> Result<ToolResult, ToolError> {
    let tool = pool
        .inner
        .resource_tools
        .iter()
        .find(|tool| tool.spec().name == name)
        .unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    let params = if name == "read_mcp_resource" {
        json!({"server":"fixture", "uri":"fixture://read"})
    } else {
        json!({})
    };
    tool.execute(context(), "resource-recovery".into(), params, updates)
        .await
}

#[tokio::test]
async fn definitive_expiry_replaces_once_and_keeps_frozen_view() {
    let fixture = Fixture::new(false).await;
    fixture.replies(&[Reply::Status(404)]);
    let (owner, source, pool) = fixture.prepare().await;
    let frozen = pool.tools();
    let instructions = pool.inner.servers[0]
        .connection
        .instructions()
        .map(str::to_owned);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    assert_eq!(fixture.executed(), 1);
    assert_eq!(pool.tools(), frozen);
    assert_eq!(
        pool.inner.servers[0].connection.instructions(),
        instructions.as_deref()
    );
    assert!(source.changed());
    owner.close().await.unwrap();
}

#[tokio::test]
async fn repeated_expiry_exhausts_without_eager_third_setup_then_next_call_opens_lazily() {
    let fixture = Fixture::new(false).await;
    fixture.replies(&[Reply::Status(404), Reply::Status(404)]);
    let (owner, _source, pool) = fixture.prepare().await;
    let (updates, _) = ToolUpdateSink::channel();
    let error = pool.inner.servers[0]
        .request(
            &context(),
            ClientRequest::CallToolRequest(CallToolRequest::new(CallToolRequestParams::new(
                "echo",
            ))),
            "echo",
            &updates,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error,
            McpRequestError::Service(rmcp::service::ServiceError::TransportSend(error))
                if matches!(error.error.downcast_ref::<crate::http_client::HttpError>(),
                    Some(crate::http_client::HttpError::SessionExpired))
        ),
        "original typed session-expiry outcome: {error:?}"
    );
    assert_eq!(
        error.into_tool_error("fixture").to_string(),
        "tool failed: MCP request failed for server fixture"
    );
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    assert_eq!(fixture.executed(), 0);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 3);
    assert_eq!(fixture.calls(), 3);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn tools_never_retry_ambiguous_http_rpc_or_network_failures_even_with_readonly_hint() {
    for read_only in [false, true] {
        for failure in [
            Reply::Status(408),
            Reply::Status(429),
            Reply::Status(503),
            Reply::Status(501),
            Reply::Status(401),
            Reply::Rpc,
            Reply::Network,
        ] {
            let fixture = Fixture::new(read_only).await;
            fixture.replies(&[failure]);
            let (owner, _source, pool) = fixture.prepare().await;
            assert!(invoke(&pool, "echo", context()).await.is_err());
            assert_eq!(fixture.calls(), 1);
            assert_eq!(fixture.starts(), 1);
            assert_eq!(fixture.executed(), 0);
            owner.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn resources_retry_only_classified_statuses_and_at_most_once() {
    for (failure, retry) in [
        (Reply::Status(408), true),
        (Reply::Status(429), true),
        (Reply::Status(503), true),
        (Reply::Status(404), true),
        (Reply::Status(501), false),
        (Reply::Status(401), false),
        (Reply::Rpc, false),
        (Reply::Network, false),
    ] {
        let fixture = Fixture::new(false).await;
        fixture.resources(&[failure]);
        let (owner, _source, pool) = fixture.prepare().await;
        assert_eq!(resources(&pool, "read_mcp_resource").await.is_ok(), retry);
        assert_eq!(
            fixture.state.pages.lock().unwrap().len(),
            if retry { 2 } else { 1 }
        );
        owner.close().await.unwrap();
    }
    let fixture = Fixture::new(false).await;
    fixture.resources(&[Reply::Status(503), Reply::Status(503)]);
    let (owner, _source, pool) = fixture.prepare().await;
    assert!(resources(&pool, "read_mcp_resource").await.is_err());
    assert_eq!(fixture.state.pages.lock().unwrap().len(), 2);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn aggregate_restart_discards_partial_pages_and_cursor_across_transient_or_expiry() {
    for failure in [Reply::Status(503), Reply::Status(404)] {
        for tool in ["list_mcp_resources", "list_mcp_resource_templates"] {
            let fixture = Fixture::new(false).await;
            fixture.resources(&[failure]);
            let (owner, _source, pool) = fixture.prepare().await;
            let result = resources(&pool, tool).await.unwrap();
            let text = serde_json::to_string(&result.content).unwrap();
            assert!(
                text.contains("round-2-first") && text.contains("round-2-last"),
                "{text}"
            );
            assert!(
                !text.contains("round-1"),
                "partial first attempt leaked: {text}"
            );
            assert_eq!(
                fixture
                    .state
                    .pages
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(_, cursor)| cursor.as_deref())
                    .collect::<Vec<_>>(),
                vec![None, Some("next"), None, Some("next")]
            );
            owner.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn aggregate_repeat_cursor_is_not_retryable() {
    let fixture = Fixture::new(false).await;
    fixture.state.repeat_cursor.store(true, Ordering::Release);
    let (owner, _source, pool) = fixture.prepare().await;
    let result = resources(&pool, "list_mcp_resources").await.unwrap();
    assert!(
        serde_json::to_string(&result.content)
            .unwrap()
            .contains("duplicate cursor")
    );
    assert_eq!(fixture.state.pages.lock().unwrap().len(), 2);
    assert_eq!(fixture.starts(), 1);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn replacement_full_descriptor_change_refuses_execution_before_wire() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    fixture.replies(&[Reply::Status(404)]);
    fixture.state.tools.lock().unwrap()[0].description = Some("new declaration".into());
    let error = invoke(&pool, "echo", context()).await.unwrap_err();
    assert!(error.to_string().contains("inventory changed"), "{error}");
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.executed(), 0);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn expired_replacement_preserves_unrelated_held_json_and_open_sse() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    let json_pool = pool.clone();
    let json = tokio::spawn(async move { invoke(&json_pool, "held-json", context()).await });
    let sse_pool = pool.clone();
    let sse = tokio::spawn(async move { invoke(&sse_pool, "held-sse", context()).await });
    entered(&fixture.state.held_entered, 2).await;
    fixture.replies(&[Reply::Status(404)]);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    assert!(
        !json.is_finished() && !sse.is_finished(),
        "replacement must not abort old admitted work"
    );
    fixture.state.held_release.add_permits(2);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), json)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), sse)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn cancelling_one_replacement_waiter_does_not_cancel_shared_opening() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    fixture.state.hold_init.store(true, Ordering::Release);
    fixture.replies(&[Reply::Status(404)]);
    let (abort, signal) = pi_core::AbortHandle::new();
    let first_pool = pool.clone();
    let first = tokio::spawn(async move {
        invoke(
            &first_pool,
            "echo",
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
        )
        .await
    });
    entered(&fixture.state.init_entered, 1).await;
    let second_pool = pool.clone();
    let second = tokio::spawn(async move { invoke(&second_pool, "echo", context()).await });
    abort.abort();
    assert!(matches!(first.await.unwrap(), Err(ToolError::Aborted)));
    fixture.state.init_release.add_permits(1);
    assert!(second.await.unwrap().is_ok());
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn final_generation_drop_retires_waiter_even_when_executables_and_supervisor_survive() {
    let fixture = Fixture::new(false).await;
    let (owner, source, pool) = fixture.prepare().await;
    fixture.state.hold_init.store(true, Ordering::Release);
    fixture.replies(&[Reply::Status(404)]);
    let invocation_pool = pool.clone();
    let invocation = tokio::spawn(async move { invoke(&invocation_pool, "echo", context()).await });
    entered(&fixture.state.init_entered, 1).await;
    drop(source);
    let error = tokio::time::timeout(Duration::from_secs(5), invocation)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("retired"), "{error}");
    fixture.state.init_release.add_permits(1);
    assert!(invoke(&pool, "echo", context()).await.is_err());
    assert_eq!(fixture.calls(), 1);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn denied_scoped_tool_preserves_spec_but_never_dispatches_or_recovers() {
    let fixture = Fixture::new(false).await;
    let (owner, _source, pool) = fixture.prepare().await;
    fixture.replies(&[Reply::Status(404)]);
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let original = &pool.inner.tools[0];
    let scoped = registry.scope_tool_execution(&[], "denied fixture");
    let denied = scoped.tool(&original.spec().name).unwrap();
    assert_eq!(denied.spec(), original.spec());
    let (updates, _) = ToolUpdateSink::channel();
    assert!(
        denied
            .execute(context(), "denied".into(), json!({}), updates)
            .await
            .is_err()
    );
    assert_eq!(fixture.calls(), 0);
    assert_eq!(fixture.starts(), 1);
    owner.close().await.unwrap();
}
