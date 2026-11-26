//! Weak exact-slot routing and lazy transport replacement. No executable policy lives here.
use super::*;
use crate::client::ConnectionUse;
use crate::client::operation::OperationError;

#[derive(Clone)]
pub(crate) struct WeakConnectionSlot(pub(super) Weak<ConnectionSlot>);

pub(crate) struct ConnectionAdmission {
    pub(crate) inventory: Arc<ConnectionInventory>,
    pub(crate) _usage: ConnectionUse,
}

impl WeakConnectionSlot {
    /// The waiter never holds a slot while suspended. Cancelling one waiter cannot cancel the
    /// shared opening; dropping the actual generation/owner still retires every attempt.
    pub(crate) async fn acquire(&self) -> Result<ConnectionAdmission, OperationError> {
        loop {
            let slot = self.0.upgrade().ok_or(OperationError::AuthorityRetired)?;
            let revision = slot.changed.clone();
            let changed = revision.notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                slot.observe_closure();
                let mut state = slot
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &state.readiness {
                    ConnectionReadiness::Ready(inventory) => {
                        let usage = inventory.connection.admit()?;
                        return Ok(ConnectionAdmission {
                            inventory: inventory.clone(),
                            _usage: usage,
                        });
                    }
                    ConnectionReadiness::Disconnected(_) => slot.open_lazy(&mut state),
                    ConnectionReadiness::Connecting => {}
                    ConnectionReadiness::Retired => return Err(OperationError::AuthorityRetired),
                    ConnectionReadiness::Failed(_) => {
                        return Err(OperationError::ConnectionUnavailable);
                    }
                }
            }
            drop(slot);
            changed.await;
        }
    }

    /// A late expiry of a detached transport must join, never replace, a newer connection.
    pub(crate) fn expire(&self, expected: &Arc<McpConnection>) -> Result<(), OperationError> {
        let slot = self.0.upgrade().ok_or(OperationError::AuthorityRetired)?;
        let mut state = slot
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(OperationError::AuthorityRetired);
        }
        let inventory = match &state.readiness {
            ConnectionReadiness::Ready(inventory)
            | ConnectionReadiness::Disconnected(inventory)
                if Arc::ptr_eq(&inventory.connection, expected) =>
            {
                inventory.clone()
            }
            _ => return Ok(()),
        };
        if matches!(state.readiness, ConnectionReadiness::Disconnected(_)) {
            return Ok(());
        }
        state.epoch += 1;
        if let Some(task) = state.refresh_task.take() {
            task.abort();
        }
        inventory.connection.detach();
        state
            .draining
            .retain(|connection| connection.strong_count() != 0);
        state.draining.push(Arc::downgrade(&inventory.connection));
        state.readiness = ConnectionReadiness::Disconnected(inventory);
        // Expiry alone never opens a transport. Exhausted callers return their original error;
        // only another admitted acquire starts the independently bounded shared setup.
        slot.changed.bump();
        Ok(())
    }
}

impl ConnectionSlot {
    pub(super) fn open_lazy(self: &Arc<Self>, state: &mut AttemptState) {
        let ConnectionReadiness::Disconnected(inventory) = &state.readiness else {
            return;
        };
        inventory.connection.detach();
        state
            .draining
            .retain(|connection| connection.strong_count() != 0);
        if !state
            .draining
            .iter()
            .any(|old| old.ptr_eq(&Arc::downgrade(&inventory.connection)))
        {
            state.draining.push(Arc::downgrade(&inventory.connection));
        }
        if let Some(task) = state.refresh_task.take() {
            task.abort();
        }
        state.epoch += 1;
        let epoch = state.epoch;
        state.readiness = ConnectionReadiness::Connecting;
        let (abort, signal) = pi_core::AbortHandle::new();
        state.abort = Some(abort);
        state.task = Some(self.spawn_attempt(epoch, self.input.clone().connect(signal)));
        self.changed.bump();
    }
}
