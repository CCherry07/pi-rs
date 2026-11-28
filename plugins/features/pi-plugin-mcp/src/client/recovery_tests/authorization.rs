//! Terminal application auth is exact-transport state, not a name-based invalidation.
use super::*;
use crate::supervisor::{ConnectionReadiness, McpSupervisor};

fn required(owner: &McpSupervisor) -> McpAuthorizationRequired {
    let snapshot = owner.snapshot();
    let ConnectionReadiness::Failed(error) = &snapshot.servers["fixture"] else {
        panic!("terminal application auth must leave an actionable failed slot");
    };
    let McpError::AuthorizationRequired(required) = error.as_ref() else {
        panic!("expected typed authorization evidence: {error:?}");
    };
    required.clone()
}

#[tokio::test]
async fn terminal_auth_projects_guidance_and_fences_new_calls() {
    for kind in 0..4 {
        let fixture = Fixture::new(false).await;
        let fixture = match kind {
            0 | 1 => fixture.oauth(),
            2 => fixture.provider(Some("fake-provider-token")),
            _ => fixture.provider(None),
        };
        let (replies, attempts, refreshes) = match kind {
            0 => (vec![Reply::Auth, Reply::Auth], 2, 1),
            1 => (vec![Reply::Scope], 1, 0),
            _ => (vec![Reply::Auth], 1, 0),
        };
        let (owner, source, pool) = fixture.prepare().await;
        let frozen = pool.tools();
        fixture.replies(&replies);
        let error = invoke(&pool, "echo", context()).await.unwrap_err();
        let required = required(&owner);
        assert_eq!(required.server, "fixture");
        assert_eq!(
            required.provider.as_deref(),
            (kind >= 2).then_some("fixture-provider")
        );
        assert!(!required.static_authorization);
        let guidance = if kind >= 2 {
            "pi auth login fixture-provider"
        } else {
            "pi mcp login fixture"
        };
        assert!(error.to_string().contains(guidance), "{error}");
        assert!(required.guidance().contains(guidance));
        assert!(!format!("{error:?} {required:?}").contains("private-scope-marker"));
        if kind == 1 {
            assert!(
                required
                    .challenge
                    .as_deref()
                    .unwrap()
                    .contains("private-scope-marker")
            );
        }
        assert!(source.changed());
        assert_eq!(
            pool.tools(),
            frozen,
            "a fault cannot edit a published registry"
        );
        assert!(invoke(&pool, "echo", context()).await.is_err());
        assert_eq!(
            fixture.starts(),
            1,
            "sign-in does not eagerly open another session"
        );
        assert_eq!(
            fixture.calls(),
            attempts,
            "old views cannot dispatch another request"
        );
        assert_eq!(fixture.state.token_calls.load(Ordering::Acquire), refreshes);
        assert_eq!(fixture.executed(), 0);
        owner.close().await.unwrap();
    }
}

#[tokio::test]
async fn static_and_nonactionable_403_leave_the_slot_usable() {
    for kind in 0..5 {
        let fixture = Fixture::new(false).await;
        let fixture = match kind {
            2 => fixture.oauth(),
            3 | 4 => fixture.provider(Some("fake-provider-token")),
            _ => fixture,
        };
        let (owner, source, pool) = fixture.prepare().await;
        fixture.replies(&[match kind {
            0 => Reply::Auth,
            1 | 4 => Reply::Scope,
            _ => Reply::Status(403),
        }]);
        let error = invoke(&pool, "echo", context()).await.unwrap_err();
        assert!(!error.to_string().contains("login"), "{error}");
        assert!(matches!(
            owner.snapshot().servers["fixture"],
            ConnectionReadiness::Ready(_)
        ));
        assert!(!source.changed());
        assert!(invoke(&pool, "echo", context()).await.is_ok());
        assert_eq!(fixture.calls(), 2);
        assert_eq!(fixture.starts(), 1);
        assert_eq!(fixture.state.token_calls.load(Ordering::Acquire), 0);
        owner.close().await.unwrap();
    }
}

#[tokio::test]
async fn terminal_auth_drains_unrelated_admitted_json_and_sse_successes() {
    let fixture = Fixture::new(false).await.oauth();
    fixture.state.hold_json_body.store(true, Ordering::Release);
    let (owner, _source, pool) = fixture.prepare().await;
    let mut held = Vec::new();
    for name in ["held-json", "held-sse"] {
        let pool = pool.clone();
        held.push(tokio::spawn(
            async move { invoke(&pool, name, context()).await },
        ));
    }
    entered(&fixture.state.held_entered, 2).await;
    fixture.replies(&[Reply::Scope]);
    assert!(invoke(&pool, "echo", context()).await.is_err());
    required(&owner);
    assert!(held.iter().all(|task| !task.is_finished()));
    fixture.state.held_release.add_permits(2);
    for task in held {
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_ok(),
            "auth rejection must not cancel another admitted success: {result:?}"
        );
    }
    assert_eq!(fixture.calls(), 3, "neither admitted success is replayed");
    assert_eq!(fixture.starts(), 1);
    assert_eq!(fixture.state.token_calls.load(Ordering::Acquire), 0);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn late_old_transport_auth_does_not_poison_its_replacement() {
    let fixture = Fixture::new(false).await.oauth();
    let (owner, _source, pool) = fixture.prepare().await;
    fixture.replies(&[Reply::HeldScope]);
    let held_pool = pool.clone();
    let held = tokio::spawn(async move { invoke(&held_pool, "echo", context()).await });
    entered(&fixture.state.held_entered, 1).await;
    fixture.replies(&[Reply::Status(404)]);
    assert!(invoke(&pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    let (next_source, next_pool) = fixture.publish(&owner).await;
    fixture.state.held_release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error.to_string().contains("pi mcp login fixture"),
        "{error}"
    );
    assert!(
        !next_source.changed(),
        "a late result must not change a newer slot epoch"
    );
    assert!(matches!(
        owner.snapshot().servers["fixture"],
        ConnectionReadiness::Ready(_)
    ));
    assert!(invoke(&next_pool, "echo", context()).await.is_ok());
    assert_eq!(fixture.starts(), 2);
    assert_eq!(fixture.calls(), 4);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn resource_page_auth_discards_partial_results_and_marks_the_slot() {
    for tool in ["list_mcp_resources", "list_mcp_resource_templates"] {
        let fixture = Fixture::new(false).await.oauth();
        let (owner, _source, pool) = fixture.prepare().await;
        fixture.resources(&[Reply::Scope]);
        let result = resources(&pool, tool).await.unwrap();
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("pi mcp login fixture"), "{text}");
        assert!(
            !text.contains("round-1-first"),
            "partial aggregate leaked: {text}"
        );
        assert!(!text.contains("private-scope-marker"));
        required(&owner);
        assert_eq!(fixture.state.pages.lock().unwrap().len(), 2);
        assert_eq!(fixture.state.token_calls.load(Ordering::Acquire), 0);
        assert_eq!(fixture.starts(), 1);
        owner.close().await.unwrap();
    }
}
