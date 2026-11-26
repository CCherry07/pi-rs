//! Admitted raw requests keep detached transports alive, never their slot or generation owner.
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::McpConnection;
use super::operation::OperationError;

pub(crate) struct ConnectionUse {
    pub(crate) connection: Arc<McpConnection>,
}

impl McpConnection {
    pub(crate) fn admit(self: &Arc<Self>) -> Result<ConnectionUse, OperationError> {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        let admitted = ConnectionUse {
            connection: self.clone(),
        };
        self.dispatch.check()?;
        if self.is_closed() {
            return Err(OperationError::TransportSuperseded);
        }
        Ok(admitted)
    }

    /// Stop new dispatch/discovery but preserve already admitted JSON POST and SSE responses.
    pub(crate) fn detach(&self) {
        self.dispatch.supersede();
        self.draining.store(true, Ordering::Release);
        self.discovery.cancel();
        if self.active_requests.load(Ordering::Acquire) == 0 {
            self.cancel();
        }
    }
}

impl Drop for ConnectionUse {
    fn drop(&mut self) {
        if self
            .connection
            .active_requests
            .fetch_sub(1, Ordering::AcqRel)
            == 1
            && self.connection.draining.load(Ordering::Acquire)
        {
            self.connection.cancel();
        }
    }
}
