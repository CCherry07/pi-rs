//! Weak exact-slot routing and lazy transport replacement. No executable policy lives here.
use super::*;
use crate::client::ConnectionUse;
use crate::client::operation::OperationError;

#[derive(Clone)]
pub(crate) struct WeakConnectionSlot(pub(super) Weak<ConnectionSlot>);

/// Explicit pools own their exact raw slot directly; generation views keep using weak pins.
/// This reuses the existing slot/attempt lifecycle, not a second reconnect coordinator.
#[derive(Clone)]
pub(crate) struct OwnedConnectionSlot(Arc<ConnectionSlot>);

impl OwnedConnectionSlot {
    pub(crate) fn ready(
        input: ConnectionInput,
        connection: Arc<McpConnection>,
        tools: Vec<rmcp::model::Tool>,
    ) -> Self {
        // Preserve the former explicit recovery setup bound; do not inherit the broader
        // supervised opening budget merely because the attempt owner is shared.
        let input = input.with_attempt_timeout(std::time::Duration::from_secs(5));
        let slot = ConnectionSlot::new(input, Arc::new(ReadinessRevision::default()));
        slot.complete(0, Ok((connection, tools)));
        Self(slot)
    }

    #[cfg(test)]
    pub(crate) fn readiness(&self) -> ConnectionReadiness {
        self.0.readiness()
    }

    pub(crate) fn downgrade(&self) -> WeakConnectionSlot {
        WeakConnectionSlot(Arc::downgrade(&self.0))
    }

    pub(crate) async fn acquire(&self) -> Result<ConnectionAdmission, AdmissionError> {
        self.downgrade()
            .acquire_checked(|state, inventory| {
                // Only a definitive expiry placed this exact transport in the draining list.
                // A plain closed explicit service must retain its original diagnostic, not
                // silently acquire the supervised path's broader lazy-reconnect policy.
                let old = Arc::downgrade(&inventory.connection);
                if state
                    .draining
                    .iter()
                    .any(|connection| connection.ptr_eq(&old))
                {
                    Ok(())
                } else {
                    Err(AdmissionError::Closed(inventory.connection.clone()))
                }
            })
            .await
    }

    pub(crate) async fn close(&self) -> Result<(), McpError> {
        // Keep both current and draining services until their bounded shutdowns settle.
        let connections = self.0.retire_connections();
        futures::future::join_all(connections.iter().map(|connection| connection.close()))
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map(|_| ())
    }
}

pub(crate) enum AdmissionError {
    Control(OperationError),
    Closed(Arc<McpConnection>),
}

impl From<OperationError> for AdmissionError {
    fn from(error: OperationError) -> Self {
        Self::Control(error)
    }
}

pub(crate) struct ConnectionAdmission {
    pub(crate) inventory: Arc<ConnectionInventory>,
    pub(crate) _usage: ConnectionUse,
}

impl WeakConnectionSlot {
    pub(crate) fn readiness(&self) -> ConnectionReadiness {
        self.0
            .upgrade()
            .map_or(ConnectionReadiness::Retired, |slot| slot.readiness())
    }

    /// The waiter never holds a slot while suspended. Cancelling one waiter cannot cancel the
    /// shared opening; dropping the actual generation/owner still retires every attempt.
    pub(crate) async fn acquire(&self) -> Result<ConnectionAdmission, OperationError> {
        self.acquire_checked(|_, _| Ok(())).await
    }

    async fn acquire_checked<E: From<OperationError>>(
        &self,
        disconnected: impl Fn(&AttemptState, &Arc<ConnectionInventory>) -> Result<(), E>,
    ) -> Result<ConnectionAdmission, E> {
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
                        let usage = match inventory.connection.admit() {
                            Ok(usage) => usage,
                            Err(error) => {
                                disconnected(&state, inventory)?;
                                return Err(error.into());
                            }
                        };
                        return Ok(ConnectionAdmission {
                            inventory: inventory.clone(),
                            _usage: usage,
                        });
                    }
                    ConnectionReadiness::Disconnected(inventory) => {
                        disconnected(&state, inventory)?;
                        slot.open_lazy(&mut state);
                    }
                    ConnectionReadiness::Connecting => {}
                    ConnectionReadiness::Retired => {
                        return Err(OperationError::AuthorityRetired.into());
                    }
                    ConnectionReadiness::Failed(_) => {
                        return Err(OperationError::ConnectionUnavailable.into());
                    }
                }
            }
            drop(slot);
            changed.await;
        }
    }

    /// Authentication is reported against the admitted transport, never the mutable name map.
    /// New calls stop immediately; unrelated admitted responses drain before terminal cleanup.
    pub(crate) fn require_authorization(
        &self,
        expected: &Arc<McpConnection>,
        required: crate::McpAuthorizationRequired,
    ) -> Result<(), OperationError> {
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
            // A late failure cannot overwrite a replacement or its in-progress setup.
            _ => return Ok(()),
        };
        state.epoch += 1;
        if let Some(task) = state.refresh_task.take() {
            task.abort();
        }
        inventory.connection.detach();
        state
            .draining
            .retain(|connection| connection.strong_count() != 0);
        let old = Arc::downgrade(&inventory.connection);
        if !state
            .draining
            .iter()
            .any(|connection| connection.ptr_eq(&old))
        {
            state.draining.push(old);
        }
        state.readiness =
            ConnectionReadiness::Failed(Arc::new(McpError::AuthorizationRequired(required)));
        slot.changed.bump();
        Ok(())
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
