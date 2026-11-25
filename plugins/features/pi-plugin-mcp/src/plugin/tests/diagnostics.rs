#![cfg(unix)]
use super::*;
use serde_json::json;

#[tokio::test]
async fn disconnected_status_keeps_frozen_catalog_and_shows_safe_stderr_tail() {
    let directory = tempfile::tempdir().unwrap();
    let command_path = format!(
        "{}/tests/fixtures/diagnostics.py",
        env!("CARGO_MANIFEST_DIR")
    );
    let config = crate::McpServerConfig::stdio("fixture", "python3")
        .args([command_path.clone(), "normal".into()])
        .env(std::collections::BTreeMap::from([(
            "FIXTURE_SECRET".into(),
            "local-fake-private-value".into(),
        )]));
    std::fs::write(
        directory.path().join("mcp.json"),
        json!({"mcpServers":{"fixture":{
            "command":"python3","args":[command_path,"normal"],"exposure":"direct"
        }}})
        .to_string(),
    )
    .unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let pool = McpToolSet::connect(vec![config]).await.unwrap();
    let command = McpCommand {
        rows: library.read(McpScope::Global).unwrap().servers,
        library,
        pool,
    };
    let names = command
        .pool
        .tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![command.pool.plugin()])
        .unwrap();
    let (updates, _) = pi_plugin::ToolUpdateSink::channel();
    let error = registry
        .tool("mcp__fixture__close")
        .unwrap()
        .execute(
            pi_plugin::ToolContext::standalone(
                directory.path().into(),
                pi_core::AbortHandle::new().1,
            ),
            "close".into(),
            json!({}),
            updates,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Connection closed"));
    assert_eq!(
        command.server_state(&command.rows[0]),
        McpServerState::Disconnected
    );
    assert_eq!(
        command
            .pool
            .tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>(),
        names
    );
    let access = Arc::new(ManagerAccess::new(vec![], None));
    let context = CommandContext::with_plugin_context(
        directory.path().into(),
        pi_core::AbortHandle::new().1,
        PluginContextEpoch::new(access.clone()).command_context(),
    );
    command.execute(context, "status".into()).await.unwrap();
    let notices = access.notices.lock().unwrap().join("\n");
    assert!(notices.contains("disconnected"), "{notices}");
    assert!(notices.contains("last diagnostic line"), "{notices}");
    assert!(!notices.contains("local-fake-private-value"));
    assert_eq!(access.reloads.load(std::sync::atomic::Ordering::SeqCst), 0);
    command.pool.shutdown().await.unwrap();
}
