//! Canonical IDs are learned from a real prior inventory, never de-normalized by tests.
use super::*;

fn named_servers(fixture: &ProductFixture, servers: Value) {
    std::fs::write(
        fixture.agent.join("mcp.json"),
        json!({"mcpServers":servers}).to_string(),
    )
    .unwrap();
}

fn entry(fixture: &ProductFixture, path: &str, policy: &Value) -> Value {
    let mut entry = fixture.http.entry(path);
    entry["exposure"] = json!("deferred");
    for (key, value) in policy.as_object().unwrap() {
        entry[key] = value.clone();
    }
    entry
}

fn tool(name: &str) -> Value {
    json!({"name":name,"inputSchema":{"type":"object"}})
}

async fn saved_from_inventory(
    fixture: &ProductFixture,
    server: &str,
    tools: Vec<Value>,
) -> (PathBuf, String) {
    fixture
        .http
        .trace
        .send_modify(|trace| trace.remote_tools = Some(tools));
    named_servers(
        fixture,
        json!({(server):entry(fixture, "/mcp", &json!({}))}),
    );
    let manager = fixture.manager();
    let session = fixture.create(&manager, "identity.jsonl").await;
    let name = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(spec) = session
                .current()
                .catalog()
                .tool_specs()
                .into_iter()
                .find(|spec| spec.name.starts_with("mcp__"))
            {
                break spec.name;
            }
            session.reload().await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    session.current().set_active_tools([name.clone()]).unwrap();
    session
        .current()
        .prompt("materialize the selected tool")
        .await
        .unwrap();
    let path = session.path().to_owned();
    assert!(path.exists());
    manager.shutdown().await.unwrap();
    fixture.http.deleted(&["connection-1"]).await;
    (path, name)
}

fn is_desired(session: &PiSession, name: &str) -> bool {
    session
        .current()
        .desired_active_tools()
        .unwrap()
        .iter()
        .any(|item| item == name)
}

fn is_active(session: &PiSession, name: &str) -> bool {
    session
        .current()
        .snapshot()
        .agent
        .active_tools
        .iter()
        .any(|item| item == name)
}

fn last_request_declares(fixture: &ProductFixture, name: &str) -> bool {
    fixture
        .http
        .snapshot()
        .provider_tools
        .last()
        .unwrap()
        .iter()
        .any(|item| item == name)
}

#[tokio::test]
async fn delayed_mixed_hashed_collision_and_truncated_identities_require_exact_inventory() {
    let cases = [
        (
            "slow".to_owned(),
            vec![tool("echo.value")],
            json!({"toolExposure":{"echo_value":"hidden"}}),
        ),
        (
            "slow".to_owned(),
            vec![tool(&"x".repeat(100))],
            json!({"toolExposure":{"secret*":"hidden"}}),
        ),
        (
            "slow".to_owned(),
            vec![tool("a-b"), tool("a_b")],
            json!({"toolExposure":{"secret*":"hidden"}}),
        ),
        ("namespace".repeat(8), vec![tool("echo")], json!({})),
    ];
    for (server, tools, policy) in cases {
        let fixture = ProductFixture::new(false).await;
        let (path, name) = saved_from_inventory(&fixture, &server, tools).await;
        named_servers(
            &fixture,
            json!({(server.clone()):entry(&fixture, "/slow", &policy)}),
        );
        let manager = fixture.manager();
        let session = manager
            .open_session(&path, Default::default())
            .await
            .unwrap();
        fixture.http.until(|trace| trace.slow_listing).await;
        assert!(is_desired(&session, &name), "pending identity lost: {name}");
        assert!(!is_active(&session, &name));
        assert!(
            !session
                .current()
                .catalog()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == name)
        );
        session
            .current()
            .prompt("withhold unresolved identity")
            .await
            .unwrap();
        assert!(is_desired(&session, &name));
        assert!(!last_request_declares(&fixture, &name));
        fixture.http.slow_gate.add_permits(1);
        super::super::super::mcp_test_support::wait_for_tools(&session, &[&name]).await;
        assert!(
            is_active(&session, &name),
            "exact ready identity not restored: {name}"
        );
        session
            .current()
            .prompt("use only the exact ready identity")
            .await
            .unwrap();
        assert!(last_request_declares(&fixture, &name));
        assert_eq!(fixture.http.snapshot().initialized.len(), 2);
        assert!(fixture.http.snapshot().calls.is_empty());
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn completed_hidden_or_renamed_inventory_prunes_unresolved_intent() {
    for hidden in [true, false] {
        let fixture = ProductFixture::new(false).await;
        let (path, name) = saved_from_inventory(&fixture, "slow", vec![tool("echo.value")]).await;
        let policy = if hidden {
            json!({"toolExposure":{"echo.value":"hidden"}})
        } else {
            json!({"toolExposure":{"secret*":"hidden"}})
        };
        if !hidden {
            fixture
                .http
                .trace
                .send_modify(|trace| trace.slow_tools = Some(vec![tool("renamed")]));
        }
        named_servers(&fixture, json!({"slow":entry(&fixture, "/slow", &policy)}));
        let manager = fixture.manager();
        let session = manager
            .open_session(&path, Default::default())
            .await
            .unwrap();
        fixture.http.until(|trace| trace.slow_listing).await;
        assert!(is_desired(&session, &name));
        assert!(!is_active(&session, &name));
        fixture.http.slow_gate.add_permits(1);
        super::super::super::mcp_test_support::wait_for_status(&session, "slow", "connected").await;
        assert!(!is_desired(&session, &name));
        assert!(!is_active(&session, &name));
        assert!(
            !session
                .current()
                .catalog()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == name)
        );
        session
            .current()
            .prompt("do not execute a hidden or renamed identity")
            .await
            .unwrap();
        assert!(!last_request_declares(&fixture, &name));
        assert!(fixture.http.snapshot().calls.is_empty());
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn overlapping_pending_namespace_quarantines_even_a_ready_alias_until_resolved() {
    let fixture = ProductFixture::new(false).await;
    let (path, name) = saved_from_inventory(&fixture, "docs", vec![tool("nested__echo")]).await;
    fixture
        .http
        .trace
        .send_modify(|trace| trace.slow_tools = Some(vec![tool("echo")]));
    named_servers(
        &fixture,
        json!({
            "docs":entry(&fixture, "/mcp", &json!({})),
            "docs__nested":entry(&fixture, "/slow", &json!({}))
        }),
    );
    let manager = fixture.manager();
    let session = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    fixture.http.until(|trace| trace.slow_listing).await;
    super::super::super::mcp_test_support::wait_for_status(&session, "docs", "connected").await;
    assert!(is_desired(&session, &name));
    assert!(!is_active(&session, &name));
    assert!(
        !session
            .current()
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == name),
        "partial inventory must not publish a guessed alias"
    );
    session
        .current()
        .prompt("namespace identity remains unresolved")
        .await
        .unwrap();
    assert!(!last_request_declares(&fixture, &name));
    fixture.http.slow_gate.add_permits(1);
    super::super::super::mcp_test_support::wait_for_tools(&session, &[&name]).await;
    assert!(is_desired(&session, &name));
    assert!(is_active(&session, &name));
    let tools = session.current().catalog().tool_specs();
    assert_eq!(tools.iter().filter(|tool| tool.name == name).count(), 1);
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool.name.starts_with("mcp__docs__nested__echo"))
            .count(),
        2
    );
    assert_eq!(fixture.http.snapshot().initialized.len(), 3);
    assert!(fixture.http.snapshot().calls.is_empty());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn bounded_setup_timeout_preserves_intent_for_later_reopen_without_replaying_calls() {
    let fixture = ProductFixture::new(false).await;
    configure(&fixture, Some(pending_entry(&fixture)));
    let path = saved(&fixture, "timeout-selection.jsonl");
    let manager = fixture.manager();
    let session = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    fixture.http.until(|trace| trace.slow_listing).await;
    let mut trace = fixture.http.trace.subscribe();
    // The actual production 30-second connection/discovery bound closes this session.
    // Wait for observable shutdown, not sleeps or a reduced test-only timeout.
    tokio::time::timeout(
        Duration::from_secs(45),
        trace.wait_for(|trace| !trace.deleted.is_empty()),
    )
    .await
    .unwrap()
    .unwrap();
    super::super::super::mcp_test_support::wait_for_status(&session, "slow", "failed").await;
    assert!(selected(&session));
    assert!(!effective(&session));
    session
        .current()
        .prompt("failed setup does not declare a pending tool")
        .await
        .unwrap();
    assert!(!last_request_declares(&fixture, SLOW_TOOL));
    assert!(selected(&session));
    manager.shutdown().await.unwrap();
    let mut ready = fixture.http.entry("/mcp");
    ready["exposure"] = json!("deferred");
    configure(&fixture, Some(ready));
    let manager = fixture.manager();
    let resumed = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    super::super::super::mcp_test_support::wait_for_tools(&resumed, &[SLOW_TOOL]).await;
    assert!(effective(&resumed));
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);
    assert!(fixture.http.snapshot().calls.is_empty());
    manager.shutdown().await.unwrap();
}
