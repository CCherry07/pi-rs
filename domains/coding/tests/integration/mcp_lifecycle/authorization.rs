use super::*;

#[tokio::test]
async fn terminal_mcp_auth_publishes_on_next_idle_without_touching_another_live_session() {
    let fixture = ProductFixture::new(false).await;
    let manager = fixture.manager();
    let session = fixture.create(&manager, "terminal-auth.jsonl").await;
    let other = fixture.create(&manager, "healthy-auth.jsonl").await;
    let current = session.current();
    let other_current = other.current();
    fixture
        .http
        .trace
        .send_modify(|trace| trace.rejected_sessions.push("connection-1".into()));
    let context = ContextParts::new(current.plugin_context(PluginContextScope::Base));
    let outcome = context
        .session
        .run_ephemeral(
            EphemeralSessionRequest {
                system_prompt: Some("Call the fixture tool once.".into()),
                origin: "mcp-auth-handoff-test".into(),
                inherit_history: false,
                history_tail: None,
                messages: vec![Message::User(pi_core::UserMessage::text(
                    "invoke fixture",
                    1,
                ))],
                tools: vec![TOOL.into()],
                plugins: Vec::new(),
                model: None,
                thinking_level: None,
                max_tool_iterations: 2,
                max_input_tokens: None,
                compaction: None,
                timeout: TIMEOUT,
            },
            AbortHandle::new().1,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        pi_plugin::EphemeralSessionStatus::Completed,
        "{outcome:?}"
    );
    let results = outcome
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results.len(),
        1,
        "exactly one failure result for the admitted tool call"
    );
    assert!(results[0].is_error);
    let result = serde_json::to_string(&results[0].content).unwrap();
    assert!(result.contains("pi mcp login fixture"), "{result}");
    assert!(!result.contains("private-scope-marker"));
    assert_eq!(fixture.http.snapshot().calls, ["connection-1"]);
    assert!(Arc::ptr_eq(&current, &session.current()));
    assert!(
        current
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == TOOL)
    );
    assert!(
        !session.path().exists(),
        "ephemeral failure must not materialize the parent's journal"
    );

    // Registered commands intentionally run before activity/freshness admission.
    // They must remain callable without materializing the parent session.
    current.prompt("/mcp status").await.unwrap();
    assert!(Arc::ptr_eq(&current, &session.current()));
    assert!(!session.path().exists());
    current
        .prompt("continue after rejected MCP call")
        .await
        .unwrap();
    let published = session.current();
    assert!(!Arc::ptr_eq(&current, &published));
    // Raw subscriptions belong to one generation. Observe the newly published command
    // explicitly rather than pretending the retired runtime forwards its notices.
    let mut events = published.subscribe().events;
    published.prompt("/mcp status").await.unwrap();
    assert!(Arc::ptr_eq(&published, &session.current()));
    let status = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.event {
            AgentSessionEvent::PluginNotice { message, .. } => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        status.contains("fixture") && status.contains("needs sign-in"),
        "{status}"
    );
    assert!(!status.contains("private-scope-marker"));
    assert!(
        !published
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == TOOL)
    );
    assert!(
        current
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == TOOL),
        "retained registries stay immutable"
    );
    assert!(
        published
            .catalog()
            .command_specs()
            .iter()
            .any(|command| command.name == "mcp")
    );
    assert!(
        session.path().exists(),
        "the first parent assistant response materializes its journal"
    );
    let journal = std::fs::read_to_string(session.path()).unwrap();
    assert!(
        !journal.contains("fixture-call"),
        "ephemeral tool calls do not enter the parent journal"
    );
    assert!(!journal.contains("private-scope-marker"));
    assert!(
        !fixture
            .http
            .snapshot()
            .provider_tools
            .last()
            .unwrap()
            .iter()
            .any(|name| name == TOOL),
        "the next provider request cannot advertise the auth-failed tool"
    );
    assert!(
        !fixture.agent.join("mcp-auth.json").exists(),
        "challenge observation is not credential persistence"
    );
    assert_eq!(
        fixture.http.snapshot().initialized.len(),
        2,
        "no eager sign-in/reconnection"
    );

    assert!(Arc::ptr_eq(&other_current, &other.current()));
    invoke(&other, "connection-2").await;
    assert_eq!(
        fixture.http.snapshot().calls,
        ["connection-1", "connection-2"]
    );
    assert!(!other.path().exists());
    manager.shutdown().await.unwrap();
    fixture
        .http
        .deleted(&["connection-1", "connection-2"])
        .await;
}
