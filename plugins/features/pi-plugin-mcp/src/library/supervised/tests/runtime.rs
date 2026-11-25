use super::*;
use pi_plugin::{
    SessionIdentity, SessionPluginContext, SessionShutdownEvent, SessionShutdownReason,
    SessionStartEvent, SessionStartReason,
};

fn context(root: &Path) -> SessionPluginContext {
    SessionPluginContext::unavailable_for_testing(
        "mcp".into(),
        1,
        SessionIdentity {
            id: "test".into(),
            path: root.join("session.jsonl"),
            cwd: root.into(),
            parent_session_id: None,
        },
    )
}

#[tokio::test]
async fn publication_precedes_failed_pending_direct_notice_and_shutdown_is_reason_aware() {
    let fixture = Fixture::new(true, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"server":fixture.entry("direct")}));
    let supervisor = McpSupervisor::new();
    let first = library
        .prepare_supervised_with_budget(supervisor.clone(), BTreeMap::new(), Duration::ZERO)
        .await
        .unwrap();
    let weak = first.readiness_watch.as_ref().unwrap().downgrade();
    assert!(matches!(
        first.server_readiness["server"],
        McpServerReadiness::Pending
    ));
    assert!(weak.active_supervisor().is_none());
    // Direct-only configuration would not use session capabilities. Failure here proves
    // the pending direct diagnostic tried the unavailable UI after publishing its lease.
    let context = context(directory.path());
    assert!(
        first
            .plugin
            .session_start(
                &context,
                &SessionStartEvent {
                    reason: SessionStartReason::Startup,
                    previous_session_file: None
                }
            )
            .await
            .is_err()
    );
    assert!(weak.active_supervisor().is_some());
    configure(&library, json!({"server":fixture.entry("codemode")}));
    let next = prepare(&library, &supervisor).await;
    let next_weak = next.readiness_watch.as_ref().unwrap().downgrade();
    first
        .plugin
        .session_shutdown(
            &context,
            &SessionShutdownEvent {
                reason: SessionShutdownReason::Reload,
                target_session_file: None,
            },
        )
        .await
        .unwrap();
    assert!(weak.is_retired());
    assert!(!next_weak.is_retired());
    fixture.gate.add_permits(1);
    inventory(&supervisor, "server").await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    next.plugin
        .session_shutdown(
            &context,
            &SessionShutdownEvent {
                reason: SessionShutdownReason::Quit,
                target_session_file: None,
            },
        )
        .await
        .unwrap();
    assert!(next_weak.is_retired());
    assert!(supervisor.snapshot().servers.is_empty());
    assert!(
        library
            .prepare_supervised_generation(supervisor, BTreeMap::new())
            .await
            .is_err()
    );
}

async fn prompt(prepared: &PreparedMcpGeneration, root: &Path) -> pi_plugin::BeforeAgentStartPatch {
    prepared
        .plugin
        .before_agent_start(
            pi_plugin::AgentPluginContext::unavailable_for_testing(
                "mcp".into(),
                pi_core::RunId::next(),
                root.into(),
                pi_core::AbortHandle::new().1,
            ),
            pi_plugin::BeforeAgentStartEvent {
                system_prompt: "base".into(),
                prompt_state: None,
                input_messages: vec![],
                active_tools: vec![],
                provider_id: "scripted".into(),
                model_id: "test".into(),
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn pending_auth_and_failed_indirect_hints_are_bounded_frozen_and_safe() {
    let slow = Fixture::new(true, 200, false).await;
    let auth = Fixture::new(false, 401, false).await;
    let failed = Fixture::new(false, 400, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let mut entry = slow.entry("codemode");
    entry["description"] = json!(format!("Configured reference {}", "😀".repeat(300)));
    configure(
        &library,
        json!({"slow":entry,"auth":auth.entry("deferred"),"failed":failed.entry("deferred"),"hidden":{"command":"/nonexistent-mcp-fixture","exposure":"hidden"},"disabled":{"enabled":false}}),
    );
    let supervisor = McpSupervisor::new();
    let _starting = prepare(&library, &supervisor).await;
    supervisor
        .wait_for(&["auth".into(), "failed".into()], Duration::from_secs(3))
        .await;
    let prepared = prepare(&library, &supervisor).await;
    let patch = prompt(&prepared, directory.path()).await;
    assert_eq!(patch.prompt_contributions.len(), 1);
    let text = patch.system_prompt.unwrap();
    assert!(text.contains("mcp__slow (codemode): Pending;"));
    assert!(text.contains("next idle refresh"));
    assert!(text.contains("Configured reference"));
    assert!(text.contains("mcp__auth (tool_search): Needs sign-in;"));
    assert!(text.contains("mcp__failed (tool_search): Connection failed;"));
    assert!(!text.contains("mcp__hidden"));
    assert!(!text.contains("mcp__disabled"));
    assert!(!text.contains("realm=fixture"));
    assert!(!text.contains("400"));
    assert!(patch.prompt_contributions[0].text.encode_utf16().count() < 4200);
    let (_, registry) = pi_plugin::RegistriesBuilder::new()
        .register_plugins(vec![prepared.plugin.clone()])
        .unwrap();
    assert!(
        !registry
            .tool_infos()
            .iter()
            .any(|tool| tool.spec.name.starts_with("mcp__"))
    );
    slow.gate.add_permits(1);
    inventory(&supervisor, "slow").await;
    assert_eq!(
        prompt(&prepared, directory.path())
            .await
            .system_prompt
            .as_deref(),
        Some(text.as_str())
    );
    let next = prepare(&library, &supervisor).await;
    assert!(
        !prompt(&next, directory.path())
            .await
            .system_prompt
            .unwrap()
            .contains("Pending;")
    );
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn manual_reconnect_uses_one_live_attempt_and_retired_views_cannot_probe() {
    let fixture = Fixture::new(false, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    configure(&library, json!({"server":fixture.entry("direct")}));
    assert!(library.reconnect_active("server").is_none());
    let supervisor = McpSupervisor::new();
    let first = prepare(&library, &supervisor).await;
    let mut bound = library.clone();
    bound.generation = Some(first.readiness_watch.as_ref().unwrap().downgrade());
    assert!(bound.reconnect_active("server").unwrap().is_err());
    assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    bound.publish_generation();
    bound.reconnect_active("server").unwrap().unwrap();
    let next = prepare(&library, &supervisor).await;
    assert!(matches!(
        next.server_readiness["server"],
        McpServerReadiness::Ready
    ));
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    bound.shutdown_generation(true).await.unwrap();
    assert!(bound.reconnect_active("server").unwrap().is_err());
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
}
