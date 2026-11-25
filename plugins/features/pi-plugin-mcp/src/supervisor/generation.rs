//! A generation's non-executable ownership lease. Only its freshness source owns it strongly.
use std::sync::{Arc, Mutex, Weak};

use super::{ConnectionSlot, ConnectionSnapshot, McpSupervisor};
use crate::McpError;

/// The host moves this opaque revision source into its existing generation-freshness object.
/// Executable plugins, commands, views and caches retain only the corresponding weak handle.
pub struct McpGenerationReadiness {
    lease: Arc<GenerationLease>,
    baseline: u64,
}

/// Non-owning access for staged publication, shutdown and the product's live-runtime cache.
#[derive(Clone)]
pub struct WeakMcpGenerationReadiness(Weak<GenerationLease>);

struct GenerationLease {
    state: Mutex<LeaseState>,
}

struct LeaseState {
    supervisor: Option<McpSupervisor>,
    candidate: u64,
    pins: Vec<Arc<ConnectionSlot>>,
    published: bool,
}

impl McpGenerationReadiness {
    pub(crate) fn new(supervisor: McpSupervisor, snapshot: &ConnectionSnapshot) -> Self {
        Self {
            lease: Arc::new(GenerationLease {
                state: Mutex::new(LeaseState {
                    supervisor: Some(supervisor),
                    candidate: snapshot.candidate,
                    pins: snapshot.pins.clone(),
                    published: false,
                }),
            }),
            baseline: snapshot.revision,
        }
    }

    pub fn downgrade(&self) -> WeakMcpGenerationReadiness {
        WeakMcpGenerationReadiness(Arc::downgrade(&self.lease))
    }

    pub fn changed(&self) -> bool {
        self.lease
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .supervisor
            .as_ref()
            .is_some_and(|owner| owner.revision() != self.baseline)
    }
}

impl Drop for GenerationLease {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.published
            && let Some(supervisor) = &state.supervisor
        {
            supervisor.abandon_candidate(state.candidate);
        }
        // Executable inventory references do not pin raw slots and cannot defeat cleanup.
        state.pins.clear();
    }
}

impl WeakMcpGenerationReadiness {
    /// Never reuse an uncommitted candidate. Preparation failure must leave the active owner
    /// selectable even while another candidate or its executable snapshot is retained.
    pub fn active_supervisor(&self) -> Option<McpSupervisor> {
        let lease = self.0.upgrade()?;
        let state = lease
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.published.then(|| state.supervisor.clone()).flatten()
    }

    pub fn is_retired(&self) -> bool {
        self.0.upgrade().is_none_or(|lease| {
            lease
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .supervisor
                .is_none()
        })
    }

    /// The existing session-start hook runs after all fallible preparation and after the old
    /// shutdown hook. A hook error is diagnostic, not a rollback to the old generation.
    pub(crate) fn publish(&self) {
        if let Some(lease) = self.0.upgrade() {
            let mut state = lease
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.published {
                state.published = state
                    .supervisor
                    .as_ref()
                    .is_some_and(|owner| owner.publish_candidate(state.candidate, &state.pins));
            }
        }
    }

    pub(crate) async fn shutdown(&self, reloading: bool) -> Result<(), McpError> {
        let retired = self.0.upgrade().and_then(|lease| {
            let mut state = lease
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let was_published = std::mem::replace(&mut state.published, false);
            let pins = std::mem::take(&mut state.pins);
            state
                .supervisor
                .take()
                .map(|owner| (owner, state.candidate, was_published, pins))
        });
        // Reload's already-prepared candidate holds another lease on this same owner.
        // New/resume/fork/shutdown retire it, even while the old freshness source survives.
        if let Some((supervisor, candidate, published, pins)) = retired {
            if reloading && !published {
                supervisor.abandon_candidate(candidate);
            }
            drop(pins);
            if !reloading {
                supervisor.close().await?;
            }
        }
        Ok(())
    }
}
