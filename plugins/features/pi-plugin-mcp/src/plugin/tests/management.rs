use super::super::manager::Action;
use super::*;
use serde_json::json;

#[tokio::test]
async fn project_toggles_preserve_global_definition_and_failed_reload_context() {
    for enabled in [false, true] {
        for reload_succeeds in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let global = directory.path().join("mcp.json");
            let cwd = directory.path().join("project");
            std::fs::create_dir(&cwd).unwrap();
            let original = json!({"mcpServers":{"docs":{"command":"unused","enabled":enabled,"env":{"TOKEN":"global-only-secret"}}}}).to_string();
            std::fs::write(&global, &original).unwrap();
            let library = McpLibrary::new(directory.path(), Some(&cwd), true);
            let command = McpCommand {
                rows: library.read(McpScope::Global).unwrap().servers,
                library: library.clone(),
                pool: McpToolSet::connect_with_policies(vec![]).await.unwrap(),
            };
            let expected = if enabled {
                Action::Disable(McpScope::Project)
            } else {
                Action::Enable(McpScope::Project)
            };
            let index = command
                .manager_actions(&command.rows[0])
                .iter()
                .position(|(action, _)| *action == expected)
                .unwrap();
            let next_access = Arc::new(ManagerAccess::new(vec![], None));
            let access = Arc::new(ManagerAccess::new(
                vec![Some(0), Some(index)],
                reload_succeeds.then(|| PluginContextEpoch::new(next_access.clone())),
            ));
            let epoch = PluginContextEpoch::new(access.clone());
            *access.old.lock().unwrap() = Some(epoch.clone());
            let context = CommandContext::with_plugin_context(
                cwd.clone(),
                pi_core::AbortHandle::new().1,
                epoch.command_context(),
            );
            let result = command.execute(context.clone(), String::new()).await;
            assert_eq!(result.is_ok(), reload_succeeds);
            assert_eq!(access.reloads.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(std::fs::read_to_string(global).unwrap(), original);
            let project: serde_json::Value =
                serde_json::from_slice(&std::fs::read(cwd.join(".pi/mcp.json")).unwrap()).unwrap();
            assert_eq!(
                project,
                json!({"version":1,"mcpServers":{"docs":{"enabled":!enabled}}})
            );
            assert_eq!(
                next_access.notices.lock().unwrap().len(),
                usize::from(reload_succeeds)
            );
            assert!(access.notices.lock().unwrap().is_empty());
            if !reload_succeeds {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("previous generation remains active")
                );
                assert!(context.ui.is_available().unwrap());
            }
            assert!(!directory.path().join("mcp-auth.json").exists());
            access.old.lock().unwrap().take();

            let untrusted = McpCommand {
                library: McpLibrary::new(directory.path(), Some(&cwd), false),
                ..command
            };
            assert!(!untrusted.manager_actions(&untrusted.rows[0]).iter().any(
                |(action, _)| matches!(
                    action,
                    Action::Enable(McpScope::Project) | Action::Disable(McpScope::Project)
                )
            ));
        }
    }
}

#[tokio::test]
async fn extension_manager_is_runtime_only_and_uses_registration_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("extensions/owned.ts");
    let library = McpLibrary::new(directory.path(), None, false)
        .with_configured_servers(false)
        .with_extension_servers([crate::McpExtensionServerDefinition::new(
            "extension-owned",
            json!({"url":"https://mcp.example.test/rpc","oauth":{}}),
            &source,
        )])
        .unwrap();
    let row = McpServerRow {
        name: "extension-owned".into(),
        scope: McpScope::Extension,
        defining_scope: McpScope::Extension,
        supports_oauth: true,
        auth_provider: None,
        transport: "http".into(),
        enabled: true,
        exposure: McpExposure::Codemode,
    };
    let command = McpCommand {
        library: library.clone(),
        rows: vec![row.clone()],
        pool: McpToolSet::connect_with_policies(vec![]).await.unwrap(),
    };

    assert_eq!(
        library
            .server_source(&row.name, row.defining_scope)
            .unwrap(),
        source
    );
    let actions = command
        .manager_actions(&row)
        .into_iter()
        .map(|(action, _)| action)
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            Action::Inspect,
            Action::Tools,
            Action::SignOut,
            Action::Reconnect,
        ]
    );
    assert!(!directory.path().join("mcp.json").exists());

    let disabled = McpServerRow {
        enabled: false,
        ..row
    };
    assert_eq!(
        command
            .manager_actions(&disabled)
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<_>>(),
        [Action::Inspect]
    );
}

struct NoUi;
impl ModelsContextAccess for NoUi {}
impl UiContextAccess for NoUi {
    fn has_ui(&self) -> PluginContextResult<bool> {
        Ok(false)
    }
}
impl SessionContextAccess for NoUi {}

#[tokio::test]
async fn eligible_pickers_prefer_pending_oauth_and_keep_cancellation_read_only() {
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
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp.json");
    let original = json!({"mcpServers":{
        "account":{"url":url}, "another":{"url":url}, "disabled":{"url":url,"enabled":false},
        "provider":{"url":url,"auth":{"provider":"vendor"}},
        "header":{"url":url,"headers":{"Authorization":"Bearer static-secret"}}, "stdio":{"command":"unused"}
    }}).to_string();
    std::fs::write(&path, &original).unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let command = McpCommand {
        rows: library.read(McpScope::Global).unwrap().servers,
        library,
        pool: McpToolSet::connect_generation(
            vec![(
                crate::McpServerConfig::http("account", url),
                crate::policy::McpServerPolicy::default(),
            )],
            crate::McpClientAuth::default(),
            true,
        )
        .await
        .unwrap(),
    };
    let completions = pi_plugin::CommandCompletions {
        name: "mcp".into(),
        items: command.argument_completions(),
    };
    for (prefix, expected) in [
        ("", vec!["login ", "logout ", "reconnect "]),
        ("l", vec!["login ", "logout "]),
        ("login ", vec!["login account", "login another"]),
        ("logout a", vec!["logout account", "logout another"]),
        (
            "reconnect ",
            vec![
                "reconnect account",
                "reconnect another",
                "reconnect header",
                "reconnect provider",
                "reconnect stdio",
            ],
        ),
        ("login account ", vec![]),
    ] {
        assert_eq!(
            completions
                .matching(prefix)
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{prefix:?}"
        );
    }
    assert_eq!(
        completions.matching("login ac")[0].description.as_deref(),
        Some("needs sign-in")
    );
    let access = Arc::new(ManagerAccess::new(vec![Some(1), None], None));
    let context = CommandContext::with_plugin_context(
        directory.path().into(),
        pi_core::AbortHandle::new().1,
        PluginContextEpoch::new(access.clone()).command_context(),
    );
    assert_eq!(
        command
            .pick_server(&context, true)
            .await
            .unwrap()
            .unwrap()
            .name,
        "account"
    );
    assert!(
        access.dialogs.lock().unwrap().is_empty(),
        "the sole pending OAuth server is automatic"
    );
    assert_eq!(
        command
            .pick_server(&context, false)
            .await
            .unwrap()
            .unwrap()
            .name,
        "another"
    );
    assert!(
        command
            .pick_server(&context, false)
            .await
            .unwrap()
            .is_none()
    );
    let oauth = command.eligible_server("account", true).unwrap();
    let actions = command.manager_actions(oauth);
    assert!(actions.iter().any(|(action, _)| *action == Action::SignIn));
    assert!(
        !actions
            .iter()
            .any(|(action, _)| matches!(action, Action::Tools | Action::SignOut))
    );
    for name in ["disabled", "provider", "header", "stdio"] {
        assert!(command.eligible_server(name, true).is_err());
        let row = command.rows.iter().find(|row| row.name == name).unwrap();
        assert!(
            !command
                .manager_actions(row)
                .iter()
                .any(|(action, _)| matches!(action, Action::SignIn | Action::SignOut))
        );
    }
    let context = CommandContext::with_plugin_context(
        directory.path().into(),
        pi_core::AbortHandle::new().1,
        PluginContextEpoch::new(Arc::new(NoUi)).command_context(),
    );
    assert!(
        command
            .pick_server(&context, false)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("Name an MCP server")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    assert!(!directory.path().join("mcp-auth.json").exists());
    assert_eq!(access.reloads.load(std::sync::atomic::Ordering::SeqCst), 0);
    command.pool.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn metadata_and_unknown_names_do_not_resolve_secrets_or_reveal_untrusted_names() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    std::fs::create_dir_all(project.join(".pi")).unwrap();
    std::fs::write(directory.path().join("mcp.json"), r#"{"mcpServers":{"account":{"url":"http://127.0.0.1:1/mcp","auth":{"provider":"vendor"},"enabled":false},"stdio":{"command":"unused","enabled":false,"env":{"TOKEN":"secret-marker"}}}}"#).unwrap();
    std::fs::write(
        project.join(".pi/mcp.json"),
        r#"{"mcpServers":{"untrusted-private":{"command":"do-not-execute"}}}"#,
    )
    .unwrap();
    let library = McpLibrary::new(directory.path(), Some(&project), false);
    let inspection = library.inspect().await;
    let json = serde_json::to_value(inspection).unwrap();
    assert_eq!(json["servers"][0]["authProvider"], "vendor");
    assert!(json["servers"][1].get("authProvider").is_none());
    assert!(!json.to_string().contains("secret-marker"));
    let error = library.logout("acount", None).unwrap_err();
    for expected in [
        "Did you mean \"account\"",
        "Configured: account, stdio",
        "project is not trusted",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    assert!(!error.contains("untrusted-private"));
    assert!(!error.contains("secret-marker"));
    assert!(!directory.path().join("mcp-auth.json").exists());
}
