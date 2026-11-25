use super::*;

#[tokio::test]
async fn pending_published_attempt_survives_failed_replacement_without_restarting() {
    let original = Fixture::new("server", true, 200).await;
    let changed = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![original.input(0)]);
    original.started().await;
    let published = generation(supervisor.clone());
    published.downgrade().publish();
    reconcile(&supervisor, vec![changed.input(0)]);
    let discarded = ready(&supervisor, "server").await;
    let failed = generation(supervisor.clone());
    drop(failed);
    closed(&discarded.connection).await;
    assert!(matches!(
        supervisor.snapshot().servers["server"],
        ConnectionReadiness::Connecting
    ));
    original.gate.add_permits(1);
    ready(&supervisor, "server").await;
    assert!(
        published.changed(),
        "real pending readiness must still be observable"
    );
    assert_eq!(original.starts.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn removed_slot_rolls_back_then_drains_after_success_without_self_staling() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    let old_inventory = ready(&supervisor, "server").await;
    let published = generation(supervisor.clone());
    let old_view = published.downgrade();
    old_view.publish();
    reconcile(&supervisor, vec![]);
    drop(generation(supervisor.clone()));
    assert!(Arc::ptr_eq(
        &old_inventory,
        &ready(&supervisor, "server").await
    ));
    reconcile(&supervisor, vec![]);
    let replacement = generation(supervisor.clone());
    old_view.shutdown(true).await.unwrap();
    replacement.downgrade().publish();
    closed(&old_inventory.connection).await;
    assert!(!replacement.changed());
    assert!(supervisor.snapshot().servers.is_empty());
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn older_candidate_abandon_cannot_roll_back_a_newer_stage() {
    let original = Fixture::new("server", false, 200).await;
    let older = Fixture::new("server", false, 200).await;
    let newer = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![original.input(0)]);
    let current = ready(&supervisor, "server").await;
    let published = generation(supervisor.clone());
    published.downgrade().publish();
    reconcile(&supervisor, vec![older.input(0)]);
    let old_inventory = ready(&supervisor, "server").await;
    let old_candidate = generation(supervisor.clone());
    reconcile(&supervisor, vec![newer.input(0)]);
    let new_inventory = ready(&supervisor, "server").await;
    let new_candidate = generation(supervisor.clone());
    drop(old_candidate);
    closed(&old_inventory.connection).await;
    assert!(Arc::ptr_eq(
        &new_inventory,
        &ready(&supervisor, "server").await
    ));
    assert!(!new_candidate.changed());
    assert!(!current.connection.is_closed());
    drop(new_candidate);
    closed(&new_inventory.connection).await;
    assert!(Arc::ptr_eq(&current, &ready(&supervisor, "server").await));
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn config_reversion_reuses_a_published_historical_slot_before_candidate_drop() {
    let original = Fixture::new("server", false, 200).await;
    let changed = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![original.input(0)]);
    let current = ready(&supervisor, "server").await;
    let published = generation(supervisor.clone());
    published.downgrade().publish();
    reconcile(&supervisor, vec![changed.input(0)]);
    let discarded = ready(&supervisor, "server").await;
    let failed = generation(supervisor.clone());
    reconcile(&supervisor, vec![original.input(0)]);
    let reverted = generation(supervisor.clone());
    assert!(Arc::ptr_eq(&current, &ready(&supervisor, "server").await));
    drop(failed);
    closed(&discarded.connection).await;
    assert!(!reverted.changed());
    published.downgrade().shutdown(true).await.unwrap();
    reverted.downgrade().publish();
    assert!(!current.connection.is_closed());
    assert_eq!(original.starts.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn lease_pins_exact_snapshot_not_the_later_current_map() {
    let original = Fixture::new("server", false, 200).await;
    let changed = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![original.input(0)]);
    let old = ready(&supervisor, "server").await;
    let snapshot = supervisor.snapshot();
    reconcile(&supervisor, vec![changed.input(0)]);
    let new = ready(&supervisor, "server").await;
    let source = McpGenerationReadiness::new(supervisor.clone(), &snapshot);
    drop(snapshot);
    assert!(!old.connection.is_closed());
    source.downgrade().publish();
    assert!(
        source.downgrade().active_supervisor().is_none(),
        "stale source cannot publish"
    );
    drop(source);
    closed(&old.connection).await;
    assert!(Arc::ptr_eq(&new, &ready(&supervisor, "server").await));
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn authoritative_actions_close_all_historical_pins_and_abandon_cannot_resurrect() {
    for action in ["invalidate", "reconnect", "close"] {
        let original = Fixture::new("server", false, 200).await;
        let changed = Fixture::new("server", false, 200).await;
        let supervisor = McpSupervisor::new();
        reconcile(&supervisor, vec![original.input(0)]);
        let old = ready(&supervisor, "server").await;
        let published = generation(supervisor.clone());
        published.downgrade().publish();
        reconcile(&supervisor, vec![changed.input(0)]);
        let staged = ready(&supervisor, "server").await;
        let candidate = generation(supervisor.clone());
        match action {
            "invalidate" => supervisor.invalidate("server"),
            "reconnect" => supervisor.reconnect("server").unwrap(),
            "close" => supervisor.close().await.unwrap(),
            _ => unreachable!(),
        }
        closed(&old.connection).await;
        closed(&staged.connection).await;
        drop(candidate);
        if action == "reconnect" {
            let reconnected = ready(&supervisor, "server").await;
            assert!(!Arc::ptr_eq(&staged, &reconnected));
            assert_eq!(changed.starts.load(Ordering::Acquire), 2);
        } else {
            assert!(supervisor.snapshot().servers.is_empty());
        }
        if action != "close" {
            reconcile(&supervisor, vec![original.input(0)]);
            let new = ready(&supervisor, "server").await;
            assert!(!Arc::ptr_eq(&old, &new));
            assert_eq!(original.starts.load(Ordering::Acquire), 2);
            supervisor.close().await.unwrap();
        }
    }
}
