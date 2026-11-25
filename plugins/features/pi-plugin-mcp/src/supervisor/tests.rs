use super::*;
mod pinning;
mod refresh;
mod transaction;
use crate::{McpClientAuth, McpServerConfig};
use axum::response::IntoResponse;
use serde_json::{Value, json};
use std::sync::atomic::AtomicUsize;
use tokio::sync::Semaphore;

struct Fixture {
    config: McpServerConfig,
    starts: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(name: &str, blocked: bool, status: u16) -> Self {
        let starts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Semaphore::new(usize::from(!blocked)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http(
            name,
            format!("http://{}/mcp", listener.local_addr().unwrap()),
        )
        .headers(BTreeMap::from([(
            "Authorization".into(),
            "Bearer supervisor-fixture".into(),
        )]));
        let app = axum::Router::new().route("/mcp", axum::routing::post({
            let starts = starts.clone();
            let gate = gate.clone();
            move |axum::Json(request): axum::Json<Value>| {
                let starts = starts.clone();
                let gate = gate.clone();
                async move {
                    let id = &request["id"];
                    if id.is_null() {
                        return http::StatusCode::ACCEPTED.into_response();
                    }
                    let result = match request["method"].as_str().unwrap() {
                        "server/discover" => return axum::Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"legacy fixture"}})).into_response(),
                        "initialize" => {
                            let attempt = starts.fetch_add(1, Ordering::AcqRel);
                            if attempt == 0 {
                                gate.acquire().await.unwrap().forget();
                            }
                            if status != 200 {
                                return http::StatusCode::from_u16(status).unwrap().into_response();
                            }
                            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"supervisor-fixture","version":"1"}})
                        }
                        "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                        "tools/call" => json!({"content":[{"type":"text","text":"still connected"}]}),
                        other => panic!("unexpected protocol request: {other}"),
                    };
                    axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
                }
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            config,
            starts,
            gate,
            task,
        }
    }

    fn input(&self, scope: u8) -> ConnectionInput {
        ConnectionInput::new(
            self.config.clone(),
            (&crate::policy::McpServerPolicy::direct()).into(),
            McpClientAuth::default(),
            [scope; 32],
        )
        .unwrap()
    }

    async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.starts.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

fn generation(supervisor: McpSupervisor) -> McpGenerationReadiness {
    let snapshot = supervisor.snapshot();
    McpGenerationReadiness::new(supervisor, &snapshot)
}

fn reconcile(supervisor: &McpSupervisor, inputs: Vec<ConnectionInput>) {
    supervisor
        .reconcile(supervisor.begin_candidate(), inputs)
        .unwrap();
}

async fn ready(supervisor: &McpSupervisor, name: &str) -> Arc<ConnectionInventory> {
    let snapshot = supervisor
        .wait_for(&[name.to_owned()], Duration::from_secs(3))
        .await;
    let Some(ConnectionReadiness::Ready(inventory)) = snapshot.servers.get(name) else {
        panic!("server {name} did not become ready");
    };
    inventory.clone()
}

async fn closed(connection: &McpConnection) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !connection.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn slow_connection_survives_cancelled_waiter_and_candidate_rebuild() {
    let fixture = Fixture::new("slow", true, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    fixture.started().await;
    let waiter = tokio::spawn({
        let supervisor = supervisor.clone();
        async move {
            supervisor
                .wait_for(&["slow".into()], Duration::from_secs(10))
                .await
        }
    });
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    for _ in 0..3 {
        reconcile(&supervisor, vec![fixture.input(0)]);
    }
    assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    let snapshot = supervisor
        .wait_for(&["slow".into()], Duration::from_millis(5))
        .await;
    assert!(matches!(
        snapshot.servers["slow"],
        ConnectionReadiness::Connecting
    ));
    fixture.gate.add_permits(1);
    let inventory = ready(&supervisor, "slow").await;
    assert_eq!(inventory.tools[0].name.as_ref(), "echo");
    assert!(supervisor.revision() > snapshot.revision);
    let next = supervisor.snapshot();
    let ConnectionReadiness::Ready(same) = &next.servers["slow"] else {
        panic!()
    };
    assert!(Arc::ptr_eq(&inventory, same));
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn independent_failure_keeps_healthy_inventory_and_wait_selection_is_bounded() {
    let healthy = Fixture::new("healthy", false, 200).await;
    let failed = Fixture::new("failed", false, 400).await;
    let slow = Fixture::new("slow", true, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(
        &supervisor,
        vec![healthy.input(0), failed.input(0), slow.input(0)],
    );
    let snapshot = supervisor
        .wait_for(&["healthy".into(), "failed".into()], Duration::from_secs(3))
        .await;
    assert!(matches!(
        snapshot.servers["healthy"],
        ConnectionReadiness::Ready(_)
    ));
    assert!(
        matches!(&snapshot.servers["failed"], ConnectionReadiness::Failed(error) if error.to_string().contains("failed"))
    );
    assert!(matches!(
        snapshot.servers["slow"],
        ConnectionReadiness::Connecting
    ));
    assert_eq!(failed.starts.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn changed_auth_or_trust_scope_replaces_transport_and_fences_old_views() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    let original = ready(&supervisor, "server").await;
    reconcile(&supervisor, vec![fixture.input(1)]);
    let replacement = ready(&supervisor, "server").await;
    assert!(!Arc::ptr_eq(&original.connection, &replacement.connection));
    closed(&original.connection).await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn remove_rejects_stale_candidate_and_no_waiter_keeps_worker_alive() {
    let fixture = Fixture::new("server", true, 200).await;
    let supervisor = McpSupervisor::new();
    let stale = supervisor.begin_candidate();
    supervisor.reconcile(stale, vec![fixture.input(0)]).unwrap();
    fixture.started().await;
    reconcile(&supervisor, Vec::new());
    assert!(matches!(
        supervisor.reconcile(stale, vec![fixture.input(0)]),
        Err(SupervisorError::Superseded)
    ));
    fixture.gate.add_permits(1);
    assert!(supervisor.snapshot().servers.is_empty());
    let weak = supervisor.downgrade();
    drop(supervisor);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn duplicate_namespace_failure_does_not_disturb_existing_valid_snapshot() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    let original = ready(&supervisor, "server").await;
    let revision = supervisor.revision();
    assert!(matches!(
        supervisor.reconcile(
            supervisor.begin_candidate(),
            vec![fixture.input(0), fixture.input(0)]
        ),
        Err(SupervisorError::InvalidConfiguration(_))
    ));
    assert_eq!(supervisor.revision(), revision);
    let same = ready(&supervisor, "server").await;
    assert!(Arc::ptr_eq(&original, &same));
    assert!(!original.connection.is_closed());
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn stale_attempt_completion_is_discarded_without_advancing_readiness() {
    let current = Fixture::new("server", false, 200).await;
    let stale = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![current.input(0)]);
    let inventory = ready(&supervisor, "server").await;
    let (unused, signal) = pi_core::AbortHandle::new();
    let (old_connection, tools) = stale.input(0).connect(signal).await.unwrap();
    drop(unused);
    let slot = supervisor.inner.state.lock().unwrap().entries["server"].clone();
    let revision = supervisor.revision();
    slot.complete(0, Ok((old_connection.clone(), tools)));
    assert_eq!(supervisor.revision(), revision);
    assert!(Arc::ptr_eq(&inventory, &ready(&supervisor, "server").await));
    closed(&old_connection).await;
    supervisor.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn local_command_values_run_in_attempts_not_input_creation_or_rebuild() {
    let fixture = Fixture::new("server", true, 200).await;
    let directory = tempfile::tempdir().unwrap();
    let crate::McpTransport::Http { url, .. } = &fixture.config.transport else {
        panic!()
    };
    let definition = crate::config::parse_document(&json!({"mcpServers":{"server":{
        "url":url,"headers":{"Authorization":"!printf call >> calls; printf 'Bearer supervisor-fixture'"}
    }}}).to_string()).unwrap().servers.remove(0).into_connection_definition().unwrap();
    let make_input = || {
        ConnectionInput::local(
            definition.clone(),
            crate::config::LocalValues {
                cwd: directory.path().to_owned(),
                allow_commands: true,
            },
            (&crate::policy::McpServerPolicy::direct()).into(),
            McpClientAuth::default(),
            [0; 32],
        )
        .unwrap()
    };
    let input = make_input();
    assert!(!directory.path().join("calls").exists());
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![input]);
    fixture.started().await;
    reconcile(&supervisor, vec![make_input()]);
    assert_eq!(
        std::fs::read(directory.path().join("calls")).unwrap(),
        b"call"
    );
    fixture.gate.add_permits(1);
    ready(&supervisor, "server").await;
    supervisor.reconnect("server").unwrap();
    ready(&supervisor, "server").await;
    assert_eq!(
        std::fs::read(directory.path().join("calls")).unwrap(),
        b"callcall"
    );
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn closed_transport_invalidates_readiness_and_same_key_reconciliation_reconnects() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    let original = ready(&supervisor, "server").await;
    let revision = supervisor.revision();
    original.connection.cancel();
    closed(&original.connection).await;
    let snapshot = supervisor.snapshot();
    assert!(
        !matches!(snapshot.servers["server"], ConnectionReadiness::Ready(_)),
        "a closed transport must not remain ready"
    );
    assert!(supervisor.revision() > revision);
    reconcile(&supervisor, vec![fixture.input(0)]);
    let replacement = ready(&supervisor, "server").await;
    assert!(!Arc::ptr_eq(&original.connection, &replacement.connection));
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn panicking_worker_settles_failure_without_leaking_payload_and_can_reconnect() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    let slot = ConnectionSlot::new(fixture.input(0), supervisor.inner.changed.clone());
    supervisor
        .inner
        .state
        .lock()
        .unwrap()
        .entries
        .insert("server".into(), slot.clone());
    supervisor
        .inner
        .state
        .lock()
        .unwrap()
        .known
        .push(Arc::downgrade(&slot));
    {
        let mut state = slot.state.lock().unwrap();
        state.epoch = 1;
        state.task = Some(slot.spawn_attempt(1, async { panic!("private-fixture-panic-payload") }));
    }
    let snapshot = supervisor
        .wait_for(&["server".into()], Duration::from_millis(100))
        .await;
    assert!(
        matches!(&snapshot.servers["server"], ConnectionReadiness::Failed(error) if !error.to_string().contains("private-fixture-panic-payload")),
        "a panicking worker must not remain connecting"
    );
    supervisor.reconnect("server").unwrap();
    ready(&supervisor, "server").await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn generation_leases_preserve_failed_candidates_and_release_retired_views() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    let inventory = ready(&supervisor, "server").await;
    let weak_supervisor = supervisor.downgrade();
    let current = generation(supervisor);
    let old_view = current.downgrade();
    assert!(old_view.active_supervisor().is_none());
    old_view.publish();
    assert!(!current.changed());

    let failed = generation(old_view.active_supervisor().unwrap());
    let failed_view = failed.downgrade();
    assert!(failed_view.active_supervisor().is_none());
    drop(failed);
    assert!(failed_view.is_retired());
    assert!(!old_view.is_retired());
    assert!(!inventory.connection.is_closed());

    let replacement = generation(old_view.active_supervisor().unwrap());
    let new_view = replacement.downgrade();
    old_view.shutdown(true).await.unwrap();
    assert!(old_view.is_retired());
    assert!(!inventory.connection.is_closed());
    new_view.publish();
    assert!(new_view.active_supervisor().is_some());
    drop(current);
    drop(replacement);
    assert!(new_view.is_retired());
    assert!(weak_supervisor.upgrade().is_none());
    closed(&inventory.connection).await;
}

#[tokio::test]
async fn conversation_shutdown_closes_old_owner_without_closing_new_conversation() {
    let fixture = Fixture::new("server", false, 200).await;
    let old = McpSupervisor::new();
    let new = McpSupervisor::new();
    reconcile(&old, vec![fixture.input(0)]);
    let old_inventory = ready(&old, "server").await;
    reconcile(&new, vec![fixture.input(0)]);
    let new_inventory = ready(&new, "server").await;
    let old_source = generation(old);
    let old_view = old_source.downgrade();
    old_view.publish();
    old_view.shutdown(false).await.unwrap();
    closed(&old_inventory.connection).await;
    assert!(old_view.is_retired());
    assert!(!new_inventory.connection.is_closed());
    assert!(!old_source.changed());
    old_view.shutdown(false).await.unwrap();
    new.close().await.unwrap();
}

#[tokio::test]
async fn authoritative_invalidation_fences_reserved_candidates_and_permits_fresh_retry() {
    let fixture = Fixture::new("server", true, 200).await;
    let supervisor = McpSupervisor::new();
    let current = supervisor.begin_candidate();
    supervisor
        .reconcile(current, vec![fixture.input(0)])
        .unwrap();
    fixture.started().await;
    let reserved = supervisor.begin_candidate();
    supervisor.invalidate("server");
    assert!(supervisor.snapshot().servers.is_empty());
    assert!(matches!(
        supervisor.ensure_current(current),
        Err(SupervisorError::Superseded)
    ));
    assert!(matches!(
        supervisor.reconcile(reserved, vec![fixture.input(0)]),
        Err(SupervisorError::Superseded)
    ));
    let fresh = supervisor.begin_candidate();
    supervisor.reconcile(fresh, vec![fixture.input(0)]).unwrap();
    let inventory = ready(&supervisor, "server").await;
    supervisor.ensure_current(fresh).unwrap();
    fixture.gate.add_permits(1);
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
    supervisor.invalidate("server");
    assert!(matches!(
        supervisor.ensure_current(fresh),
        Err(SupervisorError::Closed)
    ));
    closed(&inventory.connection).await;
}

#[tokio::test]
async fn final_owner_drop_stops_connection_even_when_old_inventory_is_retained() {
    let fixture = Fixture::new("server", false, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    let inventory = ready(&supervisor, "server").await;
    let weak = supervisor.downgrade();
    drop(supervisor);
    assert!(weak.upgrade().is_none());
    closed(&inventory.connection).await;
}

#[tokio::test]
async fn manual_reconnect_invalidates_pending_attempt_and_closed_owner_cannot_restart() {
    let fixture = Fixture::new("server", true, 200).await;
    let supervisor = McpSupervisor::new();
    reconcile(&supervisor, vec![fixture.input(0)]);
    fixture.started().await;
    let reserved = supervisor.begin_candidate();
    supervisor.reconnect("server").unwrap();
    assert!(matches!(
        supervisor.ensure_current(reserved),
        Err(SupervisorError::Superseded)
    ));
    let inventory = ready(&supervisor, "server").await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    fixture.gate.add_permits(1);
    supervisor.close().await.unwrap();
    closed(&inventory.connection).await;
    assert!(matches!(
        supervisor.reconnect("server"),
        Err(SupervisorError::Closed)
    ));
    assert!(matches!(
        supervisor.reconcile(supervisor.begin_candidate(), vec![fixture.input(0)]),
        Err(SupervisorError::Closed)
    ));
}
