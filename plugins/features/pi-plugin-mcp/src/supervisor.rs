//! Session-scoped transport ownership; generation policy and executable registries stay outside.
//!
//! Reconciliation observes authoritative configuration. Superseded attempts can never publish
//! into a newer slot, and dropping the final owner cancels work even if a retired view survives.
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::FutureExt;
use rmcp::model::Tool as RemoteTool;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::McpError;
use crate::client::McpConnection;

/// Opaque live-session owner. This contains no tools, plugin/command closures or generation policy.
#[derive(Clone, Default)]
pub struct McpSupervisor {
    inner: Arc<SupervisorInner>,
}

/// A product factory may cache this without extending a session or process lifetime.
#[derive(Clone)]
pub struct WeakMcpSupervisor(Weak<SupervisorInner>);

impl WeakMcpSupervisor {
    pub fn upgrade(&self) -> Option<McpSupervisor> {
        self.0.upgrade().map(|inner| McpSupervisor { inner })
    }
}

#[derive(Default)]
struct SupervisorInner {
    state: Mutex<SupervisorState>,
    next_candidate: AtomicU64,
    closed: AtomicBool,
    changed: Arc<ReadinessRevision>,
}

#[derive(Default)]
struct SupervisorState {
    config_epoch: u64,
    entries: BTreeMap<String, Arc<ConnectionSlot>>,
    // Neither historical inventory nor the last publication owns a session/source.
    known: Vec<Weak<ConnectionSlot>>,
    published: BTreeMap<String, Weak<ConnectionSlot>>,
}

#[derive(Default)]
struct ReadinessRevision {
    value: AtomicU64,
    notify: Notify,
}

impl ReadinessRevision {
    fn bump(&self) {
        self.value.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_waiters();
    }
}

mod generation;
mod refresh;
pub use generation::{McpGenerationReadiness, WeakMcpGenerationReadiness};
mod input;
pub(crate) use input::ConnectionInput;

/// Raw immutable protocol inventory, not executable registry entries.
pub(crate) struct ConnectionInventory {
    pub(crate) connection: Arc<McpConnection>,
    pub(crate) tools: Vec<RemoteTool>,
    pub(crate) refresh_error: Option<Arc<McpError>>,
}

#[derive(Clone)]
pub(crate) enum ConnectionReadiness {
    Connecting,
    Ready(Arc<ConnectionInventory>),
    Disconnected(Arc<ConnectionInventory>),
    Failed(Arc<McpError>),
    Retired,
}

pub(crate) struct ConnectionSnapshot {
    pub(crate) revision: u64,
    pub(crate) servers: BTreeMap<String, ConnectionReadiness>,
    candidate: u64,
    pins: Vec<Arc<ConnectionSlot>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SupervisorError {
    #[error("MCP session connections have been closed")]
    Closed,
    #[error("MCP configuration preparation was superseded; retry at the next idle boundary")]
    Superseded,
    #[error(transparent)]
    InvalidConfiguration(#[from] McpError),
}

struct ConnectionSlot {
    input: ConnectionInput,
    state: Mutex<AttemptState>,
    changed: Arc<ReadinessRevision>,
}

struct AttemptState {
    epoch: u64,
    retired: bool,
    readiness: ConnectionReadiness,
    task: Option<JoinHandle<()>>,
    refresh_task: Option<JoinHandle<()>>,
    refresh_seen: u64,
    abort: Option<pi_core::AbortHandle>,
}

impl ConnectionSlot {
    fn new(input: ConnectionInput, changed: Arc<ReadinessRevision>) -> Arc<Self> {
        Arc::new(Self {
            input,
            state: Mutex::new(AttemptState {
                epoch: 0,
                retired: false,
                readiness: ConnectionReadiness::Connecting,
                task: None,
                refresh_task: None,
                refresh_seen: 0,
                abort: None,
            }),
            changed,
        })
    }

    fn observe_closure(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ConnectionReadiness::Ready(inventory) = &state.readiness else {
            return;
        };
        if !inventory.connection.is_closed() {
            return;
        }
        state.readiness = ConnectionReadiness::Disconnected(inventory.clone());
        drop(state);
        self.changed.bump();
    }

    fn readiness(&self) -> ConnectionReadiness {
        self.observe_closure();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .readiness
            .clone()
    }

    fn start(self: &Arc<Self>, force: bool) {
        self.observe_closure();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired
            || (!force
                && state.epoch != 0
                && !matches!(state.readiness, ConnectionReadiness::Disconnected(_)))
        {
            return;
        }
        state.epoch += 1;
        let epoch = state.epoch;
        if let Some(abort) = state.abort.take() {
            abort.abort();
        }
        if let Some(task) = state.task.take() {
            task.abort();
        }
        if let Some(task) = state.refresh_task.take() {
            task.abort();
        }
        if let ConnectionReadiness::Ready(inventory)
        | ConnectionReadiness::Disconnected(inventory) = &state.readiness
        {
            inventory.connection.cancel();
        }
        state.readiness = ConnectionReadiness::Connecting;
        let input = self.input.clone();
        let (abort, signal) = pi_core::AbortHandle::new();
        state.abort = Some(abort);
        state.task = Some(self.spawn_attempt(epoch, input.connect(signal)));
        drop(state);
        self.changed.bump();
    }

    fn spawn_attempt(
        self: &Arc<Self>,
        epoch: u64,
        work: impl std::future::Future<Output = Result<(Arc<McpConnection>, Vec<RemoteTool>), McpError>>
        + Send
        + 'static,
    ) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        let name = self.input.name().to_owned();
        // The worker deliberately holds only a weak slot while doing I/O. Final owner drop
        // therefore aborts it rather than letting a pending attempt keep the session alive.
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(work)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(McpError::Initialize {
                        server: name,
                        message: "connection worker failed unexpectedly".into(),
                    })
                });
            if let Some(slot) = weak.upgrade() {
                slot.complete(epoch, result);
            }
        })
    }

    fn complete(
        self: &Arc<Self>,
        epoch: u64,
        result: Result<(Arc<McpConnection>, Vec<RemoteTool>), McpError>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired || state.epoch != epoch {
            if let Ok((connection, _)) = result {
                connection.cancel();
            }
            return;
        }
        state.task.take();
        state.abort.take();
        state.readiness = match result {
            Ok((connection, tools)) => {
                if let Some((revision, receiver)) = connection.tool_changes() {
                    state.refresh_seen = revision;
                    state.refresh_task = Some(self.spawn_refresh(epoch, revision, receiver));
                }
                ConnectionReadiness::Ready(Arc::new(ConnectionInventory {
                    connection,
                    tools,
                    refresh_error: None,
                }))
            }
            Err(error) => ConnectionReadiness::Failed(Arc::new(error)),
        };
        drop(state);
        self.changed.bump();
    }

    fn is_retired(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired
    }

    fn retire(&self) -> Option<Arc<McpConnection>> {
        let connection = self.cancel();
        self.changed.bump();
        connection
    }

    // Last-owner cleanup is not a readiness transition of the replacement's slots.
    // Otherwise draining old pins would immediately stale a successful publication.
    fn cancel(&self) -> Option<Arc<McpConnection>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.epoch += 1;
        state.retired = true;
        if let Some(abort) = state.abort.take() {
            abort.abort();
        }
        if let Some(task) = state.task.take() {
            task.abort();
        }
        if let Some(task) = state.refresh_task.take() {
            task.abort();
        }
        let old = std::mem::replace(&mut state.readiness, ConnectionReadiness::Retired);
        drop(state);
        if let ConnectionReadiness::Ready(inventory)
        | ConnectionReadiness::Disconnected(inventory) = old
        {
            inventory.connection.cancel();
            Some(inventory.connection.clone())
        } else {
            None
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Drop for SupervisorInner {
    fn drop(&mut self) {
        for slot in self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .known
            .iter()
            .filter_map(Weak::upgrade)
        {
            slot.retire();
        }
    }
}

fn same_entries(
    left: &BTreeMap<String, Arc<ConnectionSlot>>,
    right: &BTreeMap<String, Arc<ConnectionSlot>>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, slot)| {
            right
                .get(name)
                .is_some_and(|other| Arc::ptr_eq(slot, other))
        })
}

impl McpSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn downgrade(&self) -> WeakMcpSupervisor {
        WeakMcpSupervisor(Arc::downgrade(&self.inner))
    }

    pub fn revision(&self) -> u64 {
        // Freshness is observed at idle boundaries. Inspect protocol closure here without
        // keeping a polling task/session owner alive or attempting network I/O on a read.
        for slot in self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .known
            .iter()
            .filter_map(Weak::upgrade)
        {
            slot.observe_closure();
        }
        self.inner.changed.value.load(Ordering::Acquire)
    }

    /// Reserve before asynchronous configuration resolution, so an older candidate cannot
    /// overwrite configuration already observed by a newer one.
    pub(crate) fn begin_candidate(&self) -> u64 {
        self.inner.next_candidate.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn reconcile(
        &self,
        candidate: u64,
        inputs: Vec<ConnectionInput>,
    ) -> Result<(), SupervisorError> {
        // Global identity/collision failures are transactional: do not disturb healthy slots.
        crate::validate_configs(
            &inputs
                .iter()
                .map(ConnectionInput::identity)
                .collect::<Vec<_>>(),
        )?;
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SupervisorError::Closed);
        }
        if candidate < state.config_epoch {
            return Err(SupervisorError::Superseded);
        }
        state.config_epoch = candidate;
        state.known.retain(|slot| slot.strong_count() != 0);
        let mut entries = BTreeMap::new();
        for input in inputs {
            let name = input.name().to_owned();
            // A reverted configuration may reuse the published source's historical pin.
            let existing = state
                .entries
                .get(&name)
                .cloned()
                .filter(|slot| !slot.is_retired() && slot.input.equivalent(&input))
                .or_else(|| {
                    state
                        .known
                        .iter()
                        .filter_map(Weak::upgrade)
                        .find(|slot| !slot.is_retired() && slot.input.equivalent(&input))
                });
            let slot = existing.unwrap_or_else(|| {
                let slot = ConnectionSlot::new(input, self.inner.changed.clone());
                state.known.push(Arc::downgrade(&slot));
                slot
            });
            entries.insert(name, slot);
        }
        if !same_entries(&state.entries, &entries) {
            self.inner.changed.bump();
        }
        // Releasing map ownership must not cancel slots pinned by a published source.
        state.entries = entries;
        let entries = state.entries.values().cloned().collect::<Vec<_>>();
        drop(state);
        for slot in entries {
            slot.start(false);
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> ConnectionSnapshot {
        // Capture the revision first. Completion racing with this read may cause a harmless
        // extra idle refresh, but can never hide a ready transition behind a newer baseline.
        let revision = self.revision();
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ConnectionSnapshot {
            revision,
            candidate: state.config_epoch,
            pins: state.entries.values().cloned().collect(),
            servers: state
                .entries
                .iter()
                .map(|(name, slot)| (name.clone(), slot.readiness()))
                .collect(),
        }
    }

    /// Policy-free bounded wait; the generation owner selects which server names matter.
    pub(crate) async fn wait_for(&self, names: &[String], budget: Duration) -> ConnectionSnapshot {
        let names = names.iter().collect::<HashSet<_>>();
        let deadline = tokio::time::sleep(budget);
        tokio::pin!(deadline);
        loop {
            let changed = self.inner.changed.notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let snapshot = self.snapshot();
            if !snapshot.servers.iter().any(|(name, readiness)| {
                names.contains(name) && matches!(readiness, ConnectionReadiness::Connecting)
            }) {
                return snapshot;
            }
            tokio::select! {
                () = &mut changed => {},
                () = &mut deadline => return self.snapshot(),
            }
        }
    }

    /// Authoritative disable/remove/logout fences any previously reserved candidate as well
    /// as the current attempt. No new connection is opened by invalidation.
    pub(crate) fn invalidate(&self, name: &str) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        state.config_epoch = self.begin_candidate();
        state.published.remove(name);
        for slot in state
            .known
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|slot| slot.input.name() == name)
        {
            slot.retire();
        }
        state.entries.remove(name);
        self.inner.changed.bump();
    }

    /// A candidate may have waited while another authoritative reconciliation took over.
    /// Never combine that newer inventory with the waiting candidate's older policy map.
    pub(crate) fn ensure_current(&self, candidate: u64) -> Result<(), SupervisorError> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SupervisorError::Closed);
        }
        if state.config_epoch != candidate {
            return Err(SupervisorError::Superseded);
        }
        Ok(())
    }

    /// Manual reconnect invalidates the prior attempt and any backoff immediately.
    pub(crate) fn reconnect(&self, name: &str) -> Result<(), SupervisorError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SupervisorError::Closed);
        }
        let input = state.entries.get(name).map(|slot| slot.input.clone());
        state.config_epoch = self.begin_candidate();
        state.published.remove(name);
        for slot in state
            .known
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|slot| slot.input.name() == name)
        {
            slot.retire();
        }
        let input = input.ok_or(SupervisorError::Superseded)?;
        let slot = ConnectionSlot::new(input, self.inner.changed.clone());
        state.known.push(Arc::downgrade(&slot));
        state.entries.insert(name.into(), slot.clone());
        drop(state);
        slot.start(false);
        Ok(())
    }

    fn publish_candidate(&self, candidate: u64, pins: &[Arc<ConnectionSlot>]) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closed.load(Ordering::Acquire) || candidate != state.config_epoch {
            return false;
        }
        state.published = pins
            .iter()
            .filter(|slot| !slot.is_retired())
            .map(|slot| (slot.input.name().to_owned(), Arc::downgrade(slot)))
            .collect();
        true
    }

    fn abandon_candidate(&self, candidate: u64) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.closed.load(Ordering::Acquire) || candidate != state.config_epoch {
            return;
        }
        // An older source may never roll back a newer preparation or authoritative mutation.
        state.config_epoch = self.begin_candidate();
        let published = state
            .published
            .iter()
            .filter_map(|(name, weak)| {
                weak.upgrade()
                    .filter(|slot| !slot.is_retired())
                    .map(|slot| (name.clone(), slot))
            })
            .collect();
        if !same_entries(&state.entries, &published) {
            self.inner.changed.bump();
        }
        state.entries = published;
    }

    pub async fn close(&self) -> Result<(), McpError> {
        self.inner.closed.store(true, Ordering::Release);
        let slots = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let slots = state
                .known
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            state.entries.clear();
            state.published.clear();
            state.known.clear();
            slots
        };
        let connections = slots
            .iter()
            .filter_map(|slot| slot.retire())
            .collect::<Vec<_>>();
        let results =
            futures::future::join_all(connections.iter().map(|connection| connection.close()))
                .await;
        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests;
