use super::*;

#[tokio::test]
async fn oauth_then_expiry_and_expiry_then_oauth_share_one_application_allowance() {
    for auth_first in [true, false] {
        let fixture = Fixture::new(false).await.oauth();
        fixture.replies(&if auth_first {
            [Reply::Auth, Reply::Status(404)]
        } else {
            [Reply::Status(404), Reply::Auth]
        });
        let (owner, _source, pool) = fixture.prepare().await;
        assert!(invoke(&pool, "echo", context()).await.is_err());
        assert_eq!(fixture.calls(), 2, "no nested third application POST");
        assert_eq!(fixture.executed(), 0);
        assert_eq!(
            fixture.starts(),
            if auth_first { 1 } else { 2 },
            "exhausted expiry never eagerly opens"
        );
        assert_eq!(
            fixture.state.token_calls.load(Ordering::Acquire),
            usize::from(auth_first)
        );
        owner.close().await.unwrap();
    }
}

#[tokio::test]
async fn late_page_auth_and_transient_share_aggregate_budget_in_both_orders() {
    for auth_first in [true, false] {
        let fixture = Fixture::new(false).await.oauth();
        fixture.resources(&if auth_first {
            [Reply::Auth, Reply::Status(503)]
        } else {
            [Reply::Status(503), Reply::Auth]
        });
        let (owner, _source, pool) = fixture.prepare().await;
        let result = resources(&pool, "list_mcp_resources").await.unwrap();
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("errors"), "{text}");
        assert!(
            !text.contains("round-1-first") && !text.contains("round-2-first"),
            "partial aggregate leaked: {text}"
        );
        assert_eq!(
            fixture.state.pages.lock().unwrap().len(),
            if auth_first { 3 } else { 4 }
        );
        assert_eq!(
            fixture.state.rounds.load(Ordering::Acquire),
            if auth_first { 1 } else { 2 }
        );
        assert_eq!(
            fixture.state.token_calls.load(Ordering::Acquire),
            usize::from(auth_first)
        );
        assert_eq!(fixture.starts(), 1);
        owner.close().await.unwrap();
    }
}

#[tokio::test]
async fn late_page_auth_and_expiry_share_aggregate_budget_in_both_orders() {
    for auth_first in [true, false] {
        let fixture = Fixture::new(false).await.oauth();
        fixture.resources(&if auth_first {
            [Reply::Auth, Reply::Status(404)]
        } else {
            [Reply::Status(404), Reply::Auth]
        });
        let (owner, _source, pool) = fixture.prepare().await;
        let result = resources(&pool, "list_mcp_resource_templates")
            .await
            .unwrap();
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("errors"));
        assert!(!text.contains("round-1-first") && !text.contains("round-2-first"));
        assert_eq!(
            fixture.state.pages.lock().unwrap().len(),
            if auth_first { 3 } else { 4 }
        );
        assert_eq!(
            fixture.state.token_calls.load(Ordering::Acquire),
            usize::from(auth_first)
        );
        assert_eq!(fixture.starts(), if auth_first { 1 } else { 2 });
        owner.close().await.unwrap();
    }
}

#[tokio::test]
async fn oauth_expiry_preserves_an_admitted_json_body_without_replay() {
    let fixture = Fixture::new(false).await.oauth();
    fixture.state.hold_json_body.store(true, Ordering::Release);
    let (owner, _source, pool) = fixture.prepare().await;
    let held_pool = pool.clone();
    let held = tokio::spawn(async move { invoke(&held_pool, "held-json", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    assert_eq!(
        fixture.state.authorization.lock().unwrap()[0].as_deref(),
        Some("Bearer initial")
    );
    fixture.replies(&[Reply::Status(404)]);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    assert!(
        !held.is_finished(),
        "ordinary expiry must drain the admitted OAuth response"
    );
    fixture.state.held_release.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "completed old-epoch JSON must survive supersession: {result:?}"
    );
    assert_eq!(fixture.calls(), 3, "held call is never replayed");
    assert_eq!(fixture.executed(), 2);
    assert_eq!(fixture.state.token_calls.load(Ordering::Acquire), 0);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn oauth_authoritative_reconnect_still_cancels_an_admitted_json_body() {
    let fixture = Fixture::new(false).await.oauth();
    fixture.state.hold_json_body.store(true, Ordering::Release);
    let (owner, _source, pool) = fixture.prepare().await;
    let held_pool = pool.clone();
    let held = tokio::spawn(async move { invoke(&held_pool, "held-json", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    assert_eq!(
        fixture.state.authorization.lock().unwrap()[0].as_deref(),
        Some("Bearer initial")
    );
    owner.reconnect("fixture").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_err(),
        "authoritative retirement must not use completion allowance"
    );
    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.state.token_calls.load(Ordering::Acquire), 0);
    fixture.state.held_release.add_permits(1);
    let (_next_source, next_pool) = fixture.publish(&owner).await;
    assert!(invoke(&next_pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn correlated_rpc_error_on_503_is_a_typed_supervised_resource_retry() {
    // The private backend checks HTTP status before RPC bodies in every mode.
    // Only the supervised resource operation owns this bounded application retry.
    let fixture = Fixture::new(false).await;
    fixture.resources(&[Reply::StatusRpc(503)]);
    let (owner, _source, pool) = fixture.prepare().await;
    let result = resources(&pool, "read_mcp_resource").await.unwrap();
    assert!(
        serde_json::to_string(&result.content)
            .unwrap()
            .contains("resource")
    );
    assert_eq!(fixture.state.pages.lock().unwrap().len(), 2);
    assert_eq!(fixture.starts(), 1);
    owner.close().await.unwrap();
}
