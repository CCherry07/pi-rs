use super::cases::{initialized, received, settled};
use super::*;

async fn subscription_closed(fixture: &ChangingFixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.state.active_subscriptions.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("subscription survived final owner");
}

#[tokio::test]
async fn modern_subscription_precedes_initial_listing_and_is_owned_by_transport() {
    let fixture = ChangingFixture::with_protocol(true).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input()]);
    fixture.listed(1).await;
    assert_eq!(fixture.state.subscriptions.load(Ordering::Acquire), 1);
    *fixture.state.tools.lock().unwrap() = vec![tool("modern_updated", "number")];
    fixture.notify().await;
    fixture.state.gate.add_permits(1);
    fixture.listed(2).await;
    fixture.state.gate.add_permits(1);
    settled(&supervisor, 1).await;
    let inventory = ready(&supervisor, "server").await;
    assert_eq!(inventory.tools[0].name, "modern_updated");
    assert_eq!(fixture.state.discovers.load(Ordering::Acquire), 1);
    assert_eq!(fixture.state.initializes.load(Ordering::Acquire), 0);
    let source = generation(supervisor.clone());
    source.downgrade().publish();
    let weak = supervisor.downgrade();
    drop(supervisor);
    drop(source);
    assert!(weak.upgrade().is_none());
    closed(&inventory.connection).await;
    subscription_closed(&fixture).await;
}

#[tokio::test]
async fn modern_burst_and_pending_refresh_cancel_keep_one_subscription() {
    let fixture = ChangingFixture::with_protocol(true).await;
    let supervisor = McpSupervisor::new();
    let original = initialized(&fixture, &supervisor).await;
    fixture.notify().await;
    fixture.listed(2).await;
    for _ in 0..8 {
        fixture.notify().await;
    }
    received(&original.connection, 9).await;
    assert_eq!(fixture.state.lists.load(Ordering::Acquire), 2);
    fixture.state.gate.add_permits(1);
    fixture.listed(3).await;
    assert_eq!(fixture.state.subscriptions.load(Ordering::Acquire), 1);
    supervisor.invalidate("server");
    closed(&original.connection).await;
    subscription_closed(&fixture).await;
    supervisor.close().await.unwrap();
}
