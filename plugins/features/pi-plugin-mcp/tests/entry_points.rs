use std::fs;

use pi_plugin::RegistriesBuilder;
use pi_plugin_mcp::{McpLibrary, McpScope, McpToolSet};

#[tokio::test]
async fn explicit_client_pool_does_not_load_local_config_or_register_management_commands() {
    let directory = tempfile::tempdir().unwrap();
    // A malformed local document is irrelevant to callers with explicit server configs.
    fs::write(
        directory.path().join("mcp.json"),
        "invalid local configuration",
    )
    .unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    assert!(library.prepare().await.is_err());

    let pool = McpToolSet::connect(Vec::new()).await.unwrap();
    let (_, registry) = RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    assert!(registry.command_specs().is_empty());
    assert!(pool.tools().is_empty());
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_library_adds_management_only_when_explicitly_prepared() {
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let library = McpLibrary::new(&agent, None, false);
    let document = library.read(McpScope::Global).unwrap();
    assert!(document.servers.is_empty());
    assert!(
        !agent.exists(),
        "read-only discovery must not materialize state"
    );

    let plugin = library.prepare().await.unwrap();
    let (_, registry) = RegistriesBuilder::new()
        .register_plugins(vec![plugin])
        .unwrap();
    let commands = registry.command_specs();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "mcp");
    assert!(
        !agent.exists(),
        "preparing an empty library must not create files"
    );
}
