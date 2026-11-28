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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_list_needs_auth_includes_provider_and_redacted_guidance() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                [("www-authenticate", "Bearer realm=fixture")],
            )
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::create_dir(&agent).unwrap();
    fs::write(
        agent.join("mcp.json"),
        json!({"mcpServers":{"account":{"url":url,"auth":{"provider":"vendor"}}}}).to_string(),
    )
    .unwrap();
    fs::write(
        agent.join("auth.json"),
        json!({"vendor":{"type":"api_key","key":"fixture-private-token"}}).to_string(),
    )
    .unwrap();
    let output = run(root.path(), &["list", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"], json!([]));
    assert_eq!(report["servers"][0]["authProvider"], "vendor");
    assert_eq!(report["servers"][0]["state"], "needs-auth");
    assert!(
        report["servers"][0]["error"]
            .as_str()
            .unwrap()
            .contains("pi auth login vendor")
    );
    let plain = run(root.path(), &["list"]);
    assert_eq!(plain.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&plain.stdout).contains("pi auth login vendor"));
    for bytes in [&output.stdout, &output.stderr, &plain.stdout, &plain.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("fixture-private-token"));
    }
    assert!(!agent.join("sessions").exists());
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_list_opts_in_to_local_command_values_without_a_session() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let authenticated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = authenticated.clone();
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let observed = observed.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fixture-command-token");
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    [("www-authenticate", "Bearer realm=fixture")],
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    fs::create_dir(&agent).unwrap();
    let mut config = json!({"mcpServers":{"account":{"url":url,"headers":{
        "Authorization":"!printf x >> count; printf 'Bearer fixture-command-token'; printf fixture-command-error >&2"
    }}}});
    fs::write(agent.join("mcp.json"), config.to_string()).unwrap();
    let output = run(root.path(), &["list", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    // A command-produced Authorization header is still caller-owned static auth:
    // frozen Pi does not attach OAuth or offer MCP login for its rejected credentials.
    assert_eq!(report["servers"][0]["state"], "failed");
    let diagnostic = report["servers"][0]["error"].as_str().unwrap();
    assert!(diagnostic.contains("handshake failed"));
    assert!(!diagnostic.contains("pi mcp login"));
    assert_eq!(authenticated.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!agent.join("mcp-auth.json").exists());
    assert_eq!(fs::read(root.path().join("count")).unwrap(), b"x");
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("fixture-command"));
    }
    config["mcpServers"]["account"]["enabled"] = json!(false);
    fs::write(agent.join("mcp.json"), config.to_string()).unwrap();
    assert!(run(root.path(), &["list", "--json"]).status.success());
    assert_eq!(fs::read(root.path().join("count")).unwrap(), b"x");
    assert!(!agent.join("sessions").exists());
    server.abort();
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

#[test]
fn mcp_oauth_authoring_round_trips_unresolved_values_without_signing_in() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("must-not-execute");
    let command = format!("!touch {}", marker.display());
    for secret in ["${MCP_FIXTURE_UNSET_SECRET}", command.as_str()] {
        let output = run(
            root.path(),
            &[
                "add",
                "account",
                "--url",
                "https://example.invalid/mcp",
                "--oauth-client-id",
                "fixture-client",
                "--oauth-client-secret",
                secret,
                "--oauth-callback-port",
                "53700",
                "--oauth-client-name",
                "Fixture client",
                "--oauth-client-registration",
                "dcr",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value =
            serde_json::from_slice(&fs::read(root.path().join("agent/mcp.json")).unwrap()).unwrap();
        assert_eq!(
            value["mcpServers"]["account"]["oauth"],
            json!({
                "clientId":"fixture-client", "clientSecret":secret, "callbackPort":53700,
                "clientName":"Fixture client", "clientRegistration":"dcr"
            })
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
        assert!(!marker.exists());
        assert!(!root.path().join("agent/mcp-auth.json").exists());
        assert!(!root.path().join("agent/sessions").exists());
    }
    for mode in ["auto", "cimd", "dcr"] {
        let output = run(
            root.path(),
            &[
                "add",
                "account",
                "--url",
                "https://example.invalid/mcp",
                "--oauth-client-registration",
                mode,
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn mcp_invalid_oauth_authoring_never_modifies_the_existing_file() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        run(
            root.path(),
            &["add", "account", "--url", "https://example.invalid/mcp"]
        )
        .status
        .success()
    );
    let path = root.path().join("agent/mcp.json");
    let before = fs::read(&path).unwrap();
    for flags in [
        vec!["--oauth-callback-port", "0"],
        vec!["--oauth-callback-port", "65536"],
        vec!["--oauth-callback-port", "1.5"],
        vec!["--oauth-client-registration", "unknown"],
        vec!["--oauth-client-name", " "],
        vec!["--oauth-client-id", ""],
        vec![
            "--oauth-client-id",
            "fixture",
            "--oauth-client-registration",
            "cimd",
        ],
    ] {
        let mut arguments = vec!["add", "account", "--url", "https://example.invalid/mcp"];
        arguments.extend(flags);
        assert!(!run(root.path(), &arguments).status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    for flag in [
        "--oauth-client-id",
        "--oauth-client-secret",
        "--oauth-callback-port",
        "--oauth-client-name",
        "--oauth-client-registration",
    ] {
        let output = run(
            root.path(),
            &["add", "account", flag, "fixture", "--", "never-executed"],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("only applies to HTTP"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert!(!root.path().join("agent/sessions").exists());
}
