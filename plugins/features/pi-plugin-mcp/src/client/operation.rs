//! In-process request control across the SDK's service and HTTP worker tasks.
//! Only SDK metadata is serialized; this typed extension is deliberately not wire metadata.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Weak};

use pi_core::AbortSignal;
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, GetExtensions};
use rmcp::transport::streamable_http_client::StreamableHttpError;

use crate::supervisor::WeakMcpGenerationReadiness;

mod http;
pub(crate) use http::DispatchHttpClient;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ConnectionMode {
    #[default]
    Explicit,
    Supervised,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OperationError {
    #[error("MCP request was cancelled")]
    Cancelled,
    #[error("MCP generation authority was retired")]
    AuthorityRetired,
    #[error("MCP transport was superseded before dispatch")]
    TransportSuperseded,
    #[error("MCP callable inventory changed; retry after the next idle boundary")]
    InventoryChanged,
    #[error("MCP connection is unavailable; retry after the next idle boundary")]
    ConnectionUnavailable,
}

impl OperationError {
    pub(crate) fn into_http<E: std::error::Error + Send + Sync + 'static>(
        self,
    ) -> StreamableHttpError<E> {
        StreamableHttpError::Io(std::io::Error::other(self))
    }
}

/// One immutable transport epoch. It can only close, never be rearmed for a replacement.
#[derive(Default)]
pub(crate) struct DispatchGate(AtomicU8);

impl DispatchGate {
    pub(crate) fn supersede(&self) {
        let _ = self
            .0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn retire(&self) {
        self.0.store(2, Ordering::Release);
    }

    fn check_completion(&self) -> Result<(), OperationError> {
        match self.0.load(Ordering::Acquire) {
            0 | 1 => Ok(()),
            _ => Err(OperationError::AuthorityRetired),
        }
    }

    pub(crate) fn check(&self) -> Result<(), OperationError> {
        match self.0.load(Ordering::Acquire) {
            0 => Ok(()),
            1 => Err(OperationError::TransportSuperseded),
            _ => Err(OperationError::AuthorityRetired),
        }
    }
}

#[derive(Clone)]
pub(crate) struct LogicalOperation(Arc<OperationState>);

struct OperationState {
    spent: AtomicBool,
    cancelled: AtomicBool,
    signal: AbortSignal,
    authority: Option<WeakMcpGenerationReadiness>,
    retirement: Option<AbortSignal>,
}

/// Only the application operation owns this guard; transport/auth clones cannot keep it alive.
pub(crate) struct OperationGuard(LogicalOperation);

impl OperationGuard {
    pub(crate) fn new(signal: AbortSignal, authority: Option<WeakMcpGenerationReadiness>) -> Self {
        let retirement = authority
            .as_ref()
            .and_then(WeakMcpGenerationReadiness::retirement_signal);
        Self(LogicalOperation(Arc::new(OperationState {
            retirement,
            spent: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            signal,
            authority,
        })))
    }

    pub(crate) fn operation(&self) -> LogicalOperation {
        self.0.clone()
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.0.cancelled.store(true, Ordering::Release);
    }
}

impl LogicalOperation {
    pub(crate) async fn wait_retired(&self) {
        match &self.0.retirement {
            Some(signal) => signal.wait().await,
            None => std::future::pending().await,
        }
    }

    pub(crate) fn check(&self) -> Result<(), OperationError> {
        if self.0.cancelled.load(Ordering::Acquire) || self.0.signal.is_aborted() {
            return Err(OperationError::Cancelled);
        }
        if self
            .0
            .authority
            .as_ref()
            .is_some_and(|authority| !authority.is_active())
        {
            return Err(OperationError::AuthorityRetired);
        }
        Ok(())
    }

    pub(crate) fn try_recover(&self) -> Result<bool, OperationError> {
        self.check()?;
        Ok(self
            .0
            .spent
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok())
    }
}

/// Every SDK request receives a fresh immutable attachment; pages and replacements share only
/// their logical operation. Neither a retained SDK clone nor a token refresh can rearm its gate.
#[derive(Clone)]
pub(crate) struct RequestControl {
    operation: LogicalOperation,
    gate: Weak<DispatchGate>,
}

impl RequestControl {
    pub(crate) fn new(operation: LogicalOperation, gate: &Arc<DispatchGate>) -> Self {
        Self {
            operation,
            gate: Arc::downgrade(gate),
        }
    }

    pub(crate) fn attach(&self, request: &mut ClientRequest) {
        request.extensions_mut().insert(self.clone());
    }

    pub(crate) fn from_message(message: &ClientJsonRpcMessage) -> Option<Self> {
        let ClientJsonRpcMessage::Request(request) = message else {
            return None;
        };
        request.request.extensions().get::<Self>().cloned()
    }

    pub(crate) fn check_dispatch(&self) -> Result<(), OperationError> {
        self.operation.check()?;
        self.gate
            .upgrade()
            .ok_or(OperationError::AuthorityRetired)?
            .check()
    }

    /// An admitted response may finish after ordinary expiry detached its transport.
    /// This never authorizes another dispatch or relaxes cancellation/authority retirement.
    pub(crate) fn check_completion(&self) -> Result<(), OperationError> {
        self.operation.check()?;
        self.gate
            .upgrade()
            .ok_or(OperationError::AuthorityRetired)?
            .check_completion()
    }

    pub(crate) fn try_recover(&self) -> Result<bool, OperationError> {
        self.check_dispatch()?;
        self.operation.try_recover()
    }
}

#[cfg(test)]
mod tests;
