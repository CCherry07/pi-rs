//! Saved selection is intent, not permission to execute a pending MCP tool.
use super::*;
use pi_session::{ActiveToolsEntry, OpenSessionOptions, SessionEntry};

mod identities;

fn configure(fixture: &ProductFixture, entry: Option<Value>) {
    let servers = entry.map_or_else(|| json!({}), |entry| json!({"slow":entry}));
    std::fs::write(
        fixture.agent.join("mcp.json"),
        json!({"mcpServers":servers}).to_string(),
    )
    .unwrap();
}

fn pending_entry(fixture: &ProductFixture) -> Value {
    let mut entry = fixture.http.entry("/slow");
    entry["exposure"] = json!("deferred");
    entry
}

fn saved(fixture: &ProductFixture, file: &str) -> PathBuf {
    let path = fixture.agent.join(file);
    let store = SessionStore::create(&path, SessionHeader::new(file, &fixture.project)).unwrap();
    store
        .append(SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: vec![SLOW_TOOL.into()],
        }))
        .unwrap();
    path
}

fn selected(session: &PiSession) -> bool {
    session
        .current()
        .desired_active_tools()
        .unwrap()
        .iter()
        .any(|name| name == SLOW_TOOL)
}

fn effective(session: &PiSession) -> bool {
    session
        .current()
        .snapshot()
        .agent
        .active_tools
        .iter()
        .any(|name| name == SLOW_TOOL)
}

async fn release_and_publish(fixture: &ProductFixture, session: &PiSession) {
    fixture.http.slow_gate.add_permits(1);
    super::super::mcp_test_support::wait_for_tools(session, &[SLOW_TOOL]).await;
}

#[tokio::test]
async fn pending_resume_keeps_intent_through_passive_prompt_quit_and_reopen() {
    let fixture = ProductFixture::new(false).await;
    configure(&fixture, Some(pending_entry(&fixture)));
    let path = saved(&fixture, "pending-selection.jsonl");
    let manager = fixture.manager();
    let session = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    fixture.http.until(|trace| trace.slow_listing).await;
    assert!(selected(&session));
    assert!(!effective(&session));
    assert!(!has_slow_tool(&session.current()));
    session
        .current()
        .prompt("continue without the pending tool")
        .await
        .unwrap();
    assert!(
        selected(&session),
        "passive assistant capture must preserve intent"
    );
    assert!(
        fixture
            .http
            .snapshot()
            .provider_tools
            .iter()
            .all(|tools| !tools.iter().any(|name| name == SLOW_TOOL))
    );
    assert!(fixture.http.snapshot().calls.is_empty());
    manager.shutdown().await.unwrap();
    fixture.http.deleted(&["connection-1"]).await;
    // The second owner must recover from the journal, not an in-memory factory cache.
    let manager = fixture.manager();
    let resumed = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    assert!(selected(&resumed));
    assert!(!effective(&resumed));
    // Release all gated requests, including any already-cancelled HTTP handler.
    fixture.http.slow_gate.add_permits(2);
    super::super::mcp_test_support::wait_for_tools(&resumed, &[SLOW_TOOL]).await;
    assert!(selected(&resumed));
    assert!(effective(&resumed));
    resumed
        .current()
        .prompt("ready tool may now be declared")
        .await
        .unwrap();
    assert!(
        fixture
            .http
            .snapshot()
            .provider_tools
            .last()
            .unwrap()
            .iter()
            .any(|name| name == SLOW_TOOL)
    );
    assert_eq!(fixture.http.snapshot().initialized.len(), 2);
    assert!(fixture.http.snapshot().calls.is_empty());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_deselect_while_pending_survives_ready_publication() {
    let fixture = ProductFixture::new(false).await;
    configure(&fixture, Some(pending_entry(&fixture)));
    let path = saved(&fixture, "deselected.jsonl");
    let manager = fixture.manager();
    let session = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    fixture.http.until(|trace| trace.slow_listing).await;
    assert!(selected(&session));
    session
        .current()
        .set_active_tools(Vec::<String>::new())
        .unwrap();
    assert!(!selected(&session));
    release_and_publish(&fixture, &session).await;
    assert!(!selected(&session));
    assert!(!effective(&session));
    session
        .current()
        .prompt("do not restore a deselected tool")
        .await
        .unwrap();
    assert!(
        fixture
            .http
            .snapshot()
            .provider_tools
            .iter()
            .all(|tools| !tools.iter().any(|name| name == SLOW_TOOL))
    );
    assert!(fixture.http.snapshot().calls.is_empty());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn historical_branch_intent_is_available_without_selecting_it_on_current_branch() {
    let fixture = ProductFixture::new(false).await;
    configure(&fixture, Some(pending_entry(&fixture)));
    let path = saved(&fixture, "branch-selection.jsonl");
    let store = SessionStore::open_handle(&path).unwrap();
    let selected_entry = store
        .append(SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: vec![SLOW_TOOL.into()],
        }))
        .unwrap();
    store
        .append(SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: Vec::new(),
        }))
        .unwrap();
    let manager = fixture.manager();
    let session = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    fixture.http.until(|trace| trace.slow_listing).await;
    assert!(
        !selected(&session),
        "history candidates must not activate current intent"
    );
    session
        .current()
        .checkout(Some(&selected_entry))
        .await
        .unwrap();
    assert!(selected(&session));
    assert!(!effective(&session));
    release_and_publish(&fixture, &session).await;
    assert!(effective(&session));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn removed_disabled_hidden_and_revoked_pending_selections_are_not_restored() {
    for scenario in ["removed", "disabled", "hidden", "revoked"] {
        let fixture = ProductFixture::new(false).await;
        let mut entry = pending_entry(&fixture);
        if scenario == "disabled" {
            entry["enabled"] = json!(false);
        }
        if scenario == "hidden" {
            entry["exposure"] = json!("hidden");
        }
        configure(&fixture, (scenario != "removed").then_some(entry));
        let path = saved(&fixture, &format!("{scenario}.jsonl"));
        let manager = fixture.manager();
        let options = if scenario == "revoked" {
            OpenSessionOptions {
                generation_overlay: SessionGenerationOverlay::new()
                    .with_allowed_tools(vec!["tool_search".into()]),
            }
        } else {
            OpenSessionOptions::default()
        };
        let session = manager.open_session(&path, options).await.unwrap();
        assert!(!selected(&session), "{scenario} must prune intent");
        assert!(!effective(&session));
        if scenario == "revoked" {
            fixture.http.slow_gate.add_permits(1);
            super::super::mcp_test_support::wait_for_status(&session, "slow", "connected").await;
            assert!(!has_slow_tool(&session.current()));
        }
        session
            .current()
            .prompt("no denied tool declarations")
            .await
            .unwrap();
        assert!(
            fixture
                .http
                .snapshot()
                .provider_tools
                .iter()
                .all(|tools| !tools.iter().any(|name| name == SLOW_TOOL))
        );
        assert!(fixture.http.snapshot().calls.is_empty());
        manager.shutdown().await.unwrap();
        // Restoring the configuration/grant must not resurrect the pruned current intent.
        let mut restored_entry = fixture.http.entry("/mcp");
        restored_entry["exposure"] = json!("deferred");
        configure(&fixture, Some(restored_entry));
        let manager = fixture.manager();
        let restored = manager
            .open_session(&path, Default::default())
            .await
            .unwrap();
        super::super::mcp_test_support::wait_for_tools(&restored, &[SLOW_TOOL]).await;
        assert!(
            !selected(&restored),
            "{scenario} restoration resurrected intent"
        );
        assert!(!effective(&restored));
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn failed_candidate_does_not_commit_pending_intent_pruning() {
    let fixture = ProductFixture::new(false).await;
    configure(&fixture, Some(pending_entry(&fixture)));
    let path = saved(&fixture, "selection-rollback.jsonl");
    let fail = Arc::new(AtomicBool::new(false));
    let manager = fixture.manager();
    let session = manager
        .open_session(
            &path,
            OpenSessionOptions {
                generation_overlay: SessionGenerationOverlay::new().with_plugin({
                    let fail = fail.clone();
                    move || Arc::new(DuplicateCandidate(fail.load(Ordering::Acquire)))
                }),
            },
        )
        .await
        .unwrap();
    fixture.http.until(|trace| trace.slow_listing).await;
    let current = session.current();
    let before = std::fs::read(&path).unwrap();
    configure(&fixture, None);
    fail.store(true, Ordering::Release);
    let error = session.reload().await.unwrap_err().to_string();
    assert!(error.contains("duplicate"), "{error}");
    assert!(Arc::ptr_eq(&current, &session.current()));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(selected(&session));
    configure(&fixture, Some(pending_entry(&fixture)));
    fail.store(false, Ordering::Release);
    release_and_publish(&fixture, &session).await;
    assert!(effective(&session));
    assert_eq!(fixture.http.snapshot().initialized.len(), 1);
    manager.shutdown().await.unwrap();
}
