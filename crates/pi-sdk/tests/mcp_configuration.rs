use std::fs;
use std::sync::Arc;

const EMPTY: &str = "{\n  \"version\": 1,\n  \"mcpServers\": {}\n}\n";

#[tokio::test]
async fn invalid_reload_keeps_previous_generation_and_acp_mode_skips_files() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let project = dir.path().join("project");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        agent.join("memory.json"),
        r#"{"version":1,"enabled":false}"#,
    )
    .unwrap();
    let mut config = pi_sdk::Config::new(project.clone(), agent.clone());
    config.discover_extensions = false;
    config.trust_override = Some(false);
    let pi = pi_sdk::Pi::builder(config.clone()).build().unwrap();
    let session = pi
        .sessions()
        .create_session(&project, agent.join("test.jsonl"))
        .await
        .unwrap();
    assert!(
        session
            .current()
            .runtime()
            .command_specs()
            .iter()
            .any(|spec| spec.name == "mcp")
    );
    let before = session.current();
    before.submit("/mcp paths").await.unwrap();
    fs::write(agent.join("mcp.json"), "invalid").unwrap();
    assert!(session.reload().await.is_err());
    assert!(Arc::ptr_eq(&before, &session.current()));
    assert!(!agent.join("test.jsonl").exists());
    fs::write(agent.join("mcp.json"), EMPTY).unwrap();
    session.current().submit("/mcp reload").await.unwrap();
    assert!(!Arc::ptr_eq(&before, &session.current()));
    assert!(!agent.join("test.jsonl").exists());
    fs::write(agent.join("mcp.json"), "invalid").unwrap();
    config.load_mcp_config = false;
    let isolated = pi_sdk::Pi::builder(config).build().unwrap();
    let acp = isolated
        .sessions()
        .create_session(&project, agent.join("acp.jsonl"))
        .await
        .unwrap();
    assert!(
        !acp.current()
            .runtime()
            .command_specs()
            .iter()
            .any(|spec| spec.name == "mcp")
    );
    pi.sessions().shutdown().await.unwrap();
    isolated.sessions().shutdown().await.unwrap();
}
