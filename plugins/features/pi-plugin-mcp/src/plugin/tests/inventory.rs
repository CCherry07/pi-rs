use super::*;
use axum::response::IntoResponse;
use serde_json::json;

#[tokio::test]
async fn hidden_remote_tool_selection_and_cancel_are_read_only() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new().route("/mcp", axum::routing::post(|axum::Json(request): axum::Json<serde_json::Value>| async move {
        if request["id"].is_null() { return axum::http::StatusCode::ACCEPTED.into_response(); }
        let result = match request["method"].as_str().unwrap() {
            "server/discover" => return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"legacy fixture"}})).into_response(),
            "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
            "tools/list" => json!({"tools":[
                {"name":"a-b","description":"First line\nOmitted detail","inputSchema":{"type":"object"}},
                {"name":"a_b","description":"Plain","inputSchema":{"type":"object"}}
            ]}),
            other => panic!("inventory must not call a tool: {other}"),
        };
        axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp.json");
    let original =
        json!({"mcpServers":{"inventory":{"url":url,"toolExposure":{"a-b":"hidden"}}}}).to_string();
    std::fs::write(&path, &original).unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let mut policy = crate::policy::McpServerPolicy::default();
    policy
        .tool_exposure
        .push(("a-b".into(), McpExposure::Hidden));
    let pool = McpToolSet::connect_with_policies(vec![(
        crate::McpServerConfig::http("inventory", url),
        policy,
    )])
    .await
    .unwrap();
    let command = McpCommand {
        rows: library.read(McpScope::Global).unwrap().servers,
        library,
        pool,
    };
    assert_eq!(
        command.pool.tools().len(),
        1,
        "hidden inventory is not an executable tool"
    );
    for choice in [Some(0), None] {
        let access = Arc::new(ManagerAccess::new(
            vec![Some(0), Some(1), choice, None, None],
            None,
        ));
        let context = CommandContext::with_plugin_context(
            directory.path().into(),
            pi_core::AbortHandle::new().1,
            PluginContextEpoch::new(access.clone()).command_context(),
        );
        command.execute(context, String::new()).await.unwrap();
        let dialogs = access.dialogs.lock().unwrap();
        let pi_plugin::UiDialogRequest::Select { title, options } = &dialogs[2] else {
            panic!("tool selector expected")
        };
        assert!(title.contains("read-only"));
        assert_eq!(options, &["a-b [hidden] — First line", "a_b — Plain"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(access.reloads.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    assert!(!directory.path().join("mcp-auth.json").exists());
    command.pool.shutdown().await.unwrap();
    server.abort();
}
