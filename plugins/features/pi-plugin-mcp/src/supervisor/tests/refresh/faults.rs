use super::cases::{initialized, settled};
use super::*;

#[tokio::test]
async fn repeated_cursor_is_bounded_and_recovery_replaces_only_complete_inventory() {
    let fixture = ChangingFixture::new().await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    fixture.state.paginated.store(true, Ordering::Release);
    fixture.state.repeat_cursor.store(true, Ordering::Release);
    *fixture.state.tools.lock().unwrap() = vec![tool("a", "number"), tool("b", "number")];
    fixture.notify().await;
    fixture.state.gate.add_permits(2);
    settled(&supervisor, 1).await;
    let failed = ready(&supervisor, "server").await;
    assert_eq!(failed.tools, old.tools);
    assert!(
        failed
            .refresh_error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("pagination")
    );
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 3);
    fixture.state.repeat_cursor.store(false, Ordering::Release);
    fixture.notify().await;
    fixture.state.gate.add_permits(2);
    settled(&supervisor, 2).await;
    let recovered = ready(&supervisor, "server").await;
    assert_eq!(recovered.tools.len(), 2);
    assert!(recovered.refresh_error.is_none());
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn subscription_end_preserves_tools_and_reconnect_restores_subscription() {
    let fixture = ChangingFixture::with_protocol(true).await;
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    fixture.state.end_subscription.notify_one();
    settled(&supervisor, 1).await;
    let failed = ready(&supervisor, "server").await;
    assert_eq!(failed.tools, old.tools);
    assert!(
        failed
            .refresh_error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("subscription ended")
    );
    assert!(!failed.connection.is_closed());
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 1);
    assert!(source.changed());
    supervisor.reconnect("server").unwrap();
    fixture.state.gate.add_permits(1);
    let restored = ready(&supervisor, "server").await;
    assert!(restored.refresh_error.is_none());
    assert_eq!(fixture.state.subscriptions.load(Ordering::Acquire), 2);
    assert_eq!(fixture.state.discovers.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn unavailable_subscription_is_diagnostic_not_loss_of_discovered_tools() {
    let fixture = ChangingFixture::with_protocol(true).await;
    fixture
        .state
        .subscription_allowed
        .store(false, Ordering::Release);
    let supervisor = McpSupervisor::new();
    let old = initialized(&fixture, &supervisor).await;
    settled(&supervisor, 1).await;
    let failed = ready(&supervisor, "server").await;
    assert_eq!(failed.tools, old.tools);
    assert!(
        failed
            .refresh_error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("subscription unavailable")
    );
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 1);
    assert_eq!(fixture.state.discovers.load(Ordering::Acquire), 1);
    assert_eq!(fixture.state.subscriptions.load(Ordering::Acquire), 0);
    assert!(!failed.connection.is_closed());
    supervisor.close().await.unwrap();
}
