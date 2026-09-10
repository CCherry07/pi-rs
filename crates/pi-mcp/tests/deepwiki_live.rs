//! Opt-in compatibility smoke test for a public Streamable HTTP server.
//! Kept ignored so the deterministic workspace suite never depends on a network service.

use pi_mcp::{McpServerConfig, McpToolSet};

#[tokio::test]
#[ignore = "requires public network access"]
async fn deepwiki_streamable_http_handshake_and_discovery() {
    let tools = McpToolSet::connect(vec![McpServerConfig::http(
        "deepwiki",
        "https://mcp.deepwiki.com/mcp",
    )])
    .await
    .expect("DeepWiki MCP handshake and discovery should succeed");

    let names = tools
        .tools()
        .into_iter()
        .map(|tool| tool.remote_name)
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "read_wiki_structure"));
    assert!(names.iter().any(|name| name == "read_wiki_contents"));
    assert!(names.iter().any(|name| name == "ask_question"));
    tools.shutdown().await.unwrap();
}
