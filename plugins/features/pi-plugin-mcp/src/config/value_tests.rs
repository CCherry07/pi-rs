use super::*;
use pi_core::AbortHandle;

fn context(path: &Path) -> McpConfigContext<'_> {
    McpConfigContext {
        config_dir: path,
        default_cwd: path,
    }
}
fn entry(value: Value) -> McpConfigEntry {
    parse_config(&json!({"mcpServers":{"fixture":value}}).to_string())
        .unwrap()
        .pop()
        .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn command_output_is_fresh_unrescanned_and_never_resolved_by_transport_api() {
    let dir = tempfile::tempdir().unwrap();
    let config = entry(json!({
        "command":"!touch forbidden-command", "args":["!touch forbidden-arg"], "exposure":"direct",
        "env":{"VALUE":"!printf x >> invoked; printf '  !literal $MISSING  '"}
    }));
    let explicit = config.clone().resolve(context(dir.path())).unwrap();
    assert!(
        matches!(explicit.transport, McpTransport::Stdio { env, .. } if env["VALUE"].starts_with('!'))
    );
    assert!(!dir.path().join("invoked").exists());
    let error = config
        .clone()
        .resolve_for_connect(context(dir.path()), false, AbortHandle::new().1)
        .await
        .unwrap_err();
    assert!(error.contains("explicit local-library opt-in"));
    assert!(!dir.path().join("invoked").exists());
    for _ in 0..2 {
        let resolved = config
            .clone()
            .resolve_for_connect(context(dir.path()), true, AbortHandle::new().1)
            .await
            .unwrap();
        let McpTransport::Stdio {
            command, args, env, ..
        } = resolved.transport
        else {
            panic!("stdio")
        };
        assert_eq!(command, "!touch forbidden-command");
        assert_eq!(args, ["!touch forbidden-arg"]);
        assert_eq!(env["VALUE"], "!literal $MISSING");
    }
    assert_eq!(std::fs::read(dir.path().join("invoked")).unwrap(), b"xx");
    assert!(!dir.path().join("forbidden-command").exists());
    assert!(!dir.path().join("forbidden-arg").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn headers_resolve_at_connection_and_oauth_secret_waits_for_explicit_auth() {
    let dir = tempfile::tempdir().unwrap();
    let config = entry(json!({
        "url":"https://example.invalid/mcp",
        "headers":{"X-Secret":"!printf 'header-value';\nprintf ignored >&2; printf x >> header-count"},
        "oauth":{"clientId":"fixture","clientSecret":"!printf x >> secret-count; printf client-value"}
    }));
    let identity = config.clone().resolve_identity().unwrap();
    assert!(
        identity
            .oauth
            .unwrap()
            .client_secret
            .unwrap()
            .starts_with('!')
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let resolved = config
        .resolve_for_connect(context(dir.path()), true, AbortHandle::new().1)
        .await
        .unwrap();
    let McpTransport::Http { headers, .. } = resolved.transport else {
        panic!("HTTP")
    };
    assert_eq!(headers["X-Secret"], "header-value");
    assert!(!dir.path().join("secret-count").exists());
    let settings = LocalValues {
        cwd: dir.path().into(),
        allow_commands: true,
    }
    .oauth(resolved.oauth.unwrap(), AbortHandle::new().1)
    .await
    .unwrap();
    assert_eq!(settings.client_secret.unwrap(), "client-value");
    assert_eq!(
        std::fs::read(dir.path().join("header-count")).unwrap(),
        b"x"
    );
    assert_eq!(
        std::fs::read(dir.path().join("secret-count")).unwrap(),
        b"x"
    );
}

#[tokio::test]
async fn static_authorization_and_disabled_entries_do_not_resolve_oauth_helpers() {
    let dir = tempfile::tempdir().unwrap();
    for enabled in [false, true] {
        let config = entry(json!({
            "url":"https://example.invalid/mcp", "enabled":enabled,
            "headers":{"AUTHORIZATION":"Bearer static"},
            "oauth":{"clientId":"fixture","clientSecret":"!touch forbidden; printf secret"}
        }));
        let result = config
            .resolve_for_connect(context(dir.path()), true, AbortHandle::new().1)
            .await;
        assert_eq!(result.is_ok(), enabled);
        assert!(!dir.path().join("forbidden").exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn secret_failures_and_cancel_are_redacted_with_no_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    for command in [
        "!printf private-output; printf private-error >&2; exit 4",
        "!printf '   '",
        "!printf '\\377'",
        "!while :; do printf 'private-large-output'; done",
        "!while :; do printf 'private-large-stderr' >&2; done",
    ] {
        let error = values::resolve(command, dir.path(), true, AbortHandle::new().1)
            .await
            .unwrap_err();
        assert!(!error.contains("private"), "{error}");
        assert!(!error.contains("printf"), "{error}");
    }
    let (abort, signal) = AbortHandle::new();
    abort.abort();
    assert!(
        values::resolve("!touch forbidden", dir.path(), true, signal)
            .await
            .unwrap_err()
            .contains("cancelled")
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn inherited_environment_templates_are_resolved_at_connection_not_parse_time() {
    let dir = tempfile::tempdir().unwrap();
    let config = entry(
        json!({"command":"unused","env":{"VALUE":"prefix:$PATH/end","LITERAL":"$!not-a-command"}}),
    );
    let resolved = config
        .resolve_for_connect(context(dir.path()), false, AbortHandle::new().1)
        .await
        .unwrap();
    let McpTransport::Stdio { env, .. } = resolved.transport else {
        panic!("stdio")
    };
    assert_eq!(
        env["VALUE"],
        format!("prefix:{}/end", std::env::var("PATH").unwrap())
    );
    assert_eq!(env["LITERAL"], "!not-a-command");
}

#[cfg(unix)]
#[tokio::test]
async fn command_resolution_honors_cancellation_and_the_ten_second_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (abort, signal) = AbortHandle::new();
    let cancel = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        abort.abort();
    });
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        values::resolve("!sleep 30; touch forbidden", dir.path(), true, signal),
    )
    .await
    .unwrap()
    .unwrap_err();
    cancel.await.unwrap();
    assert!(error.contains("cancelled"));
    let error = tokio::time::timeout(
        Duration::from_secs(15),
        values::resolve(
            "!sleep 30; touch forbidden",
            dir.path(),
            true,
            AbortHandle::new().1,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("timed out"));
    assert!(!dir.path().join("forbidden").exists());
}
