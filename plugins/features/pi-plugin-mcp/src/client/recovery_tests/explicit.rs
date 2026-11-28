//! Explicit pools share the existing exact-slot owner, not an in-place SDK session.
use super::*;

#[tokio::test]
async fn explicit_expiry_replaces_the_sdk_connection_and_keeps_the_frozen_view() {
    let fixture = Fixture::new(false).await;
    let pool = fixture.explicit().await;
    let frozen = pool.tools();
    let old = pool.inner.servers[0].connection.clone();
    fixture.replies(&[Reply::Status(404)]);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    let slot = pool.inner.servers[0]
        .binding
        .as_ref()
        .unwrap()
        .owned()
        .unwrap()
        .downgrade();
    let active = slot.acquire().await.unwrap();
    assert!(
        !Arc::ptr_eq(&old, &active.inventory.connection),
        "fresh SDK owner, not a changed session on the old peer"
    );
    assert_eq!(pool.tools(), frozen);
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    assert_eq!(fixture.executed(), 1);
    old.close().await.unwrap();
    assert!(
        invoke(&pool, "echo", context()).await.is_ok(),
        "old service shutdown cannot close its replacement"
    );
    assert_eq!(fixture.starts(), 2);
    assert!(pool.connection_error("fixture").is_none());
    active.inventory.connection.close().await.unwrap();
    let error = invoke(&pool, "echo", context()).await.unwrap_err();
    assert!(error.to_string().contains("Connection closed"), "{error}");
    assert_eq!(
        fixture.starts(),
        2,
        "a plain closed replacement is not expiry"
    );
    pool.shutdown().await.unwrap();
    assert!(invoke(&pool, "echo", context()).await.is_err());
    assert_eq!(fixture.calls(), 3);
}

#[tokio::test]
async fn explicit_replacement_preserves_admitted_json_and_sse_successes() {
    let fixture = Fixture::new(false).await;
    let pool = fixture.explicit().await;
    let json_pool = pool.clone();
    let json = tokio::spawn(async move { invoke(&json_pool, "held-json", context()).await });
    let sse_pool = pool.clone();
    let sse = tokio::spawn(async move { invoke(&sse_pool, "held-sse", context()).await });
    entered(&fixture.state.held_entered, 2).await;
    fixture.replies(&[Reply::Status(404)]);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    assert!(
        !json.is_finished() && !sse.is_finished(),
        "old admitted work must remain alive"
    );
    fixture.state.held_release.add_permits(2);
    for call in [json, sse] {
        assert!(
            tokio::time::timeout(Duration::from_secs(5), call)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
    }
    assert_eq!(fixture.calls(), 4);
    assert_eq!(fixture.executed(), 3);
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_shutdown_retires_and_joins_current_and_draining_services() {
    let fixture = Fixture::new(false).await;
    let pool = fixture.explicit().await;
    let old_pool = pool.clone();
    let old = tokio::spawn(async move { invoke(&old_pool, "held-json", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    fixture.replies(&[Reply::Status(404)]);
    invoke(&pool, "echo", context()).await.unwrap();
    let current_pool = pool.clone();
    let current = tokio::spawn(async move { invoke(&current_pool, "held-sse", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    assert!(!old.is_finished() && !current.is_finished());
    tokio::time::timeout(Duration::from_secs(2), async {
        pool.shutdown().await.unwrap();
        let mut deleted = fixture.state.deleted.lock().unwrap().clone();
        deleted.sort();
        assert_eq!(
            deleted,
            ["session-1", "session-2"],
            "shutdown joined both acquired services"
        );
        for request in [old, current] {
            assert!(
                request.await.unwrap().is_err(),
                "authoritative shutdown fences admitted work"
            );
        }
        pool.shutdown().await.unwrap();
    })
    .await
    .expect("bounded explicit shutdown");
    assert!(invoke(&pool, "echo", context()).await.is_err());
    assert_eq!(fixture.state.deleted.lock().unwrap().len(), 2);
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 4);
    assert_eq!(fixture.executed(), 1);
}

#[tokio::test]
async fn explicit_cancelled_waiter_does_not_cancel_shared_fresh_setup() {
    let fixture = Fixture::new(false).await;
    let pool = fixture.explicit().await;
    fixture.state.hold_init.store(true, Ordering::Release);
    fixture.replies(&[Reply::Status(404)]);
    let (abort, signal) = pi_core::AbortHandle::new();
    let first_pool = pool.clone();
    let first = tokio::spawn(async move {
        invoke(
            &first_pool,
            "echo",
            ToolContext::standalone(PathBuf::from("/tmp"), signal),
        )
        .await
    });
    entered(&fixture.state.init_entered, 1).await;
    let second_pool = pool.clone();
    let second = tokio::spawn(async move { invoke(&second_pool, "echo", context()).await });
    abort.abort();
    assert!(matches!(first.await.unwrap(), Err(ToolError::Aborted)));
    fixture.state.init_release.add_permits(1);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 2);
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_oauth_and_expiry_share_one_application_allowance_in_both_orders() {
    for auth_first in [true, false] {
        let fixture = Fixture::new(false).await.oauth();
        fixture.replies(&if auth_first {
            [Reply::Auth, Reply::Status(404)]
        } else {
            [Reply::Status(404), Reply::Auth]
        });
        let pool = fixture.explicit().await;
        assert!(invoke(&pool, "echo", context()).await.is_err());
        assert_eq!(fixture.calls(), 2, "no nested third application POST");
        assert_eq!(fixture.executed(), 0);
        assert_eq!(fixture.starts(), if auth_first { 1 } else { 2 });
        assert_eq!(
            fixture.state.token_calls.load(Ordering::Acquire),
            usize::from(auth_first)
        );
        pool.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn explicit_replacement_cannot_expand_a_frozen_callable_grant() {
    let fixture = Fixture::new(false).await;
    let pool = fixture.explicit().await;
    fixture.replies(&[Reply::Status(404)]);
    fixture.state.tools.lock().unwrap()[0].description = Some("new declaration".into());
    let error = invoke(&pool, "echo", context()).await.unwrap_err();
    assert!(error.to_string().contains("inventory changed"), "{error}");
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.executed(), 0);
    pool.shutdown().await.unwrap();
}
