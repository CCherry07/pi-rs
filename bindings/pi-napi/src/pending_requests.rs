//! Cancellation stays in the Node transport, outside native plugin contracts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

type Requests = Arc<Mutex<HashMap<String, Pending>>>;

struct Pending {
    _cancel: oneshot::Sender<()>,
    receiver: Option<oneshot::Receiver<()>>,
}

#[derive(Default)]
pub(crate) struct PendingRequests {
    next: AtomicU64,
    requests: Requests,
}

impl PendingRequests {
    pub(crate) fn prepare(&self) -> Result<String, &'static str> {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if requests.len() >= 256 {
            return Err("too many pending extension requests");
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed).to_string();
        let (cancel, receiver) = oneshot::channel();
        requests.insert(
            id.clone(),
            Pending {
                _cancel: cancel,
                receiver: Some(receiver),
            },
        );
        Ok(id)
    }

    pub(crate) fn cancel(&self, id: &str) {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }

    pub(crate) fn take(&self, id: &str) -> Option<RequestLease> {
        let receiver = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(id)?
            .receiver
            .take()?;
        Some(RequestLease {
            id: id.to_owned(),
            requests: self.requests.clone(),
            receiver,
        })
    }
}

pub(crate) struct RequestLease {
    id: String,
    requests: Requests,
    receiver: oneshot::Receiver<()>,
}

impl RequestLease {
    pub(crate) async fn cancelled(&mut self) {
        let _ = (&mut self.receiver).await;
    }
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_before_and_after_dispatch_releases_requests() {
        let pending = PendingRequests::default();
        let before = pending.prepare().unwrap();
        pending.cancel(&before);
        assert!(pending.take(&before).is_none());
        let after = pending.prepare().unwrap();
        let mut lease = pending.take(&after).unwrap();
        pending.cancel(&after);
        lease.cancelled().await;
        assert!(pending.take(&after).is_none());
    }

    #[test]
    fn completion_releases_capacity_and_late_cancel_does_not_create_a_request() {
        let pending = PendingRequests::default();
        for _ in 0..300 {
            let id = pending.prepare().unwrap();
            drop(pending.take(&id).unwrap());
            pending.cancel(&id);
        }
        assert!(pending.requests.lock().unwrap().is_empty());
    }
}
