#![cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use pi_core::AbortHandle;
use pi_plugin::{RegistriesBuilder, ToolContext, ToolUpdateSink};
use pi_plugin_mcp::{McpLibrary, McpScope, McpServerConfig, McpToolSet};
use serde_json::json;

fn config(mode: &str) -> McpServerConfig {
    McpServerConfig::stdio("fixture", "python3")
        .args([
            format!(
                "{}/tests/fixtures/diagnostics.py",
                env!("CARGO_MANIFEST_DIR")
            ),
            mode.into(),
        ])
        .env(BTreeMap::from([(
            "FIXTURE_SECRET".into(),
            "local-fake-private-value".into(),
        )]))
}

fn library(dir: &std::path::Path, mode: &str) -> McpLibrary {
    let library = McpLibrary::new(dir, None, false);
    let config = config(mode);
    let pi_plugin_mcp::McpTransport::Stdio {
        command, args, env, ..
    } = config.transport
    else {
        unreachable!()
    };
    let content = json!({"mcpServers":{"fixture": {"command":command,"args":args,"env":env,"exposure":"direct"}}});
    let doc = library.read(McpScope::Global).unwrap();
    library
        .save(McpScope::Global, &doc.revision, &content.to_string())
        .unwrap();
    library
}

#[tokio::test]
async fn failed_initialization_drains_large_stderr_and_exposes_only_redacted_tail() {
    for mode in ["boot", "discovery"] {
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            McpToolSet::connect(vec![config(mode)]),
        )
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
        assert!(error.contains("configuration unavailable"), "{error}");
        assert!(error.ends_with("last diagnostic line"), "{error}");
        assert!(error.contains("[REDACTED]"));
        assert!(!error.contains("local-fake-private-value"));
        assert!(error.len() < 2200, "unbounded error: {}", error.len());
    }
}

#[tokio::test]
async fn library_notifications_go_to_private_rotated_sink_not_stderr_tail() {
    let dir = tempfile::tempdir().unwrap();
    let library = library(dir.path(), "normal");
    let names = library.test("fixture").await.unwrap();
    assert_eq!(names, ["mcp__fixture__close"]);
    let text = fs::read_to_string(dir.path().join("mcp.log")).unwrap();
    assert!(
        text.contains("Z [fixture] warning fixture: first line\n    credential=[REDACTED]\n"),
        "{text}"
    );
    assert!(!text.contains("local-fake-private-value"));
    assert!(!dir.path().join("sessions").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.path().join("mcp.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn unavailable_log_path_does_not_break_discovery_or_requests() {
    let dir = tempfile::tempdir().unwrap();
    let library = library(dir.path(), "normal");
    fs::create_dir(dir.path().join("mcp.log")).unwrap();
    assert_eq!(
        library.test("fixture").await.unwrap(),
        ["mcp__fixture__close"]
    );
}

#[tokio::test]
async fn disconnect_has_redacted_tail_and_explicit_client_creates_no_log() {
    let dir = tempfile::tempdir().unwrap();
    let pool = McpToolSet::connect(vec![config("normal").cwd(dir.path())])
        .await
        .unwrap();
    let (_, registry) = RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let (_, signal) = AbortHandle::new();
    let (updates, _) = ToolUpdateSink::channel();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        registry.tool("mcp__fixture__close").unwrap().execute(
            ToolContext::standalone(PathBuf::from(dir.path()), signal),
            "close".into(),
            json!({}),
            updates,
        ),
    )
    .await
    .unwrap()
    .unwrap_err()
    .to_string();
    assert!(error.contains("Connection closed"), "{error}");
    assert!(error.ends_with("last diagnostic line"), "{error}");
    assert!(!error.contains("local-fake-private-value"));
    assert!(error.len() < 2200);
    pool.shutdown().await.unwrap();
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        0,
        "transport-only clients never infer a log path"
    );
}

#[tokio::test]
async fn metadata_and_disabled_or_untrusted_config_never_materialize_log_files() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let cwd = dir.path().join("project");
    fs::create_dir_all(cwd.join(".pi")).unwrap();
    fs::write(
        cwd.join(".pi/mcp.json"),
        json!({"mcpServers":{"ignored":{"command":"absent"}}}).to_string(),
    )
    .unwrap();
    let library = McpLibrary::new(&agent, Some(&cwd), false);
    let doc = library.read(McpScope::Global).unwrap();
    library
        .save(
            McpScope::Global,
            &doc.revision,
            r#"{"mcpServers":{"disabled":{"command":"absent","enabled":false}}}"#,
        )
        .unwrap();
    assert!(library.inspect().await.is_ok());
    let _ = library.prepare().await.unwrap();
    assert!(!agent.join("mcp.log").exists());
    assert!(!agent.join("mcp.log.lock").exists());
}
