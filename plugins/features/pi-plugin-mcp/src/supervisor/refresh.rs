//! Coalesced notification discovery. Only a raw slot owns the worker, never the reverse.
use super::*;
use tokio::sync::watch;

const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

impl ConnectionSlot {
    pub(super) fn spawn_refresh(
        self: &Arc<Self>,
        epoch: u64,
        mut seen: u64,
        mut notifications: watch::Receiver<u64>,
    ) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let revision = *notifications.borrow_and_update();
                if revision == seen {
                    if notifications.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
                // Drop the upgraded slot before any network await. Retained executable views
                // may own the raw connection, but cannot prevent slot cancellation/retirement.
                let Some(connection) = weak
                    .upgrade()
                    .and_then(|slot| slot.refresh_connection(epoch))
                else {
                    return;
                };
                let result =
                    std::panic::AssertUnwindSafe(connection.refresh_tools(REFRESH_TIMEOUT))
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| {
                            Err(McpError::ListTools {
                        server: connection.name().into(),
                        message:
                            "tool inventory refresh worker failed; last discovered tools retained"
                                .into(),
                    })
                        });
                let current = weak.upgrade().is_some_and(|slot| {
                    slot.complete_refresh(epoch, &connection, revision, result)
                });
                if !current {
                    return;
                }
                // Consume only the pre-request revision. Any burst arriving during the entire
                // paginated request needs one follow-up, even if this request failed.
                seen = revision;
            }
        })
    }

    fn refresh_connection(&self, epoch: u64) -> Option<Arc<McpConnection>> {
        self.observe_closure();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired || state.epoch != epoch {
            return None;
        }
        match &state.readiness {
            ConnectionReadiness::Ready(inventory) => Some(inventory.connection.clone()),
            _ => None,
        }
    }

    pub(super) fn complete_refresh(
        &self,
        epoch: u64,
        connection: &Arc<McpConnection>,
        revision: u64,
        result: Result<Vec<RemoteTool>, McpError>,
    ) -> bool {
        self.observe_closure();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired || state.epoch != epoch {
            return false;
        }
        let ConnectionReadiness::Ready(previous) = &state.readiness else {
            return false;
        };
        if !Arc::ptr_eq(&previous.connection, connection) {
            return false;
        }
        let (tools, error) = match result {
            Ok(tools) => (tools, None),
            Err(error) => (previous.tools.clone(), Some(Arc::new(error))),
        };
        let changed = !same_tools(&previous.tools, &tools)
            || previous.refresh_error.as_ref().map(ToString::to_string)
                != error.as_ref().map(ToString::to_string);
        if changed {
            state.readiness = ConnectionReadiness::Ready(Arc::new(ConnectionInventory {
                connection: connection.clone(),
                tools,
                refresh_error: error,
            }));
        }
        state.refresh_seen = revision;
        drop(state);
        // Do not trigger idle publication for an equivalent inventory or repeated diagnostic.
        if changed {
            self.changed.bump();
        }
        true
    }
}

fn same_tools(left: &[RemoteTool], right: &[RemoteTool]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut left = left.iter().collect::<Vec<_>>();
    let mut right = right.iter().collect::<Vec<_>>();
    left.sort_by(|a, b| a.name.cmp(&b.name));
    right.sort_by(|a, b| a.name.cmp(&b.name));
    left == right
}
