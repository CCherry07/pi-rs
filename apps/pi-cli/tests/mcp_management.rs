//! Session-free MCP commands through the real native entrypoint.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pi"))
        .args(["--cwd", root.to_str().unwrap(), "--agent-dir"])
        .arg(root.join("agent"))
        .arg("--no-approve")
        .arg("mcp")
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn mcp_list_json_preserves_diagnostics_trust_and_exit_status_without_a_session() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(root.path().join(".pi")).unwrap();
    // Management must not construct a model runtime or parse project MCP before trust.
    fs::write(agent.join("models.json"), "invalid model configuration").unwrap();
    fs::write(
        root.path().join(".pi/mcp.json"),
        "invalid untrusted configuration",
    )
    .unwrap();
    fs::write(
        agent.join("mcp.json"),
        json!({"mcpServers": {
            "parked": {"command":"never-executed", "enabled":false},
            "invalid": {"args":["missing command"]}
        }})
        .to_string(),
    )
    .unwrap();
    let output = run(root.path(), &["list", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["servers"][0]["name"], "parked");
    assert_eq!(report["servers"][0]["state"], "disabled");
    assert_eq!(report["errors"].as_array().unwrap().len(), 1);
    assert!(report["note"].as_str().unwrap().contains("not trusted"));
    assert!(!agent.join("sessions").exists());

    let removed = run(root.path(), &["remove", "invalid"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let output = run(root.path(), &["list", "--json"]);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["errors"].as_array().unwrap().is_empty());
    assert!(!agent.join("sessions").exists());
}

#[test]
fn mcp_logout_uses_the_durable_store_without_touching_other_servers() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        run(
            root.path(),
            &["add", "account", "--url", "https://example.invalid/mcp"]
        )
        .status
        .success()
    );
    let store =
        pi_plugin_mcp::McpOAuthCredentialStore::new(root.path().join("agent/mcp-auth.json"));
    for name in ["account", "other"] {
        store
            .save_tokens(
                name,
                "https://example.invalid/mcp",
                "fixture",
                pi_plugin_mcp::McpOAuthTokens {
                    access_token: "fixture-token".into(),
                    token_type: "Bearer".into(),
                    refresh_token: None,
                    expires_in: None,
                    scope: None,
                },
            )
            .unwrap();
    }
    let output = run(root.path(), &["logout", "account"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Signed out"));
    assert!(
        !store
            .has_credentials("account", "https://example.invalid/mcp")
            .unwrap()
    );
    assert!(
        store
            .has_credentials("other", "https://example.invalid/mcp")
            .unwrap()
    );
    assert!(!root.path().join("agent/sessions").exists());
}

#[test]
fn mcp_authoring_preserves_child_arguments_and_never_grants_trust() {
    let root = tempfile::tempdir().unwrap();
    let help = run(root.path(), &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--bearer-token-env-var"));
    let output = run(
        root.path(),
        &[
            "add",
            "--local",
            "docs",
            "--",
            "node",
            "server.js",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value =
        serde_json::from_str(&fs::read_to_string(root.path().join(".pi/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        value["mcpServers"]["docs"]["args"],
        json!(["server.js", "--json"])
    );
    let output = run(root.path(), &["list", "--json"]);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["servers"].as_array().unwrap().is_empty());
    assert!(report["note"].is_string());
    assert!(!root.path().join("agent/sessions").exists());
    assert!(!run(root.path(), &["login", "docs"]).status.success());
    assert!(!root.path().join("agent/mcp-auth.json").exists());
}
