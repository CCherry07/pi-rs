use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock as McpContentBlock, NumberOrString,
    ProgressNotificationParam, ProgressToken, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct RequestProbe {
    cancelled: Arc<Notify>,
}

impl ServerHandler for RequestProbe {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    #[allow(deprecated)] // This fixture verifies Pi's stable roots contract.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        match request.name.as_ref() {
            "roots" => {
                let roots = context.peer.list_roots().await.unwrap();
                return Ok(CallToolResult::success(vec![McpContentBlock::text(
                    serde_json::to_string(&roots).unwrap(),
                )])
                .into());
            }
            "progress" => {
                let token = context.meta.get_progress_token().unwrap();
                for step in 1..=5 {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    context
                        .peer
                        .notify_progress(
                            ProgressNotificationParam::new(token.clone(), f64::from(step))
                                .with_total(5.0)
                                .with_message(format!("step {step}")),
                        )
                        .await
                        .unwrap();
                }
                return Ok(CallToolResult::success(vec![McpContentBlock::text("done")]).into());
            }
            "unrelated-progress" => loop {
                tokio::select! {
                    () = context.ct.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_millis(40)) => {
                        context.peer.notify_progress(ProgressNotificationParam::new(
                            ProgressToken(NumberOrString::String("unrelated".into())),
                            1.0,
                        )).await.unwrap();
                    }
                }
            },
            _ => context.ct.cancelled().await,
        }
        self.cancelled.notify_one();
        Ok(CallToolResult::success(Vec::new()).into())
    }
}

async fn memory_server(
    probe: RequestProbe,
    policy: McpServerPolicy,
) -> (Arc<ConnectedServer>, tokio::task::JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        probe
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let handler = McpClientHandler::new(policy.root.as_deref());
    let service = handler.clone().serve(client_io).await.unwrap();
    (
        connected_server("fixture".into(), policy, handler, service),
        task,
    )
}

fn tool(server: Arc<ConnectedServer>, name: &str) -> McpTool {
    McpTool::new(
        server,
        McpToolSpec::new(
            name.to_owned(),
            "Protocol contract fixture",
            Arc::new(serde_json::Map::new()),
        ),
        format!("mcp__fixture__{name}"),
    )
}

async fn close_memory_server(server: Arc<ConnectedServer>, task: tokio::task::JoinHandle<()>) {
    server.connection.close().await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn request_timeout_cancels_the_remote_operation() {
    let probe = RequestProbe::default();
    let (server, task) = memory_server(
        probe.clone(),
        McpServerPolicy {
            timeout: Duration::from_millis(180),
            ..Default::default()
        },
    )
    .await;
    let (_, signal) = pi_core::AbortHandle::new();
    let (updates, _) = ToolUpdateSink::channel();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        tool(server.clone(), "wait").execute(
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
            "timeout".into(),
            json!({}),
            updates,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ToolError::Execution(error)) if error.contains("timed out")));
    tokio::time::timeout(Duration::from_secs(3), probe.cancelled.notified())
        .await
        .unwrap();
    close_memory_server(server, task).await;
}

#[tokio::test]
async fn matching_progress_extends_deadline_and_reaches_tool_updates() {
    let (server, task) = memory_server(
        RequestProbe::default(),
        McpServerPolicy {
            timeout: Duration::from_millis(250),
            ..Default::default()
        },
    )
    .await;
    let (_, signal) = pi_core::AbortHandle::new();
    let (updates, mut receiver) = ToolUpdateSink::channel();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        tool(server.clone(), "progress").execute(
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
            "progress".into(),
            json!({}),
            updates,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        result.content,
        vec![ContentBlock::Text(TextContent::new("done"))]
    );
    let first = receiver.recv().await.unwrap();
    assert_eq!(
        first.content,
        vec![ContentBlock::Text(TextContent::new("step 1"))]
    );
    assert_eq!(
        first.details,
        Some(json!({"server":"fixture","tool":"progress"}))
    );
    close_memory_server(server, task).await;
}

#[tokio::test]
async fn another_requests_progress_neither_extends_timeout_nor_updates_the_tool() {
    let probe = RequestProbe::default();
    let (server, task) = memory_server(
        probe.clone(),
        McpServerPolicy {
            timeout: Duration::from_millis(180),
            ..Default::default()
        },
    )
    .await;
    let (_, signal) = pi_core::AbortHandle::new();
    let (updates, mut receiver) = ToolUpdateSink::channel();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        tool(server.clone(), "unrelated-progress").execute(
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
            "unrelated".into(),
            json!({}),
            updates,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ToolError::Execution(error)) if error.contains("timed out")));
    assert!(receiver.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(3), probe.cancelled.notified())
        .await
        .unwrap();
    close_memory_server(server, task).await;
}

#[tokio::test]
async fn server_receives_the_session_root_during_a_tool_request() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("session space");
    let (server, task) = memory_server(
        RequestProbe::default(),
        McpServerPolicy {
            root: Some(cwd.clone()),
            ..Default::default()
        },
    )
    .await;
    let (_, signal) = pi_core::AbortHandle::new();
    let (updates, _) = ToolUpdateSink::channel();
    let result = tool(server.clone(), "roots")
        .execute(
            ToolContext::standalone(PathBuf::from("/some-other-cwd"), signal),
            "roots".into(),
            json!({}),
            updates,
        )
        .await
        .unwrap();
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("expected root JSON");
    };
    let value: Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(
        value["roots"],
        json!([{
            "uri":url::Url::from_file_path(&cwd).unwrap().as_str(),
            "name":"session space"
        }])
    );
    close_memory_server(server, task).await;
}

async fn http_catalog(
    names: Option<Vec<&str>>,
) -> (
    McpServerConfig,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use axum::response::IntoResponse;

    let tools = names.map(|names| names.into_iter().map(str::to_owned).collect::<Vec<_>>());
    let list_calls = Arc::new(AtomicUsize::new(0));
    let counter = list_calls.clone();
    let router = axum::Router::new().route("/mcp", axum::routing::post(move |axum::Json(request): axum::Json<Value>| {
        let tools = tools.clone();
        let counter = counter.clone();
        async move {
            let id = &request["id"];
            if id.is_null() { return http::StatusCode::ACCEPTED.into_response(); }
            let result = match request["method"].as_str().unwrap() {
                "server/discover" => return axum::Json(json!({"jsonrpc":"2.0", "id":"legacy-only", "error":{"code":-32601,"message":"Method not found"}})).into_response(),
                "initialize" => json!({"protocolVersion":"2025-03-26", "capabilities":if tools.is_some() { json!({"tools":{}}) } else { json!({"resources":{}}) }, "serverInfo":{"name":"fixture","version":"1"}}),
                "tools/list" => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    json!({"tools":tools.unwrap_or_default().into_iter().map(|name| json!({"name":name,"inputSchema":{"type":"object"}})).collect::<Vec<_>>()})
                }
                method => panic!("unexpected method {method}"),
            };
            axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = McpServerConfig::http(
        "fixture",
        format!("http://{}/mcp", listener.local_addr().unwrap()),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (config, list_calls, task)
}

#[tokio::test]
async fn resource_only_servers_connect_without_a_tools_list_request() {
    let (config, list_calls, task) = http_catalog(None).await;
    let pool = McpToolSet::connect(vec![config]).await.unwrap();
    assert!(pool.tools().is_empty());
    assert_eq!(list_calls.load(Ordering::SeqCst), 0);
    pool.shutdown().await.unwrap();
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn collision_names_remain_stable_when_list_order_or_exposure_changes() {
    let mut snapshots = Vec::new();
    for (names, hide) in [
        (vec!["a-b", "a_b"], false),
        (vec!["a_b", "a-b"], false),
        (vec!["a_b", "a-b"], true),
    ] {
        let (config, _, task) = http_catalog(Some(names)).await;
        let mut policy = McpServerPolicy::default();
        if hide {
            policy
                .tool_exposure
                .push(("a-b".into(), McpExposure::Hidden));
        }
        let pool = McpToolSet::connect_with_policies(vec![(config, policy)])
            .await
            .unwrap();
        let inventory = pool.offered_tools("fixture");
        assert_eq!(inventory.len(), 2);
        let raw = inventory.iter().find(|tool| tool.name == "a-b").unwrap();
        assert_eq!(
            raw.exposure,
            if hide {
                McpExposure::Hidden
            } else {
                McpExposure::Codemode
            }
        );
        assert!(inventory.iter().any(|tool| tool.name == "a_b"));
        assert!(pool.offered_tools("unknown").is_empty());
        let names: BTreeMap<_, _> = pool
            .tools()
            .into_iter()
            .map(|tool| (tool.remote_name, tool.name))
            .collect();
        let (_, registries) = pi_plugin::RegistriesBuilder::new()
            .register_plugins(vec![pool.plugin()])
            .unwrap();
        if hide {
            assert!(!names.contains_key("a-b"));
            let hidden_name = qualified_tool_name("fixture", "a-b", |_| true);
            assert!(registries.tool(&hidden_name).is_none());
        }
        snapshots.push(names);
        pool.shutdown().await.unwrap();
        task.abort();
        let _ = task.await;
    }
    assert_eq!(snapshots[0], snapshots[1]);
    assert_eq!(snapshots[1]["a_b"], snapshots[2]["a_b"]);
    assert_ne!(snapshots[0]["a_b"], "mcp__fixture__a_b");
}
