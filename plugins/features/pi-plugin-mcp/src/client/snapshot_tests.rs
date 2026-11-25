use super::*;
use rmcp::{ServerHandler, ServiceExt};

#[derive(Clone)]
struct ProjectionServer;

impl ServerHandler for ProjectionServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut info = rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.instructions = Some("  Shared inventory instructions.  ".into());
        info
    }

    async fn call_tool(
        &self,
        _: CallToolRequestParams,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        Ok(
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                "still connected",
            )])
            .into(),
        )
    }
}

async fn connection(name: &str) -> (Arc<McpConnection>, tokio::task::JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        ProjectionServer
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let handler = McpClientHandler::new(None);
    let service = handler.clone().serve(client_io).await.unwrap();
    (connected_transport(name.into(), handler, service), task)
}

fn remote(name: &str) -> McpToolSpec {
    McpToolSpec::new(
        name.to_owned(),
        "Original tool description",
        Arc::new(serde_json::Map::new()),
    )
}

fn auth_required(name: &str) -> McpAuthorizationRequired {
    McpAuthorizationRequired {
        server: name.into(),
        provider: None,
        static_authorization: false,
        challenge: None,
    }
}

#[tokio::test]
async fn from_connections_freezes_inventory_policy_names_and_resource_metadata() {
    let (connection, task) = connection("shared").await;
    let mut inventory = vec![remote("echo"), remote("a-b"), remote("a_b")];
    let old_policy = McpServerPolicy {
        description: Some("Old generation".into()),
        timeout: Duration::from_secs(1),
        tool_exposure: vec![("a-b".into(), McpExposure::Hidden)],
        ..McpServerPolicy::direct()
    };
    let old = McpToolSet::from_connections(
        vec![(connection.clone(), inventory.clone(), old_policy.clone())],
        vec![auth_required("pending")],
    )
    .unwrap();
    let old_tools = old.tools();
    assert_eq!(old_tools.len(), 2);
    assert_eq!(old.offered_tools("shared").len(), 3);
    assert_eq!(old.authorization_required()[0].server, "pending");
    assert_eq!(old.inner.resource_tools.len(), 3);
    assert_eq!(
        old.resource_discovery()
            .namespace
            .unwrap()
            .instructions
            .as_deref(),
        Some("Instructions for MCP resources from `shared`:\nShared inventory instructions.")
    );
    let alias = old_tools
        .iter()
        .find(|tool| tool.remote_name == "a_b")
        .unwrap();
    // The hidden collision peer still contributes to deterministic name assignment.
    assert_eq!(alias.name, qualified_tool_name("shared", "a_b", |_| true));

    inventory[0].description = Some("Updated tool description".into());
    inventory.push(remote("new"));
    let next_policy = McpServerPolicy {
        exposure: McpExposure::Hidden,
        description: Some("Next generation".into()),
        timeout: Duration::from_secs(5),
        tool_exposure: vec![
            ("echo".into(), McpExposure::Deferred),
            ("a_b".into(), McpExposure::Direct),
            ("new".into(), McpExposure::Direct),
        ],
        ..Default::default()
    };
    let next = McpToolSet::from_connections(
        vec![(connection.clone(), inventory, next_policy.clone())],
        Vec::new(),
    )
    .unwrap();
    assert_eq!(old.tools(), old_tools);
    assert_eq!(old.offered_tools("shared").len(), 3);
    assert_eq!(next.tools().len(), 3);
    assert_eq!(next.offered_tools("shared").len(), 4);
    assert!(next.inner.resource_tools.is_empty());
    assert_eq!(old.inner.servers[0].policy, old_policy);
    assert_eq!(next.inner.servers[0].policy, next_policy);
    assert!(!Arc::ptr_eq(&old.inner.servers[0], &next.inner.servers[0]));
    assert!(Arc::ptr_eq(
        &old.inner.servers[0].connection,
        &next.inner.servers[0].connection
    ));
    let (_, old_registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![old.plugin()])
        .unwrap();
    let (_, next_registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![next.plugin()])
        .unwrap();
    let old_info = old_registry
        .tool_infos()
        .into_iter()
        .find(|tool| tool.spec.name == "mcp__shared__echo")
        .unwrap();
    let next_info = next_registry
        .tool_infos()
        .into_iter()
        .find(|tool| tool.spec.name == "mcp__shared__echo")
        .unwrap();
    assert_eq!(old_info.discovery.exposure, ToolExposure::Direct);
    assert_eq!(next_info.discovery.exposure, ToolExposure::Deferred);
    assert_eq!(
        next_info
            .discovery
            .namespace
            .unwrap()
            .instructions
            .as_deref(),
        Some("Shared inventory instructions.")
    );
    drop(next);
    assert_eq!(old.tools(), old_tools);
    assert!(!connection.is_closed());
    connection.close().await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn quarantined_final_view_keeps_names_transport_and_original_pool_frozen() {
    let (connection, task) = connection("shared").await;
    for exposure in [
        McpExposure::Direct,
        McpExposure::Deferred,
        McpExposure::Codemode,
    ] {
        let original = McpToolSet::from_connections(
            vec![(
                connection.clone(),
                vec![remote("a-b"), remote("a_b"), remote("echo")],
                McpServerPolicy {
                    exposure,
                    ..Default::default()
                },
            )],
            vec![auth_required("pending")],
        )
        .unwrap();
        let frozen = original.tools();
        let blocked = frozen
            .iter()
            .find(|tool| tool.remote_name == "a-b")
            .unwrap()
            .name
            .clone();
        let filtered = original.without_tools(&[blocked.clone(), "absent".into()]);
        let expected = frozen
            .iter()
            .filter(|tool| tool.name != blocked)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(filtered.tools(), expected);
        assert_eq!(original.tools(), frozen);
        assert!(Arc::ptr_eq(
            &original.inner.servers[0],
            &filtered.inner.servers[0]
        ));
        assert_eq!(filtered.authorization_required()[0].server, "pending");
        assert_eq!(
            filtered.inner.resource_tools.len(),
            original.inner.resource_tools.len()
        );
        assert_eq!(
            original
                .offered_tools("shared")
                .iter()
                .find(|tool| tool.name == "a-b")
                .unwrap()
                .exposure,
            exposure
        );
        assert_eq!(
            filtered
                .offered_tools("shared")
                .iter()
                .find(|tool| tool.name == "a-b")
                .unwrap()
                .exposure,
            McpExposure::Hidden
        );
        assert!(!filtered.tool_names().contains(&blocked));
        let (_, registry) = pi_plugin::RegistriesBuilder::new()
            .register_plugins(vec![filtered.plugin()])
            .unwrap();
        assert!(registry.tool(&blocked).is_none());
        assert!(
            registry
                .tool_infos()
                .iter()
                .all(|tool| tool.spec.name != blocked)
        );
        for tool in &expected {
            assert!(registry.tool(&tool.name).is_some());
        }
        let blocked_original = original
            .inner
            .tools
            .iter()
            .find(|tool| tool.descriptor.name == blocked)
            .unwrap();
        let (_, signal) = pi_core::AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = blocked_original
            .execute(
                ToolContext::standalone(PathBuf::from("/tmp"), signal),
                "call".into(),
                serde_json::json!({}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(
            result.content,
            vec![ContentBlock::Text(TextContent::new("still connected"))]
        );
        assert!(!connection.is_closed());
    }
    connection.close().await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn from_connections_rejects_collisions_without_closing_retained_transports() {
    let (first, first_task) = connection("server-a").await;
    let (alias, alias_task) = connection("server_a").await;
    let policy = McpServerPolicy::direct();
    let offered = vec![remote("echo")];
    let old = McpToolSet::from_connections(
        vec![(first.clone(), offered.clone(), policy.clone())],
        Vec::new(),
    )
    .unwrap();
    let frozen = old.tools();
    let candidate = || (first.clone(), offered.clone(), policy.clone());
    assert!(matches!(
        McpToolSet::from_connections(vec![candidate(), candidate()], Vec::new()),
        Err(McpError::DuplicateServerName(name)) if name == "server-a"
    ));
    assert!(matches!(
        McpToolSet::from_connections(
            vec![
                candidate(),
                (alias.clone(), offered.clone(), policy.clone())
            ],
            Vec::new(),
        ),
        Err(McpError::DuplicateNamespace(_))
    ));
    assert!(matches!(
        McpToolSet::from_connections(vec![candidate()], vec![auth_required("server-a")]),
        Err(McpError::DuplicateServerName(_))
    ));
    assert!(matches!(
        McpToolSet::from_connections(
            vec![(first.clone(), vec![remote("echo"), remote("echo")], policy)],
            Vec::new(),
        ),
        Err(McpError::DuplicateTool(name)) if name == "server-a/echo"
    ));
    assert_eq!(old.tools(), frozen);
    assert!(!first.is_closed());
    assert!(!alias.is_closed());
    let (_, signal) = pi_core::AbortHandle::new();
    let (updates, _) = ToolUpdateSink::channel();
    let result = old.inner.tools[0]
        .execute(
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
            "call".into(),
            serde_json::json!({}),
            updates,
        )
        .await
        .unwrap();
    assert_eq!(
        result.content,
        vec![ContentBlock::Text(TextContent::new("still connected"))]
    );
    first.close().await.unwrap();
    alias.close().await.unwrap();
    first_task.await.unwrap();
    alias_task.await.unwrap();
}
